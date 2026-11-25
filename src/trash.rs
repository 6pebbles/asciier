//! The freedesktop.org trash (spec 1.0): move a file there, put the last one back.

use std::ffi::{CString, OsString};
use std::fmt::Write as _;
use std::fs;
use std::io::{self, Write as _};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// A trashed file: where it was, where it is, its .trashinfo, its (device, inode).
#[derive(Clone, PartialEq, Debug)]
pub struct Trashed {
    pub path: PathBuf,
    pub stored: PathBuf,
    pub info: PathBuf,
    pub ident: (u64, u64),
}

/// Why a trashed file did not go back.
#[derive(Debug)]
pub enum Refused {
    /// Something is at the old path now: nothing changed.
    Exists,
    /// The trash no longer holds the file trashed (gone, another file under the name, or its .trashinfo missing).
    Gone,
    /// Any other failure of the file system.
    Io,
}

/// Move the file at `path` to the trash without copying (rows T1-T4 of the inventory).
pub fn trash(path: &Path) -> std::io::Result<Trashed> {
    let home = home_trash(std::env::var_os("XDG_DATA_HOME"), std::env::home_dir())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no home directory"))?;
    trash_from(path, &home)
}

/// The home trash: `data` (XDG_DATA_HOME) when it is an absolute path, else .local/share in `home`.
fn home_trash(data: Option<OsString>, home: Option<PathBuf>) -> Option<PathBuf> {
    let data = match data.map(PathBuf::from) {
        Some(data) if data.is_absolute() => data,
        _ => home?.join(".local/share"),
    };
    Some(data.join("Trash"))
}

/// `trash` with the home trash at `home`: taken when it is on the file's device, else the trash of the file's mount point.
fn trash_from(path: &Path, home: &Path) -> io::Result<Trashed> {
    let path = entry(path)?;
    let top = if same_device(&path, home)? {
        None
    } else {
        path.parent().map(mount_point)
    };
    trash_at(&path, home, top.as_deref(), unsafe { libc::getuid() })
}

/// `path` with its folder resolved and its own name kept: a symlink itself is what moves.
fn entry(path: &Path) -> io::Result<PathBuf> {
    let name = path.file_name().ok_or(io::ErrorKind::InvalidInput)?;
    let folder = match path.parent() {
        Some(folder) if !folder.as_os_str().is_empty() => folder,
        _ => Path::new("."),
    };
    Ok(folder.canonicalize()?.join(name))
}

/// Whether the file at `path` shares a device with `trash`, a directory that may not exist yet: its nearest existing parent counts.
fn same_device(path: &Path, trash: &Path) -> io::Result<bool> {
    let mut near = trash;
    while near.symlink_metadata().is_err() {
        near = near.parent().ok_or(io::ErrorKind::NotFound)?;
    }
    Ok(path.symlink_metadata()?.dev() == near.metadata()?.dev())
}

/// The mount point of the file system holding `folder` (resolved): the nearest folder upward on another device than its parent, or the root.
fn mount_point(folder: &Path) -> PathBuf {
    let mut at = folder;
    while let Some(above) = at.parent() {
        match (at.symlink_metadata(), above.symlink_metadata()) {
            (Ok(here), Ok(up)) if here.dev() != up.dev() || here.ino() == up.ino() => break,
            _ => at = above,
        }
    }
    at.to_path_buf()
}

/// Move the file at `path` (as `entry` gives it) to the home trash at `home`, or, with `top` its mount point on another device, to that mount's trash for user `uid`: the shared $top/.Trash/$uid if .Trash is a sticky directory and no symlink, then $top/.Trash-$uid.
/// The .trashinfo is made first, exclusively: it reserves a name free in the trash (name, name.2, name.3 ... before the extension) and goes again if the move fails. Its Path is absolute in the home trash, else relative to `top`.
fn trash_at(path: &Path, home: &Path, top: Option<&Path>, uid: u32) -> io::Result<Trashed> {
    let folders = match top {
        None => vec![home.to_path_buf()],
        Some(top) => {
            let shared = top.join(".Trash");
            let sticky = shared
                .symlink_metadata()
                .is_ok_and(|meta| meta.is_dir() && meta.mode() & libc::S_ISVTX != 0);
            let own = top.join(format!(".Trash-{uid}"));
            match sticky {
                true => vec![shared.join(uid.to_string()), own],
                false => vec![own],
            }
        }
    };
    // the first that can be made ready, else the last one's error
    let none = Err(io::ErrorKind::NotFound.into());
    let (files, infos) = folders.iter().fold(none, |ready, folder| {
        ready.or_else(|_| trash_ready(folder, top.is_some(), uid))
    })?;
    let listed = match top {
        Some(top) => path
            .strip_prefix(top)
            .map_err(|_| io::ErrorKind::InvalidInput)?,
        None => path,
    };
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    let text = format!(
        "[Trash Info]\nPath={}\nDeletionDate={}\n",
        quote(listed.as_os_str().as_bytes()),
        stamp(now)
    );
    let name = path.file_name().ok_or(io::ErrorKind::InvalidInput)?;
    let mut n = 0;
    loop {
        n += 1;
        let mut name = OsString::from_vec(numbered(name.as_bytes(), n));
        let stored = files.join(&name);
        name.push(".trashinfo");
        let info = infos.join(name);
        let made = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&info);
        let mut record = match made {
            Ok(record) => record,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        // a rename keeps the inode: the file's own is the stored one's
        let moved = record
            .write_all(text.as_bytes())
            .and_then(|()| path.symlink_metadata())
            .and_then(|meta| rename_new(path, &stored).map(|()| meta));
        match moved {
            Ok(meta) => {
                return Ok(Trashed {
                    path: path.to_path_buf(),
                    stored,
                    info,
                    ident: (meta.dev(), meta.ino()),
                });
            }
            Err(error) => {
                fs::remove_file(info)?;
                // a file the trash holds without a .trashinfo
                if error.kind() != io::ErrorKind::AlreadyExists {
                    return Err(error);
                }
            }
        }
    }
}

/// Move a file only if the destination name is still free when the kernel moves it.
fn rename_new(from: &Path, to: &Path) -> io::Result<()> {
    let from = CString::new(from.as_os_str().as_bytes())?;
    let to = CString::new(to.as_os_str().as_bytes())?;
    // Both C strings live through the syscall. Unsupported filesystems leave the source in place.
    let status = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if status == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Make the trash directory `folder` with its files/ and info/ and return those two.
/// Each must be a directory of user `uid`; in a mount's trash (`shared`: others may write to the mount) also no symlink, or another user could have set it up to read or remove what lands in it.
fn trash_ready(folder: &Path, shared: bool, uid: u32) -> io::Result<(PathBuf, PathBuf)> {
    let make = |dir: &Path| -> io::Result<()> {
        match fs::DirBuilder::new().mode(0o700).create(dir) {
            Err(error) if error.kind() != io::ErrorKind::AlreadyExists || !dir.is_dir() => {
                return Err(error);
            }
            _ => {}
        }
        let meta = match shared {
            true => dir.symlink_metadata()?,
            false => dir.metadata()?,
        };
        if !meta.is_dir() || meta.uid() != uid {
            let whose = format!("{} is not a directory of this user", dir.display());
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, whose));
        }
        Ok(())
    };
    if let Some(above) = folder.parent() {
        fs::create_dir_all(above)?;
    }
    // checked before anything is made inside it
    make(folder)?;
    let (files, info) = (folder.join("files"), folder.join("info"));
    make(&files)?;
    make(&info)?;
    Ok((files, info))
}

/// The `n`th name tried for a file in the trash: its own, then `n` before the extension, which follows the last dot unless only dots come before it.
fn numbered(name: &[u8], n: u32) -> Vec<u8> {
    if n == 1 {
        return name.to_vec();
    }
    let dot = match name.iter().rposition(|&byte| byte == b'.') {
        Some(dot) if name[..dot].iter().any(|&byte| byte != b'.') => dot,
        _ => name.len(),
    };
    [&name[..dot], b".", n.to_string().as_bytes(), &name[dot..]].concat()
}

/// Percent-encode a path's bytes; letters, digits and `_.-~/` stay.
fn quote(path: &[u8]) -> String {
    let mut text = String::new();
    for &byte in path {
        if byte.is_ascii_alphanumeric() || b"_.-~/".contains(&byte) {
            text.push(byte as char);
        } else {
            let _ = write!(text, "%{byte:02X}");
        }
    }
    text
}

/// `seconds` since the epoch as a DeletionDate: local time, YYYY-MM-DDTHH:MM:SS.
fn stamp(seconds: libc::time_t) -> String {
    let mut time: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&seconds, &mut time) };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        time.tm_year + 1900,
        time.tm_mon + 1,
        time.tm_mday,
        time.tm_hour,
        time.tm_min,
        time.tm_sec
    )
}

/// Put a trashed file back where it was and drop its .trashinfo (row T7): a hard link first, so nothing is replaced.
pub fn untrash(entry: &Trashed) -> Result<(), Refused> {
    untrash_by(entry, |stored, path| fs::hard_link(stored, path))
}

/// `untrash` with the step that links the stored file to its old path: where `link` cannot (no hard links there), an exclusive rename moves the file.
fn untrash_by(
    entry: &Trashed,
    link: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> Result<(), Refused> {
    let entry_error = |error: io::Error| match error.kind() {
        io::ErrorKind::NotFound => Refused::Gone,
        _ => Refused::Io,
    };
    let stored = entry.stored.symlink_metadata().map_err(entry_error)?;
    // another file under the name, or one without its record: someone else's
    if (stored.dev(), stored.ino()) != entry.ident {
        return Err(Refused::Gone);
    }
    entry.info.symlink_metadata().map_err(entry_error)?;
    match link(&entry.stored, &entry.path) {
        Ok(()) => fs::remove_file(&entry.stored).map_err(|_| Refused::Io)?,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Err(Refused::Exists),
        Err(_) => {
            rename_new(&entry.stored, &entry.path).map_err(|error| match error.kind() {
                io::ErrorKind::AlreadyExists => Refused::Exists,
                _ => Refused::Io,
            })?;
        }
    }
    match fs::remove_file(&entry.info) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(Refused::Io),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::Permissions;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::process::Command;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A temp dir with the trash's data home inside it, so files and trash share a device; the real trash is never touched.
    struct Temp {
        dir: PathBuf,
    }

    impl Temp {
        fn new() -> Temp {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let name = format!(
                "asciier-trash-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            );
            let dir = std::env::temp_dir().join(name);
            fs::create_dir(&dir).unwrap();
            Temp {
                dir: dir.canonicalize().unwrap(),
            }
        }

        fn home(&self) -> PathBuf {
            self.dir.join("data/Trash")
        }

        /// A file of `bytes` in the folder `sub`.
        fn file(&self, sub: &str, name: &str, bytes: &[u8]) -> PathBuf {
            fs::create_dir_all(self.dir.join(sub)).unwrap();
            let file = self.dir.join(sub).join(name);
            fs::write(&file, bytes).unwrap();
            file
        }

        /// The names in the home trash's `sub`, sorted; none when it is not there.
        fn listing(&self, sub: &str) -> Vec<String> {
            let mut names: Vec<String> = fs::read_dir(self.home().join(sub))
                .into_iter()
                .flatten()
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .collect();
            names.sort();
            names
        }

        /// `trash` with this data home.
        fn trash(&self, path: &Path) -> io::Result<Trashed> {
            trash_from(path, &self.home())
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            // folders a test closed are opened again
            fn open(dir: &Path) {
                let _ = fs::set_permissions(dir, Permissions::from_mode(0o700));
                for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
                    if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                        open(&entry.path());
                    }
                }
            }
            open(&self.dir);
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn uid() -> u32 {
        unsafe { libc::getuid() }
    }

    fn mode(path: &Path) -> u32 {
        fs::symlink_metadata(path).unwrap().mode() & 0o7777
    }

    fn info(entry: &Trashed) -> String {
        fs::read_to_string(&entry.info).unwrap()
    }

    fn empty(dir: &Path) -> bool {
        fs::read_dir(dir).unwrap().next().is_none()
    }

    // TrashTest of tests/test_viewer.py:1347 at tag python-final.

    #[test]
    fn trash_stores_file_and_info() {
        let temp = Temp::new();
        let src = temp.file("my pics", "a b#é.png", b"pixels");
        let entry = temp.trash(&src).unwrap();
        assert!(!src.exists());
        assert_eq!(entry.path, src);
        assert_eq!(entry.stored, temp.home().join("files/a b#é.png"));
        assert_eq!(fs::read(&entry.stored).unwrap(), b"pixels");
        assert_eq!(entry.info, temp.home().join("info/a b#é.png.trashinfo"));
        let stored = fs::symlink_metadata(&entry.stored).unwrap();
        assert_eq!(entry.ident, (stored.dev(), stored.ino()));
        let text = info(&entry);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines.len() == 3 && text.ends_with('\n'));
        assert_eq!(lines[0], "[Trash Info]");
        let folder = temp.dir.to_str().unwrap().replace(' ', "%20");
        assert_eq!(
            lines[1],
            format!("Path={folder}/my%20pics/a%20b%23%C3%A9.png")
        );
        // DeletionDate=YYYY-MM-DDTHH:MM:SS
        let date = lines[2].strip_prefix("DeletionDate=").unwrap().as_bytes();
        assert_eq!(date.len(), 19);
        for (at, byte) in date.iter().enumerate() {
            match at {
                4 | 7 => assert_eq!(*byte, b'-'),
                10 => assert_eq!(*byte, b'T'),
                13 | 16 => assert_eq!(*byte, b':'),
                _ => assert!(byte.is_ascii_digit()),
            }
        }
        // row T2: the three directories for this user alone, the parent as any other; row T3: the record too
        for dir in ["", "files", "info"] {
            assert_eq!(mode(&temp.home().join(dir)), 0o700);
        }
        let status = fs::read_to_string("/proc/self/status").unwrap();
        let umask = status
            .lines()
            .find_map(|line| line.strip_prefix("Umask:"))
            .unwrap();
        let umask = u32::from_str_radix(umask.trim(), 8).unwrap();
        assert_eq!(mode(&temp.dir.join("data")), 0o777 & !umask);
        assert_eq!(mode(&entry.info), 0o600);
    }

    #[test]
    fn unique_names() {
        let temp = Temp::new();
        // the same name from two folders, twice over
        let stored: Vec<Trashed> = ["one", "two", "one", "two"]
            .iter()
            .map(|sub| {
                temp.trash(&temp.file(sub, "x.png", sub.as_bytes()))
                    .unwrap()
            })
            .collect();
        let names: Vec<_> = stored
            .iter()
            .map(|entry| entry.stored.file_name().unwrap())
            .collect();
        assert_eq!(names, ["x.png", "x.2.png", "x.3.png", "x.4.png"]);
        let held: Vec<_> = stored
            .iter()
            .map(|entry| fs::read(&entry.stored).unwrap())
            .collect();
        assert_eq!(held, [b"one", b"two", b"one", b"two"]);
        assert_eq!(
            temp.listing("info"),
            [
                "x.2.png.trashinfo",
                "x.3.png.trashinfo",
                "x.4.png.trashinfo",
                "x.png.trashinfo"
            ]
        );
        let second = format!("Path={}\n", temp.dir.join("two/x.png").display());
        assert!(info(&stored[1]).contains(&second));
    }

    #[test]
    fn name_taken_by_a_file_without_info() {
        let temp = Temp::new();
        fs::create_dir_all(temp.home().join("files")).unwrap();
        fs::write(temp.home().join("files/x.png"), b"orphan").unwrap();
        let entry = temp.trash(&temp.file("", "x.png", b"new")).unwrap();
        assert_eq!(entry.stored.file_name().unwrap(), "x.2.png");
        assert_eq!(
            fs::read(temp.home().join("files/x.png")).unwrap(),
            b"orphan"
        );
        assert_eq!(temp.listing("info"), ["x.2.png.trashinfo"]);
    }

    #[test]
    fn the_move_refuses_a_name_taken_after_its_selection() {
        let temp = Temp::new();
        let src = temp.file("pics", "a.png", b"original");
        let dest = temp.file("files", "a.png", b"arrived later");
        let error = rename_new(&src, &dest).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(src).unwrap(), b"original");
        assert_eq!(fs::read(dest).unwrap(), b"arrived later");
    }

    #[test]
    fn failed_move_leaves_the_file_and_no_info() {
        let temp = Temp::new();
        let src = temp.file("pics", "a.png", b"a");
        // a folder that lets nothing out fails the rename the Python test faked; not for root
        fs::set_permissions(temp.dir.join("pics"), Permissions::from_mode(0o555)).unwrap();
        if uid() != 0 {
            let error = temp.trash(&src).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            assert!(src.exists() && temp.home().join("info").is_dir());
            assert!(temp.listing("files").is_empty() && temp.listing("info").is_empty());
        }
        // a file that is not there is no trash either
        assert!(temp.trash(&temp.dir.join("pics/nope.png")).is_err());
        assert!(temp.listing("info").is_empty());
    }

    #[test]
    fn undo_restores_bytes_and_removes_info() {
        let temp = Temp::new();
        let src = temp.file("pics", "a.png", b"bytes");
        let entry = temp.trash(&src).unwrap();
        untrash(&entry).unwrap();
        assert_eq!(fs::read(&src).unwrap(), b"bytes");
        assert!(temp.listing("files").is_empty() && temp.listing("info").is_empty());
    }

    #[test]
    fn undo_does_not_overwrite() {
        let temp = Temp::new();
        let src = temp.file("pics", "a.png", b"a");
        let entry = temp.trash(&src).unwrap();
        fs::write(&src, b"newer").unwrap();
        assert!(matches!(untrash(&entry), Err(Refused::Exists)));
        assert_eq!(fs::read(&src).unwrap(), b"newer");
        assert!(entry.stored.exists() && entry.info.exists());
    }

    #[test]
    fn undo_refuses_a_slot_reused_by_another_file() {
        let temp = Temp::new();
        let first = temp.trash(&temp.file("e", "x.png", b"from e")).unwrap();
        // what a file manager's restore does
        fs::rename(&first.stored, temp.dir.join("restored-elsewhere.png")).unwrap();
        fs::remove_file(&first.info).unwrap();
        let second = temp.trash(&temp.file("f", "x.png", b"from f")).unwrap();
        // the name was free again
        assert_eq!(second.stored, first.stored);
        assert!(matches!(untrash(&first), Err(Refused::Gone)));
        assert!(!temp.dir.join("e/x.png").exists());
        assert_eq!(fs::read(&second.stored).unwrap(), b"from f");
        assert!(second.info.exists());
        untrash(&second).unwrap();
        assert_eq!(fs::read(temp.dir.join("f/x.png")).unwrap(), b"from f");
    }

    #[test]
    fn undo_when_the_trash_was_emptied_or_lost_its_info() {
        let temp = Temp::new();
        let (a, b) = (
            temp.file("pics", "a.png", b"a"),
            temp.file("pics", "b.png", b"b"),
        );
        let (gone, bare) = (temp.trash(&a).unwrap(), temp.trash(&b).unwrap());
        fs::remove_file(&gone.stored).unwrap();
        fs::remove_file(&gone.info).unwrap();
        fs::remove_file(&bare.info).unwrap();
        for entry in [&gone, &bare] {
            assert!(matches!(untrash(entry), Err(Refused::Gone)));
        }
        assert!(!a.exists() && !b.exists());
        // left alone
        assert!(bare.stored.exists());
    }

    #[test]
    fn undo_without_hard_links_still_checks() {
        let temp = Temp::new();
        let src = temp.file("pics", "a.png", b"a");
        let entry = temp.trash(&src).unwrap();
        let no_links = |_: &Path, _: &Path| Err(io::ErrorKind::PermissionDenied.into());
        fs::write(&src, b"newer").unwrap();
        assert!(matches!(untrash_by(&entry, no_links), Err(Refused::Exists)));
        assert_eq!(fs::read(&src).unwrap(), b"newer");
        assert_eq!(fs::read(&entry.stored).unwrap(), b"a");
        assert!(entry.info.is_file());
        fs::remove_file(&src).unwrap();
        untrash_by(&entry, no_links).unwrap();
        assert_eq!(fs::read(&src).unwrap(), b"a");
        assert!(!entry.stored.exists() && !entry.info.exists());
    }

    #[test]
    fn device_and_mount_lookups() {
        let temp = Temp::new();
        let device = |path: &Path| fs::metadata(path).unwrap().dev();
        // the trash does not exist yet: its first existing parent counts
        assert!(same_device(&temp.dir, &temp.home()).unwrap());
        let mount = mount_point(&temp.dir);
        assert_eq!(device(&mount), device(&temp.dir));
        // coreutils names the same mount point
        let named = Command::new("stat")
            .args(["-c", "%m"])
            .arg(&temp.dir)
            .output()
            .unwrap();
        assert_eq!(mount.as_os_str().as_bytes(), named.stdout.trim_ascii_end());
        assert_eq!(mount_point(Path::new("/")), Path::new("/"));
        let shm = Path::new("/dev/shm");
        if shm.is_dir() && device(shm) != device(&temp.dir) {
            assert!(!same_device(shm, &temp.home()).unwrap());
        }
    }

    #[test]
    fn symlink_moves_itself() {
        let temp = Temp::new();
        let target = temp.file("", "real.png", b"real");
        let link = temp.dir.join("link.png");
        symlink(&target, &link).unwrap();
        let entry = temp.trash(&link).unwrap();
        assert!(target.exists() && entry.stored.is_symlink());
        untrash(&entry).unwrap();
        assert!(link.is_symlink());
        assert_eq!(fs::read_link(&link).unwrap(), target);
    }

    #[test]
    fn a_linked_home_trash_is_followed() {
        // bin/ascii-viewer:335: the home trash and its directories may be links (to another disk); only a mount's trash must be none
        let temp = Temp::new();
        let (disk, files) = (temp.dir.join("disk"), temp.dir.join("disk-files"));
        for dir in [&disk, &files, &temp.dir.join("data")] {
            fs::create_dir(dir).unwrap();
        }
        symlink(&disk, temp.home()).unwrap();
        symlink(&files, disk.join("files")).unwrap();
        let src = temp.file("pics", "a.png", b"a");
        let entry = temp.trash(&src).unwrap();
        assert_eq!(entry.stored, temp.home().join("files/a.png"));
        assert_eq!(fs::read(files.join("a.png")).unwrap(), b"a");
        untrash(&entry).unwrap();
        assert!(src.exists());
    }

    // TopdirTrashTest of tests/test_viewer.py:1475 at tag python-final.

    /// A file on another device than the home trash: faked, with a folder standing for its mount.
    struct Mount {
        temp: Temp,
        top: PathBuf,
        src: PathBuf,
        /// The trash directory of this user alone on the mount.
        own: PathBuf,
    }

    impl Mount {
        fn new() -> Mount {
            let temp = Temp::new();
            Mount {
                top: temp.dir.join("mnt"),
                src: temp.file("mnt/sub", "f č.png", b"x"),
                own: temp.dir.join(format!("mnt/.Trash-{}", uid())),
                temp,
            }
        }

        /// The mount's shared trash directory, made with `mode`.
        fn shared(&self, mode: u32) -> PathBuf {
            let shared = self.top.join(".Trash");
            fs::create_dir(&shared).unwrap();
            fs::set_permissions(&shared, Permissions::from_mode(mode)).unwrap();
            shared
        }

        /// A directory outside the mount, for links to point at.
        fn elsewhere(&self) -> PathBuf {
            let real = self.temp.dir.join("elsewhere");
            fs::create_dir(&real).unwrap();
            real
        }

        fn trash(&self) -> io::Result<Trashed> {
            trash_at(&self.src, &self.temp.home(), Some(&self.top), uid())
        }

        /// Trash the file and find it in `folder`, its path relative to the mount, the home trash unused.
        fn check(&self, folder: &Path) -> Trashed {
            let entry = self.trash().unwrap();
            assert_eq!(entry.stored, folder.join("files/f č.png"));
            assert_eq!(fs::read(&entry.stored).unwrap(), b"x");
            assert_eq!(info(&entry).lines().nth(1), Some("Path=sub/f%20%C4%8D.png"));
            assert!(self.temp.listing("files").is_empty());
            entry
        }
    }

    #[test]
    fn dot_trash_uid_when_sticky() {
        let mount = Mount::new();
        let folder = mount.shared(0o1777).join(uid().to_string());
        let entry = mount.check(&folder);
        assert_eq!(mode(&folder), 0o700);
        untrash(&entry).unwrap();
        assert!(mount.src.exists());
    }

    #[test]
    fn dot_trash_dash_uid_without_dot_trash() {
        let mount = Mount::new();
        mount.check(&mount.own);
        assert_eq!(mode(&mount.own), 0o700);
    }

    #[test]
    fn dot_trash_not_sticky_is_passed_over() {
        let mount = Mount::new();
        let shared = mount.shared(0o777);
        mount.check(&mount.own);
        assert!(empty(&shared));
    }

    #[test]
    fn dot_trash_symlink_is_passed_over() {
        let mount = Mount::new();
        let real = mount.elsewhere();
        fs::set_permissions(&real, Permissions::from_mode(0o1777)).unwrap();
        symlink(&real, mount.top.join(".Trash")).unwrap();
        mount.check(&mount.own);
        assert!(empty(&real));
    }

    #[test]
    fn dot_trash_uid_that_cannot_be_made_falls_back() {
        // sticky, but not writable: no directory of the user can be made in it; root can make it
        if uid() != 0 {
            let mount = Mount::new();
            mount.shared(0o1555);
            mount.check(&mount.own);
        }
    }

    #[test]
    fn a_symlinked_uid_directory_is_refused() {
        let mount = Mount::new();
        let real = mount.elsewhere();
        symlink(&real, mount.shared(0o1777).join(uid().to_string())).unwrap();
        // the next candidate
        mount.check(&mount.own);
        assert!(empty(&real));
    }

    #[test]
    fn a_symlinked_dash_uid_directory_is_refused() {
        let mount = Mount::new();
        let real = mount.elsewhere();
        symlink(&real, &mount.own).unwrap();
        assert!(mount.trash().is_err());
        assert!(empty(&real));
        assert_eq!(fs::read(&mount.src).unwrap(), b"x");
    }

    #[test]
    fn a_symlinked_files_directory_is_refused() {
        let mount = Mount::new();
        let real = mount.elsewhere();
        fs::create_dir(&mount.own).unwrap();
        symlink(&real, mount.own.join("files")).unwrap();
        assert!(mount.trash().is_err());
        assert!(empty(&real));
    }

    #[test]
    fn a_directory_of_another_user_is_refused() {
        let mount = Mount::new();
        let other = mount.temp.file("mnt/sub", "g.png", b"g");
        // made by this user, so not the user asked for
        fs::create_dir(mount.top.join(format!(".Trash-{}", uid() + 1))).unwrap();
        let refused = trash_at(&other, &mount.temp.home(), Some(&mount.top), uid() + 1);
        assert_eq!(refused.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(fs::read(&other).unwrap(), b"g");
    }

    #[test]
    fn same_device_picks_the_home_trash() {
        let mount = Mount::new();
        let entry = trash_at(&mount.src, &mount.temp.home(), None, uid()).unwrap();
        assert_eq!(entry.stored, mount.temp.home().join("files/f č.png"));
        // absolute there
        let path = format!("Path={}/mnt/sub/f%20%C4%8D.png\n", mount.temp.dir.display());
        assert!(info(&entry).contains(&path));
    }

    // The pieces, against the Python functions they stand for.

    #[test]
    fn path_bytes_are_percent_encoded() {
        // urllib.parse.quote(os.fsencode(path)) as bin/ascii-viewer:363 calls it, run by python3
        for (path, want) in [
            (
                &b"/tmp/my pics/a b#\xc3\xa9.png"[..],
                "/tmp/my%20pics/a%20b%23%C3%A9.png",
            ),
            (b"sub/f \xc4\x8d.png", "sub/f%20%C4%8D.png"),
            (b"/a/B/z_0.9-~x", "/a/B/z_0.9-~x"),
            (b"/\xff\xfe/%41+&=?", "/%FF%FE/%2541%2B%26%3D%3F"),
            (b"a\nb\tc", "a%0Ab%09c"),
            (
                b" !\"#$%&'()*+,-./0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\]^_`abcdefghijklmnopqrstuvwxyz{|}~",
                "%20%21%22%23%24%25%26%27%28%29%2A%2B%2C-./0123456789%3A%3B%3C%3D%3E%3F%40ABCDEFGHIJKLMNOPQRSTUVWXYZ%5B%5C%5D%5E_%60abcdefghijklmnopqrstuvwxyz%7B%7C%7D~",
            ),
        ] {
            assert_eq!(quote(path), want);
        }
    }

    #[test]
    fn a_taken_name_is_numbered_before_its_extension() {
        // os.path.splitext(name) and "%s.2%s" of its halves, run by python3
        for (name, second) in [
            ("x.png", "x.2.png"),
            ("x", "x.2"),
            ("x.", "x.2."),
            (".png", ".png.2"),
            ("..png", "..png.2"),
            ("a..png", "a..2.png"),
            (".bashrc", ".bashrc.2"),
            ("a.tar.gz", "a.tar.2.gz"),
            ("...", "....2"),
            ("a.b.", "a.b.2."),
            (".a.b", ".a.2.b"),
            ("x.2.png", "x.2.2.png"),
            ("f č.png", "f č.2.png"),
            (". .x", ". .2.x"),
        ] {
            assert_eq!(numbered(name.as_bytes(), 1), name.as_bytes());
            assert_eq!(
                String::from_utf8(numbered(name.as_bytes(), 2)).unwrap(),
                second
            );
        }
        // in the trash itself, a name without an extension, a hidden one and one ending in a dot
        let temp = Temp::new();
        for (name, second) in [("n", "n.2"), (".hidden", ".hidden.2"), ("a.b.", "a.b.2.")] {
            temp.trash(&temp.file("one", name, b"1")).unwrap();
            let entry = temp.trash(&temp.file("two", name, b"2")).unwrap();
            assert_eq!(entry.stored.file_name().unwrap(), second);
            assert_eq!(
                entry.info.file_name().unwrap(),
                format!("{second}.trashinfo").as_str()
            );
        }
    }

    #[test]
    fn home_trash_from_the_data_home_or_the_home() {
        // home_trash() of bin/ascii-viewer:289 under these XDG_DATA_HOME, with HOME=/home/u, run by python3
        for (data, want) in [
            (Some("/data"), "/data/Trash"),
            (Some("/data/"), "/data/Trash"),
            (Some(""), "/home/u/.local/share/Trash"),
            (Some("rel/data"), "/home/u/.local/share/Trash"),
            (None, "/home/u/.local/share/Trash"),
        ] {
            let found = home_trash(data.map(OsString::from), Some("/home/u".into()));
            assert_eq!(found.unwrap().as_os_str(), want, "{data:?}");
        }
        assert_eq!(home_trash(None, None), None);
    }

    #[test]
    fn deletion_dates_are_local_time() {
        // date(1) reads the same time zone
        for seconds in [0, 1_700_000_000, 1_720_000_000] {
            let said = Command::new("date")
                .args(["-d", &format!("@{seconds}"), "+%Y-%m-%dT%H:%M:%S"])
                .output()
                .unwrap();
            assert_eq!(stamp(seconds).as_bytes(), said.stdout.trim_ascii_end());
        }
    }

    #[test]
    fn the_folder_is_resolved_and_the_name_kept() {
        // row T4, and bin/ascii-viewer:353
        let temp = Temp::new();
        let real = temp.file("real", "a.png", b"a");
        symlink(temp.dir.join("real"), temp.dir.join("way")).unwrap();
        symlink(&real, temp.dir.join("real/l.png")).unwrap();
        assert_eq!(entry(&temp.dir.join("way/a.png")).unwrap(), real);
        assert_eq!(
            entry(&temp.dir.join("way/l.png")).unwrap(),
            temp.dir.join("real/l.png")
        );
        let here = std::env::current_dir().unwrap().canonicalize().unwrap();
        assert_eq!(entry(Path::new("a.png")).unwrap(), here.join("a.png"));
        assert!(entry(Path::new("/")).is_err());
        assert!(entry(&temp.dir.join("none/a.png")).is_err());
        let trashed = temp.trash(&temp.dir.join("way/a.png")).unwrap();
        assert_eq!(trashed.path, real);
        assert!(info(&trashed).contains(&format!("Path={}\n", real.display())));
    }

    /// Not a test: `trash` in a process of its own, under the environment the test below gives it.
    #[test]
    fn trash_child() {
        let Some(file) = std::env::var_os("ASCIIER_TRASH_CHILD") else {
            return;
        };
        match trash(Path::new(&file)) {
            Ok(entry) => println!("stored {}", entry.stored.display()),
            Err(error) => println!("failed {error}"),
        }
        std::process::exit(0);
    }

    #[test]
    fn the_home_trash_follows_the_environment() {
        // row T1: the data home when it is absolute, else the home
        let temp = Temp::new();
        let data = temp.dir.join("data");
        for (data, trash) in [
            (data.to_str(), "data/Trash"),
            (Some("relative"), "home/.local/share/Trash"),
            (None, "home/.local/share/Trash"),
        ] {
            let src = temp.file("pics", "a.png", b"a");
            let mut child = Command::new(std::env::current_exe().unwrap());
            child
                .args(["--exact", "trash::tests::trash_child", "--nocapture"])
                .env("ASCIIER_TRASH_CHILD", &src)
                .env("HOME", temp.dir.join("home"));
            match data {
                Some(data) => child.env("XDG_DATA_HOME", data),
                None => child.env_remove("XDG_DATA_HOME"),
            };
            let said = child.output().unwrap().stdout;
            let said = String::from_utf8_lossy(&said);
            let stored = temp.dir.join(trash).join("files");
            assert!(
                said.contains(&format!("stored {}/a", stored.display())),
                "{said}"
            );
            assert!(!src.exists() && !empty(&stored));
        }
    }

    #[test]
    fn a_file_on_another_device_goes_to_its_mounts_trash() {
        // row T1: trash_dirs of bin/ascii-viewer:310 at tag python-final: a file on another device than the home trash goes to the trash of its mount point, $top/.Trash-$uid where $top/.Trash is not there, its Path relative to $top, the home trash unused; the device is a tmpfs of a user and mount namespace of its own (unprivileged; uid 0 there), gone with it
        let temp = Temp::new();
        let top = temp.dir.join("mnt");
        fs::create_dir(&top).unwrap();
        // without unshare, or where unprivileged user and mount namespaces are off, no such device can be made: the test is passed over, and T1's mount clause is then unproven by it; said on stderr past libtest's capture, which keeps eprintln! of a passing test unseen
        let mounts = Command::new("unshare")
            .args(["--user", "--map-root-user", "--mount", "sh", "-c"])
            .args([r#"mount -t tmpfs none "$1""#, "sh"])
            .arg(&top)
            .output();
        if !mounts.is_ok_and(|ran| ran.status.success()) {
            let skipped = "skipped: unshare mounts no tmpfs in a user namespace here: row T1's mount clause unproven\n";
            let _ = std::io::Write::write_all(&mut std::io::stderr(), skipped.as_bytes());
            return;
        }
        let script = r#"mount -t tmpfs none "$1" || exit 9
mkdir "$1/sub" && printf x > "$1/sub/$2" || exit 8
ASCIIER_TRASH_CHILD="$1/sub/$2" "$3" --exact trash::tests::trash_child --nocapture
echo "files: $(ls -A "$1/.Trash-0/files")"
echo "left: $(ls -A "$1/sub")"
cat "$1/.Trash-0/info/$2.trashinfo""#;
        let ran = Command::new("unshare")
            .args([
                "--user",
                "--map-root-user",
                "--mount",
                "sh",
                "-c",
                script,
                "sh",
            ])
            .arg(&top)
            .arg("f č.png")
            .arg(std::env::current_exe().unwrap())
            .env("HOME", temp.dir.join("home"))
            .env("XDG_DATA_HOME", temp.dir.join("data"))
            .output()
            .unwrap();
        let said = String::from_utf8_lossy(&ran.stdout);
        let why = String::from_utf8_lossy(&ran.stderr);
        assert!(ran.status.success(), "{said}{why}");
        let stored = format!("stored {}/.Trash-0/files/f č.png\n", top.display());
        assert!(said.contains(&stored), "{said}");
        assert!(said.contains("files: f č.png\nleft: \n"), "{said}");
        assert!(said.contains("\nPath=sub/f%20%C4%8D.png\n"), "{said}");
        assert!(!temp.home().exists() && empty(&top));
    }
}

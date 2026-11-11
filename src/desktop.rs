//! The desktop around the viewer: the oscilloscope child, the clipboard, Hyprland, kitty's remote control.

use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};

/// A command for the program `name`, looked up on PATH: the environment's, or `path` in its place (the tests' stubs).
fn program(name: impl AsRef<OsStr>, path: Option<&OsStr>) -> Command {
    let mut command = Command::new(name);
    if let Some(path) = path {
        command.env("PATH", path);
    }
    command
}

/// The fields of /proc/<pid>/stat after the process name: state, parent, ...
fn stat(pid: u32) -> Option<Vec<String>> {
    let text = std::fs::read(format!("/proc/{pid}/stat")).ok()?;
    let text = String::from_utf8_lossy(&text);
    let fields = text.rsplit_once(')')?.1.split_whitespace();
    Some(fields.map(str::to_owned).collect())
}

/// Toggle Hyprland fullscreen on the window owning this process (row V22); silent on any failure.
pub fn fullscreen() {
    fullscreen_on(None);
}

fn fullscreen_on(path: Option<&OsStr>) {
    let Ok(listed) = program("hyprctl", path).args(["clients", "-j"]).output() else {
        return;
    };
    let Ok(clients) = serde_json::from_slice(&listed.stdout) else {
        return;
    };
    let parent = |pid| stat(pid)?.get(1)?.parse().ok();
    let Some(address) = owner(&clients, std::process::id(), parent) else {
        return;
    };
    let window = format!(
        "hl.dsp.window.fullscreen({{ mode = \"fullscreen\", window = \"address:{address}\" }})"
    );
    let _ = program("hyprctl", path)
        .args(["dispatch", &window])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// The address of the window whose process is `pid` or the nearest ancestor of it among hyprctl's `clients`: by address, not the focused window, which can change between the key and the dispatch. Of one process's windows the last listed counts.
fn owner(
    clients: &serde_json::Value,
    mut pid: u32,
    parent: impl Fn(u32) -> Option<u32>,
) -> Option<String> {
    let clients = clients.as_array()?;
    loop {
        let own = |client: &&serde_json::Value| client["pid"].as_u64() == Some(pid as u64);
        if let Some(client) = clients.iter().rfind(own) {
            return Some(client["address"].as_str()?.to_owned());
        }
        if pid <= 1 {
            return None;
        }
        pid = parent(pid)?;
    }
}

/// The focused monitor's size in px, from `hyprctl monitors -j` (row R9).
pub fn screen_size() -> Option<(u32, u32)> {
    screen_on(None)
}

fn screen_on(path: Option<&OsStr>) -> Option<(u32, u32)> {
    let listed = program("hyprctl", path)
        .args(["monitors", "-j"])
        .output()
        .ok()?;
    let monitors: serde_json::Value = serde_json::from_slice(&listed.stdout).ok()?;
    let focused = monitors
        .as_array()?
        .iter()
        .find(|monitor| monitor["focused"] == true)?;
    let side = |key: &str| u32::try_from(focused[key].as_u64()?).ok();
    Some((side("width")?, side("height")?))
}

/// kitty ligatures for this window, off while the viewer draws (row V4); silent without kitty's socket.
pub fn ligatures(off: bool) {
    let variable = std::env::var_os;
    ligatures_on(
        None,
        variable("KITTY_LISTEN_ON"),
        variable("KITTY_WINDOW_ID"),
        off,
    );
}

/// `kitten @` over kitty's `socket` for its window `window`: the font joins runs of the dithered glyphs ("!=", "<-", "==") into single symbols unless told `always` to leave them; `never` is the configuration's own value.
fn ligatures_on(
    path: Option<&OsStr>,
    socket: Option<OsString>,
    window: Option<OsString>,
    off: bool,
) {
    let (Some(socket), Some(window)) = (socket, window) else {
        return;
    };
    if socket.is_empty() || window.is_empty() {
        return;
    }
    let mut target = OsString::from("id:");
    target.push(window);
    let _ = program("kitten", path)
        .args(["@", "--to"])
        .arg(socket)
        .args(["disable-ligatures", "--match"])
        .arg(target)
        .arg(if off { "always" } else { "never" })
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// PNG bytes to the Wayland clipboard: `wl-copy --type image/png`, in its own session so it outlives the viewer; whether it took them.
pub fn copy_png(png: &[u8]) -> bool {
    copy_on(None, png)
}

fn copy_on(path: Option<&OsStr>, png: &[u8]) -> bool {
    let mut command = program("wl-copy", path);
    command
        .args(["--type", "image/png"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // it forks to serve the clipboard: neither the terminal's end nor the viewer's may take that down
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        })
    };
    let Ok(mut child) = command.spawn() else {
        return false;
    };
    if let Some(mut stdin) = child.stdin.take() {
        // its exit status says whether it took them
        let _ = stdin.write_all(png);
    }
    child.wait().is_ok_and(|status| status.success())
}

/// Show an image in kitty (`kitten icat`), the converter's `--at` on a kitty terminal.
pub fn icat(file: &Path) {
    icat_on(None, file);
}

fn icat_on(path: Option<&OsStr>, file: &Path) {
    let _ = program("kitten", path).arg("icat").arg(file).status();
}

/// The oscilloscope that owns the screen for audio alone (rows A1, A6).
pub struct Scope {
    child: Option<Child>,
}

/// The scope's command: the program at `beside` if there is one, else `scope` on PATH.
fn scope_command(path: Option<&OsStr>, beside: Option<&Path>, target: &str) -> Command {
    let beside = beside.filter(|file| file.exists());
    let mut command = program(beside.unwrap_or(Path::new("scope")), path);
    command
        .env("SCOPE_SPARE_ROWS", "1")
        .env("SCOPE_TARGET", target)
        // the player's node name: with it the scope's recorder would bear the name it is told to record
        .env_remove("PIPEWIRE_PROPS")
        .stdin(Stdio::null());
    let viewer = unsafe { libc::getpid() };
    // SIGKILL when its starter ends, however that happens; a starter gone before this took hold shows as another parent
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 || libc::getppid() != viewer
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        })
    };
    command
}

impl Scope {
    /// Start `scope` (beside the executable, else on PATH) recording the node `target`, one spare row, stdin /dev/null, `PIPEWIRE_PROPS` removed, dying with this process; a missing program gives a scope that does nothing.
    /// The death signal follows the thread that calls this: call it from the one that lives as long as the process.
    pub fn start(target: &str) -> Scope {
        let beside = std::env::current_exe().map(|program| program.with_file_name("scope"));
        Scope::start_on(None, beside.ok().as_deref(), target)
    }

    fn start_on(path: Option<&OsStr>, beside: Option<&Path>, target: &str) -> Scope {
        Scope {
            child: scope_command(path, beside, target).spawn().ok(),
        }
    }

    /// The scope's pid while it runs; one that has exited is reaped and forgotten: its pid may be another process's next.
    fn running(&mut self) -> Option<libc::pid_t> {
        let child = self.child.as_mut()?;
        if let Ok(None) = child.try_wait() {
            return Some(child.id() as libc::pid_t);
        }
        self.child = None;
        None
    }

    /// SIGSTOP it and wait until it is stopped (100 ms at most); false when it is not running.
    pub fn stop(&mut self) -> bool {
        let Some(pid) = self.running() else {
            return false;
        };
        unsafe { libc::kill(pid, libc::SIGSTOP) };
        // stopped, none of its writes lands in what the viewer draws next
        for _ in 0..50 {
            match stat(pid as u32) {
                Some(fields) if fields.first().is_some_and(|state| state != "T") => {
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                _ => break,
            }
        }
        true
    }

    /// SIGCONT.
    pub fn resume(&mut self) {
        if let Some(pid) = self.running() {
            unsafe { libc::kill(pid, libc::SIGCONT) };
        }
    }

    /// SIGCONT, SIGTERM, reap.
    pub fn end(&mut self) {
        // stopped, it could not act on SIGTERM
        if let Some(pid) = self.running() {
            unsafe {
                libc::kill(pid, libc::SIGCONT);
                libc::kill(pid, libc::SIGTERM);
            }
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.wait();
        }
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        self.end();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{Duration, Instant};

    /// How long a test waits for a stub before it fails; the stubs act within milliseconds.
    const PATIENCE: Duration = Duration::from_secs(10);

    /// Notes its arguments and what its descriptors are in `<itself>.calls`, then prints `<itself>.<first argument>` if there is one: stands in for hyprctl and kitten.
    const NOTES: &str = r#"#!/bin/sh
in=$(readlink /proc/$$/fd/0) out=$(readlink /proc/$$/fd/1) err=$(readlink /proc/$$/fd/2)
{ printf '%s\n' "$@"; echo "in=${in%%\[*} out=${out%%\[*} err=${err%%\[*}"; } >> "$0.calls"
[ ! -e "$0.$1" ] || cat "$0.$1"
"#;

    /// wl-copy: notes as above and its session, keeps its input in `<itself>.stdin`, exits with the status in `<itself>.exit`.
    const COPY: &str = r#"#!/bin/sh
in=$(readlink /proc/$$/fd/0) out=$(readlink /proc/$$/fd/1) err=$(readlink /proc/$$/fd/2)
{ printf '%s\n' "$@"; echo "in=${in%%\[*} out=${out%%\[*} err=${err%%\[*}"; echo "leads=$(cut -d' ' -f6 /proc/$$/stat | grep -cx $$)"; } >> "$0.calls"
cat > "$0.stdin"
exit "$(cat "$0.exit" 2>/dev/null || echo 0)"
"#;

    /// The oscilloscope: notes its environment, input and session in `<itself>.out`, then runs until SIGTERM, which it notes too.
    const SCOPE: &str = r#"#!/bin/sh
in=$(readlink /proc/$$/fd/0) out=$(readlink /proc/$$/fd/1) err=$(readlink /proc/$$/fd/2)
trap 'echo term >> "$0.out"; exit 0' TERM
{ echo "args=$# rows=$SCOPE_SPARE_ROWS target=$SCOPE_TARGET props=${PIPEWIRE_PROPS-none}"; echo "in=$in out=$out err=$err"; echo "session=$(cut -d' ' -f6 /proc/$$/stat)"; echo ready; } > "$0.out"
while :; do sleep 0.02; done
"#;

    /// A temp dir whose bin/ holds stub programs; `path` puts it before the system's, so the stubs are found and never the real programs.
    struct Stubs {
        dir: PathBuf,
    }

    impl Stubs {
        fn new() -> Stubs {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let name = format!(
                "asciier-desktop-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            );
            let dir = std::env::temp_dir().join(name);
            std::fs::create_dir_all(dir.join("bin")).unwrap();
            Stubs { dir }
        }

        /// The stubs first, then where sh finds cat, cut, readlink and sleep.
        fn path(&self) -> OsString {
            let mut path = self.dir.join("bin").into_os_string();
            path.push(":/usr/bin:/bin");
            path
        }

        /// A PATH with no program of the desktop on it.
        fn nowhere(&self) -> OsString {
            self.dir.join("empty").into_os_string()
        }

        /// The stub `name` in bin/, a shell script.
        fn stub(&self, name: &str, script: &str) {
            write_program(&self.dir.join("bin").join(name), script);
        }

        /// The file `name` beside the stubs: what one prints, or noted.
        fn put(&self, name: &str, text: &str) {
            std::fs::write(self.dir.join("bin").join(name), text).unwrap();
        }

        fn read(&self, name: &str) -> String {
            std::fs::read_to_string(self.dir.join("bin").join(name)).unwrap_or_default()
        }

        /// Wait until the stub's file `name` holds `text`.
        fn wait(&self, name: &str, text: &str) {
            let end = Instant::now() + PATIENCE;
            while !self.read(name).contains(text) {
                assert!(
                    Instant::now() < end,
                    "no {text} in {name}: {}",
                    self.read(name)
                );
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }

    impl Drop for Stubs {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// An executable script at `file`, written by a child process: written from here, a child forked meanwhile by another test would hold it open for writing, and running it would fail as busy.
    fn write_program(file: &Path, script: &str) {
        let mut writer = Command::new("/bin/sh")
            .args(["-c", r#"cat > "$0" && chmod 755 "$0""#])
            .arg(file)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        writer
            .stdin
            .take()
            .unwrap()
            .write_all(script.as_bytes())
            .unwrap();
        assert!(writer.wait().unwrap().success());
    }

    /// What a descriptor of this process is: a child that inherits it has the same.
    fn own(fd: u32) -> String {
        let link = std::fs::read_link(format!("/proc/self/fd/{fd}")).unwrap();
        link.to_str().unwrap().to_owned()
    }

    fn state(scope: &Scope) -> String {
        stat(scope.child.as_ref().unwrap().id()).unwrap()[0].clone()
    }

    fn parent_of(pid: u32) -> Option<u32> {
        match pid {
            100 => Some(50),
            50 => Some(10),
            10 => Some(1),
            _ => None,
        }
    }

    #[test]
    fn the_window_is_the_nearest_ancestors() {
        // Player.fullscreen of bin/ascii-viewer:1601 at tag python-final: the pid walk and its dict of pid to address, read from the code
        let window = |clients: serde_json::Value| owner(&clients, 100, parent_of);
        let listed =
            |pid: i64, address: &str| serde_json::json!({ "pid": pid, "address": address });
        assert_eq!(
            window(serde_json::json!([listed(7, "0x7"), listed(10, "0xa")])),
            Some("0xa".into())
        );
        assert_eq!(
            window(serde_json::json!([
                listed(10, "0xfar"),
                listed(50, "0xnear")
            ])),
            Some("0xnear".into())
        );
        assert_eq!(
            window(serde_json::json!([
                listed(100, "0xown"),
                listed(50, "0xnear")
            ])),
            Some("0xown".into())
        );
        // one process, several windows: the last listed, as a dict keeps it
        assert_eq!(
            window(serde_json::json!([
                listed(50, "0xold"),
                listed(50, "0xnew")
            ])),
            Some("0xnew".into())
        );
        // the walk ends at pid 1, which may own a window too
        assert_eq!(
            window(serde_json::json!([listed(1, "0xinit")])),
            Some("0xinit".into())
        );
        for none in [
            serde_json::json!([]),
            serde_json::json!([listed(7, "0x7"), listed(-1, "0xneg")]),
            serde_json::json!([{ "address": "0xnopid" }, { "pid": 50 }]),
            serde_json::json!({ "pid": 50, "address": "0xmap" }),
            serde_json::json!("clients"),
        ] {
            assert_eq!(window(none), None);
        }
        // a parent that cannot be read ends the walk
        let clients = serde_json::json!([listed(10, "0xa")]);
        assert_eq!(owner(&clients, 100, |_| None), None);
        assert_eq!(owner(&clients, 10, |_| None), Some("0xa".into()));
    }

    #[test]
    fn a_process_parent_is_read_from_proc() {
        let parent = unsafe { libc::getppid() } as u32;
        assert_eq!(stat(std::process::id()).unwrap()[1], parent.to_string());
        assert!(stat(u32::MAX).is_none());
    }

    #[test]
    fn fullscreen_dispatches_for_the_window_that_owns_the_process() {
        // row V22; the dispatch text: the f-string of bin/ascii-viewer:1618 at tag python-final, run by python3
        let stubs = Stubs::new();
        stubs.stub("hyprctl", NOTES);
        let parent = unsafe { libc::getppid() };
        let clients = format!(
            r#"[{{"pid": 1, "address": "0x1"}}, {{"address": "0xnopid"}}, {{"pid": {parent}, "address": "0xold"}},
            {{"pid": -1, "address": "0xneg"}}, {{"pid": {parent}, "address": "0x55d0c0ffee"}}]"#
        );
        stubs.put("hyprctl.clients", &clients);
        fullscreen_on(Some(&stubs.path()));
        assert_eq!(
            stubs.read("hyprctl.calls"),
            "clients\n-j\nin=/dev/null out=pipe: err=pipe:\ndispatch\nhl.dsp.window.fullscreen({ mode = \"fullscreen\", window = \"address:0x55d0c0ffee\" })\nin=/dev/null out=/dev/null err=/dev/null\n"
        );
    }

    #[test]
    fn fullscreen_is_silent_without_a_window_or_hyprctl() {
        let stubs = Stubs::new();
        stubs.stub("hyprctl", NOTES);
        // no ancestor's window, no clients, no JSON, no list, nothing
        let stranger = r#"[{"pid": 4194305, "address": "0xfar"}]"#;
        for (round, clients) in [stranger, "[]", "not json", r#"{"pid": 1}"#, ""]
            .iter()
            .enumerate()
        {
            stubs.put("hyprctl.clients", clients);
            fullscreen_on(Some(&stubs.path()));
            let calls = stubs.read("hyprctl.calls");
            assert_eq!(calls.matches("clients\n-j\n").count(), round + 1);
            assert!(!calls.contains("dispatch"), "{clients}");
        }
        fullscreen_on(Some(&stubs.nowhere()));
    }

    #[test]
    fn the_screen_is_the_focused_monitor() {
        // row R9; screen_size of bin/ascii-viewer:570 at tag python-final, read from the code
        let stubs = Stubs::new();
        stubs.stub("hyprctl", NOTES);
        let monitors = r#"[{"name": "a", "width": 2560, "height": 1440, "focused": false}, {"name": "b", "width": 1920, "height": 1200, "focused": true},
            {"name": "c", "width": 800, "height": 600, "focused": true}]"#;
        stubs.put("hyprctl.monitors", monitors);
        assert_eq!(screen_on(Some(&stubs.path())), Some((1920, 1200)));
        assert_eq!(
            stubs.read("hyprctl.calls"),
            "monitors\n-j\nin=/dev/null out=pipe: err=pipe:\n"
        );
        for none in [
            r#"[{"width": 2560, "height": 1440, "focused": false}]"#,
            r#"[{"width": 2560, "focused": true}]"#,
            r#"[{"width": -1, "height": 1440, "focused": true}]"#,
            r#"{"width": 2560, "height": 1440, "focused": true}"#,
            "[]",
            "HYPRLAND_INSTANCE_SIGNATURE not set! (is hyprland running?)",
            "",
        ] {
            stubs.put("hyprctl.monitors", none);
            assert_eq!(screen_on(Some(&stubs.path())), None, "{none}");
        }
        assert_eq!(screen_on(Some(&stubs.nowhere())), None);
    }

    #[test]
    fn ligatures_go_off_and_back_through_kittys_socket() {
        // row V4; the command of bin/ascii-viewer:2436 at tag python-final
        let stubs = Stubs::new();
        stubs.stub("kitten", NOTES);
        let (socket, window) = (Some("unix:/run/kitty-7".into()), Some("12".into()));
        let path = stubs.path();
        ligatures_on(Some(&path), socket.clone(), window.clone(), true);
        ligatures_on(Some(&path), socket.clone(), window.clone(), false);
        let call = |mode: &str| {
            format!(
                "@\n--to\nunix:/run/kitty-7\ndisable-ligatures\n--match\nid:12\n{mode}\nin=/dev/null out=/dev/null err=/dev/null\n"
            )
        };
        assert_eq!(stubs.read("kitten.calls"), call("always") + &call("never"));
        // only with both variables set
        ligatures_on(Some(&path), None, window.clone(), true);
        ligatures_on(Some(&path), socket.clone(), None, true);
        ligatures_on(Some(&path), Some("".into()), window.clone(), true);
        ligatures_on(Some(&path), socket.clone(), Some("".into()), true);
        assert_eq!(
            stubs
                .read("kitten.calls")
                .matches("disable-ligatures")
                .count(),
            2
        );
        // no kitten: nothing happens
        ligatures_on(Some(&stubs.nowhere()), socket, window, true);
    }

    #[test]
    fn icat_shows_the_file_on_the_terminal() {
        // row C6
        let stubs = Stubs::new();
        stubs.stub("kitten", NOTES);
        icat_on(Some(&stubs.path()), Path::new("/tmp/out ascii.png"));
        // it draws on this terminal: the descriptors are this process's own (a pipe's number left out, as the stub notes it)
        let [stdin, stdout, stderr] = [0, 1, 2].map(|fd| {
            let link = own(fd);
            link[..link.find('[').unwrap_or(link.len())].to_owned()
        });
        assert_eq!(
            stubs.read("kitten.calls"),
            format!("icat\n/tmp/out ascii.png\nin={stdin} out={stdout} err={stderr}\n")
        );
        icat_on(Some(&stubs.nowhere()), Path::new("/tmp/out.png"));
    }

    #[test]
    fn copy_hands_wl_copy_the_png_in_a_session_of_its_own() {
        // row S14; the command of bin/ascii-viewer:1666 at tag python-final; tests/test_viewer_tty.py:190
        let stubs = Stubs::new();
        stubs.stub("wl-copy", COPY);
        let png: Vec<u8> = (0..300_000u32).map(|i| (i * 7) as u8).collect();
        assert!(copy_on(Some(&stubs.path()), &png));
        assert_eq!(
            stubs.read("wl-copy.calls"),
            "--type\nimage/png\nin=pipe: out=/dev/null err=/dev/null\nleads=1\n"
        );
        assert_eq!(
            std::fs::read(stubs.dir.join("bin/wl-copy.stdin")).unwrap(),
            png
        );
        // its status decides
        stubs.put("wl-copy.exit", "1");
        assert!(!copy_on(Some(&stubs.path()), &png));
        // one that does not read is no failure by itself
        stubs.stub("wl-copy", "#!/bin/sh\nexit 0\n");
        assert!(copy_on(Some(&stubs.path()), &png));
        assert!(!copy_on(Some(&stubs.nowhere()), &png));
    }

    #[test]
    fn the_scope_starts_with_its_target_and_ends_on_term() {
        // rows A1, V6; ScopeStreamTest of tests/test_viewer.py:629
        let stubs = Stubs::new();
        stubs.stub("scope", SCOPE);
        let command = scope_command(Some(&stubs.path()), None, "asciier-77");
        let set: Vec<_> = command.get_envs().collect();
        // the node name of the player's stream must not reach the scope's recorder
        assert!(set.contains(&(OsStr::new("PIPEWIRE_PROPS"), None)));
        assert!(set.contains(&(OsStr::new("SCOPE_TARGET"), Some(OsStr::new("asciier-77")))));
        let mut scope = Scope::start_on(Some(&stubs.path()), None, "asciier-77");
        stubs.wait("scope.out", "ready");
        let session = unsafe { libc::getsid(0) };
        assert_eq!(
            stubs.read("scope.out"),
            format!(
                "args=0 rows=1 target=asciier-77 props=none\nin=/dev/null out={} err={}\nsession={session}\nready\n",
                own(1),
                own(2)
            )
        );
        let pid = scope.child.as_ref().unwrap().id();
        scope.end();
        assert!(scope.child.is_none() && stubs.read("scope.out").ends_with("ready\nterm\n"));
        assert!(stat(pid).is_none());
        // again: nothing
        scope.end();
        assert!(!scope.stop());
        scope.resume();
    }

    #[test]
    fn the_scope_beside_the_program_comes_before_the_paths() {
        let stubs = Stubs::new();
        stubs.stub("scope", SCOPE);
        let beside = stubs.dir.join("bin/beside/scope");
        std::fs::create_dir(beside.parent().unwrap()).unwrap();
        write_program(&beside, SCOPE);
        let mut scope = Scope::start_on(Some(&stubs.path()), Some(&beside), "node");
        stubs.wait("beside/scope.out", "ready");
        scope.end();
        assert_eq!(stubs.read("scope.out"), "");
        // none there: the one on the path
        let mut scope = Scope::start_on(
            Some(&stubs.path()),
            Some(&stubs.dir.join("none/scope")),
            "node",
        );
        stubs.wait("scope.out", "ready");
        scope.end();
    }

    #[test]
    fn the_scope_stops_for_help_and_goes_on() {
        // row A6; HelpTest.test_audio_alone_stops_the_scope_and_keeps_its_colour of tests/test_viewer.py:614
        let stubs = Stubs::new();
        stubs.stub("scope", SCOPE);
        let mut scope = Scope::start_on(Some(&stubs.path()), None, "node");
        stubs.wait("scope.out", "ready");
        assert!(scope.stop());
        assert_eq!(state(&scope), "T");
        scope.resume();
        let end = Instant::now() + PATIENCE;
        while state(&scope) == "T" {
            assert!(Instant::now() < end);
            std::thread::sleep(Duration::from_millis(1));
        }
        scope.end();
    }

    #[test]
    fn ending_a_stopped_scope_continues_it_first() {
        // N14: leaving while help shows must not wait on a scope that cannot act
        let stubs = Stubs::new();
        stubs.stub("scope", SCOPE);
        let mut scope = Scope::start_on(Some(&stubs.path()), None, "node");
        stubs.wait("scope.out", "ready");
        assert!(scope.stop());
        // on a thread of its own: a wait that never ends must fail the test, not hold it
        let (done, ended) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            scope.end();
            let _ = done.send(());
        });
        assert!(ended.recv_timeout(PATIENCE).is_ok());
        assert!(stubs.read("scope.out").ends_with("term\n"));
    }

    #[test]
    fn a_dropped_scope_is_ended() {
        let stubs = Stubs::new();
        stubs.stub("scope", SCOPE);
        let scope = Scope::start_on(Some(&stubs.path()), None, "node");
        stubs.wait("scope.out", "ready");
        drop(scope);
        assert!(stubs.read("scope.out").ends_with("term\n"));
    }

    #[test]
    fn without_a_scope_the_viewer_goes_on() {
        // row A1a
        let stubs = Stubs::new();
        let mut scope = Scope::start_on(
            Some(&stubs.nowhere()),
            Some(&stubs.dir.join("none/scope")),
            "node",
        );
        assert!(scope.child.is_none() && !scope.stop());
        scope.resume();
        scope.end();
        // one that ran and left is not started again, nor signalled
        stubs.stub("scope", "#!/bin/sh\necho gone > \"$0.out\"\n");
        let mut scope = Scope::start_on(Some(&stubs.path()), None, "node");
        stubs.wait("scope.out", "gone");
        let end = Instant::now() + PATIENCE;
        while scope.stop() {
            assert!(Instant::now() < end);
            scope.resume();
        }
        assert!(scope.child.is_none());
        scope.end();
    }

    #[test]
    fn the_scope_dies_with_the_thread_that_started_it() {
        // row V6: the death signal follows the starter, in the viewer the loop's thread, whose end is the viewer's
        let stubs = Stubs::new();
        stubs.stub("scope", SCOPE);
        let path = stubs.path();
        let starter = std::thread::spawn(move || {
            let mut scope = Scope::start_on(Some(&path), None, "node");
            scope.child.take().unwrap()
        });
        let mut child = starter.join().unwrap();
        let end = Instant::now() + PATIENCE;
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() > end {
                let _ = child.kill();
                panic!("the scope outlives its starter");
            }
            std::thread::sleep(Duration::from_millis(2));
        };
        assert_eq!(status.signal(), Some(libc::SIGKILL));
    }
}

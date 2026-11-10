//! The freedesktop.org trash (spec 1.0): move a file there, put the last one back.

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
    Io(std::io::Error),
}

/// Move the file at `path` to the trash without copying (rows T1-T4 of the inventory).
pub fn trash(path: &Path) -> std::io::Result<Trashed> {
    todo!("{path:?}")
}

/// Put a trashed file back where it was and drop its .trashinfo (row T7): a hard link first, so nothing is replaced.
pub fn untrash(entry: &Trashed) -> Result<(), Refused> {
    todo!("{entry:?}")
}

//! asciier: `asciier FILE` views a video, an audio file or a still image as text; `asciier convert` writes one as PNG or MP4.

mod cells;
mod convert;
mod desktop;
mod font;
mod look;
mod mpris;
mod mpv;
mod page;
mod paint;
mod picture;
mod still;
mod term;
mod theme;
mod trash;
mod view;

use std::process::ExitCode;

/// A failure with its message; `main` prints it as `asciier: <message>` and exits 1.
#[derive(Debug)]
pub struct Error(pub String);
pub type Result<T> = std::result::Result<T, Error>;

impl<E: std::fmt::Display> From<E> for Error {
    fn from(e: E) -> Self {
        Error(e.to_string())
    }
}

/// `err!("...")`: an early return with a formatted `Error`.
#[macro_export]
macro_rules! err {
    ($($arg:tt)*) => { return Err($crate::Error(format!($($arg)*))) };
}

pub const NAME: &str = "asciier";
const USAGE: &str = "usage: asciier FILE (needs a terminal)\n       asciier convert [options] INPUT (asciier convert --help)";

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let first = args.next();
    let code = match first.as_ref().and_then(|a| a.to_str()) {
        Some("convert") => convert::main(args),
        Some("--help" | "-h") => {
            println!("{USAGE}\n\nKeys: F1 in the viewer.");
            0
        }
        Some("--version") => {
            println!("{NAME} {}", env!("CARGO_PKG_VERSION"));
            0
        }
        _ => match (first, args.next()) {
            (Some(file), None) => view::main(std::path::Path::new(&file)),
            _ => {
                eprintln!("{USAGE}");
                1
            }
        },
    };
    ExitCode::from(code)
}

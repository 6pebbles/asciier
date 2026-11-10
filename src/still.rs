//! The still view: zoom, pan, turn, the folder's other images, the image itself or its ASCII, copy, trash.

use crate::picture::Still;
use crate::view::Ui;
use image::RgbImage;
use std::path::{Path, PathBuf};

/// An SVG part rendered in the background: the view it was asked for and its pixels (`None`: resvg failed).
pub struct Part {
    pub key: String,
    pub image: Option<image::RgbaImage>,
}

/// The still on screen, its view, its folder, and what was trashed in this run.
pub struct Gallery {
    path: PathBuf,
}

impl Gallery {
    /// The still at `path`, unturned, at the fit; `image`: shown as itself over the kitty protocol, else as ASCII.
    pub fn new(path: PathBuf, still: Still, image: bool) -> Gallery {
        let _ = (still, image);
        Gallery { path }
    }

    /// The file on screen (the folder's last image stays named after it is gone).
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A key of the still view (zoom, pan, step, turn, `v`, `y`, `d`, `u`, `D` and its answer); false when the key is not the still's: the viewer then tries the keys shared with video. On the empty folder everything but `u`, `q`, `Q`, `f` counts as taken.
    pub fn key(&mut self, key: &[u8], ui: &mut Ui) -> bool {
        todo!("{key:?} {}", ui.epoch)
    }

    /// The end of a key batch: open the image the pending steps reached (row S8).
    pub fn settle(&mut self, ui: &mut Ui) {
        todo!("{}", ui.epoch)
    }

    /// The screen without its status row: the image placed over the kitty protocol, or its ASCII, or the empty folder's notice; `clear`: from a cleared screen.
    pub fn draw(&mut self, ui: &mut Ui, clear: bool) -> Vec<u8> {
        todo!("{clear} {}", ui.epoch)
    }

    /// A background SVG part arrived.
    pub fn part(&mut self, part: Part, ui: &mut Ui) {
        todo!("{} {}", part.key, ui.epoch)
    }

    /// The whole picture as turned, transparency over #808080: what `s` and `r` take. `None` on the empty folder.
    pub fn source(&self) -> Option<RgbImage> {
        todo!()
    }

    /// `D` asked: the status row waits for `y`.
    pub fn confirming(&self) -> bool {
        todo!()
    }

    /// The `D` question for a status row of `cols` cells, while it waits: `delete <name> permanently? y/n`, the name cut with `…`.
    pub fn question(&self, cols: u32) -> Option<String> {
        todo!("{cols}")
    }
}

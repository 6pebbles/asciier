//! The render page (`r`): the picture as the converter would save it, set interactively and exported.

use crate::Result;
use crate::convert::Options;
use crate::view::Ui;
use image::RgbImage;
use std::path::PathBuf;

/// What the viewer does after a key.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum After {
    Stay,
    Close,
}

pub struct Page {
    out: PathBuf,
}

impl Page {
    /// The page for `picture` (the still as turned, or a video's frame), to be exported as `out`, opening on the look of `options`.
    pub fn new(picture: RgbImage, out: PathBuf, options: Options) -> Result<Page> {
        let _ = (picture, options);
        Ok(Page { out })
    }

    pub fn key(&mut self, key: &[u8]) -> After {
        todo!("{key:?} {:?}", self.out)
    }

    /// The whole screen: menu, list, prompt or crop editor on the left, the preview in its frame on the right.
    pub fn draw(&mut self, ui: &mut Ui) -> Vec<u8> {
        todo!("{}", ui.epoch)
    }
}

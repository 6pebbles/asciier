//! Still images: opened upright with their transparency, SVG through resvg, cut and scaled for the screen, encoded as PNG.

use crate::Result;
use crate::theme::Rgb;
use fast_image_resize as fir;
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::metadata::Orientation;
use image::{
    ColorType, DynamicImage, ImageBuffer, ImageDecoder, ImageFormat, ImageReader, Pixel, RgbImage,
    RgbaImage,
};
use resvg::{tiny_skia, usvg};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::{Arc, OnceLock};

/// A still's pixels: RGBA when anything in it is transparent, else RGB.
#[derive(Clone)]
pub enum Pic {
    Rgb(RgbImage),
    Rgba(RgbaImage),
}

impl Pic {
    pub fn size(&self) -> (u32, u32) {
        match self {
            Pic::Rgb(i) => i.dimensions(),
            Pic::Rgba(i) => i.dimensions(),
        }
    }
}

/// An opened still: upright pixels (an SVG's first raster, `SVG_WIDTH` px wide) and the SVG behind them.
pub struct Still {
    pub pic: Pic,
    pub svg: Option<Arc<Svg>>,
}

pub enum Opened {
    Still(Still),
    /// Several frames to play (GIF, APNG, WebP): the media way.
    Animation,
    /// Over `MAX_PIXELS`; the message `<name> is too large to open (over 179 megapixels)`.
    TooLarge(String),
    NotImage,
}

/// Px an image may have: twice Pillow's MAX_IMAGE_PIXELS.
pub const MAX_PIXELS: u64 = 178_956_970;
pub const SVG_WIDTH: u32 = 1600;
/// Px on an SVG raster's long side, at most, when the converter rasterizes again.
pub const SVG_MAX: u32 = 8192;

/// Why a file gave no picture.
enum Not {
    Image,
    Animation,
    TooLarge,
}

fn too_large((w, h): (u32, u32)) -> bool {
    w as u64 * h as u64 > MAX_PIXELS
}

/// A file that cannot be read as far as a walk goes holds no picture.
impl From<std::io::Error> for Not {
    fn from(_: std::io::Error) -> Not {
        Not::Image
    }
}

/// `work`'s result; `None` when it panicked: a decoder's or resvg's panic is a file that does not open, with no message and the terminal left as it is.
fn caught<T>(work: impl FnOnce() -> T) -> Option<T> {
    crate::term::guard(std::panic::AssertUnwindSafe(work)).ok()
}

/// A raster still's frames as Pillow's n_frames counts a well-formed file's (lib/ascii_image.py:61), up to 2, nothing decoded: a GIF by its blocks, an APNG and a WebP by their chunks, an AVIF by its boxes, 1 for any other format; `Not::Image` where they cannot be counted, `Not::TooLarge` for a GIF whose screen, grown to its first frame, and for a PNG, a QOI, an AVIF, a BMP, an icon or a WebP whose header is over `MAX_PIXELS`, measured before their decoders, which give up on some such headers as no picture or decode an AVIF whole to learn its size; a damaged file can count otherwise in Pillow (N6, N7).
fn frames(path: &Path, format: Option<ImageFormat>) -> std::result::Result<u32, Not> {
    let mut file = BufReader::new(File::open(path).map_err(|_| Not::Image)?);
    match format {
        Some(ImageFormat::Gif) => gif_frames(&mut file),
        Some(ImageFormat::Png) => png_frames(&mut file),
        Some(ImageFormat::Qoi) => qoi_frames(&mut file),
        Some(ImageFormat::Avif) => avif_frames(&mut file),
        Some(ImageFormat::WebP) => webp_frames(&mut file),
        Some(ImageFormat::Ico) => ico_frames(&mut file),
        Some(ImageFormat::Bmp) => bmp_frames(&mut file),
        _ => Ok(1),
    }
}

/// A GIF's frames, up to 2, by a walk of its blocks as Pillow 13 walks a well-formed GIF (GifImagePlugin _seek), nothing decoded: its image descriptors, the blocks between walked past their data; the trailer or the file's end ends the walk, a byte that starts no block is passed over, a descriptor cut short counts no frame; no frame, or no GIF87a or GIF89a signature (a GIF by its suffix alone), `Not::Image`. The first frame reaching past the screen grows the picture to its extent, read from the descriptor's first 8 bytes, before its flags, as Pillow measures it: over `MAX_PIXELS`, `Not::TooLarge`, as is a screen over it once the first frame is read (Image.open's measure). A damaged GIF can count otherwise in Pillow (N6, N7): it reads on past an extension's empty first sub-block (a comment's aside) and, before the first frame, past a NETSCAPE2.0 block's empty second one, and there refuses a graphic control block too short for its fields.
fn gif_frames(file: &mut (impl Read + Seek)) -> std::result::Result<u32, Not> {
    fn byte(file: &mut impl Read) -> Option<u8> {
        let mut byte = [0];
        file.read_exact(&mut byte).ok().map(|()| byte[0])
    }
    fn skip(file: &mut impl Seek, bytes: i64) -> std::result::Result<(), Not> {
        file.seek_relative(bytes).map_err(|_| Not::Image)
    }
    // past the data sub-blocks to their terminator, or to the file's end
    fn blocks(file: &mut (impl Read + Seek)) -> std::result::Result<(), Not> {
        while let Some(length @ 1..) = byte(file) {
            skip(file, i64::from(length))?;
        }
        Ok(())
    }
    let colours = |flags: u8| {
        if flags & 0x80 == 0 {
            0
        } else {
            3 << ((flags & 7) + 1)
        }
    };
    let side = |bytes: &[u8]| u32::from(u16::from_le_bytes([bytes[0], bytes[1]]));
    let mut head = [0; 13]; // the header and the logical screen descriptor
    file.read_exact(&mut head).map_err(|_| Not::Image)?;
    if !head.starts_with(b"GIF87a") && !head.starts_with(b"GIF89a") {
        return Err(Not::Image); // a GIF by its suffix alone
    }
    let screen = (side(&head[6..]), side(&head[8..]));
    skip(file, colours(head[10]))?;
    let mut frames = 0;
    while frames < 2 {
        match byte(file) {
            None | Some(b';') => break,
            Some(b'!') => {
                if byte(file).is_none() {
                    break; // no label
                }
                blocks(file)?;
            }
            Some(b',') => {
                let mut image = Vec::new();
                file.by_ref()
                    .take(9)
                    .read_to_end(&mut image)
                    .map_err(|_| Not::Image)?;
                if frames == 0 && image.len() >= 8 {
                    // the extent, read before the flags
                    let extent = (
                        side(&image) + side(&image[4..]),
                        side(&image[2..]) + side(&image[6..]),
                    );
                    let grown = (extent.0.max(screen.0), extent.1.max(screen.1));
                    if grown != screen && too_large(grown) {
                        return Err(Not::TooLarge);
                    }
                }
                if image.len() < 9 {
                    break;
                }
                skip(file, colours(image[8]))?;
                if byte(file).is_none() {
                    break; // no LZW code size
                }
                frames += 1;
                if too_large(screen) {
                    return Err(Not::TooLarge); // Image.open's measure, once the first frame is read
                }
                blocks(file)?;
            }
            Some(_) => {}
        }
    }
    match frames {
        0 => Err(Not::Image),
        frames => Ok(frames),
    }
}

/// Bytes of a PNG's profile Pillow 13 inflates at most: it refuses a file whose profile holds more (PngImagePlugin MAX_TEXT_CHUNK, _safe_zlib_decompress).
const MAX_TEXT_CHUNK: u64 = 1 << 20;

/// A PNG's frames as Pillow 13 counts them from the chunks before the image data: acTL's number, one more for a default image outside the animation (no fcTL before the IDAT); 1 without an acTL, with one of no frames or over 2^31, or with two; `Not::Image` without PNG's signature (a PNG by its suffix alone), for a header under 13 bytes or whose CRC fails, which the png crate refuses too, for an acTL under 8 bytes and for a profile over `MAX_TEXT_CHUNK`, files Pillow refuses, and for a file without image data, which Pillow counts as one frame where IEND ends it (N6, N7); `Not::TooLarge` for a header over `MAX_PIXELS`, measured at the image data as Pillow measures it, where the png crate gives up on the largest headers as no picture. The profile measured is the png crate's, the first iCCP within its bounds whose CRC holds (it passes over an ancillary chunk whose CRC fails), which it inflates whole while the decoder is made.
fn png_frames(file: &mut (impl BufRead + Seek)) -> std::result::Result<u32, Not> {
    let (mut size, mut frames, mut control, mut profile) = (None, None, false, true);
    let mut signature = [0; 8];
    file.read_exact(&mut signature)?;
    if &signature != b"\x89PNG\r\n\x1a\n" {
        return Err(Not::Image); // a PNG by its suffix alone
    }
    loop {
        let mut head = [0; 8];
        file.read_exact(&mut head)?;
        let length = u32::from_be_bytes([head[0], head[1], head[2], head[3]]);
        let mut left = u64::from(length) + 4; // to the next chunk: the data, its CRC
        match &head[4..] {
            b"IDAT" | b"fdAT" if size.is_some_and(too_large) => return Err(Not::TooLarge),
            b"IDAT" | b"fdAT" => return Ok(frames.map_or(1, |n| n + u32::from(!control))),
            b"IHDR" if size.is_none() => {
                let mut sides = [0; 8];
                file.by_ref().take(length.into()).read_exact(&mut sides)?;
                file.seek_relative(-8)?;
                if length < 13 || !crc_holds(file, &head[4..], length.into())? {
                    return Err(Not::Image); // a header the png crate refuses, and Pillow
                }
                left = 0;
                let side = |at: usize| {
                    u32::from_be_bytes([sides[at], sides[at + 1], sides[at + 2], sides[at + 3]])
                };
                size = Some((side(0), side(4)));
            }
            b"fcTL" => control = true,
            b"acTL" if length < 8 => return Err(Not::Image),
            b"acTL" => {
                let mut count = [0; 4];
                file.read_exact(&mut count)?;
                left -= 4;
                let count = u32::from_be_bytes(count);
                frames = match frames {
                    Some(_) => None,
                    None => (1..=0x8000_0000).contains(&count).then_some(count),
                };
            }
            b"iCCP" if profile && length <= 0x7FFF_FFFF => {
                left = 0;
                if crc_holds(file, &head[4..], length.into())? {
                    profile = false;
                    file.seek_relative(-(i64::from(length) + 4))?; // back to its data
                    let mut data = file.by_ref().take(length.into());
                    if profile_too_long(&mut data)? {
                        return Err(Not::Image);
                    }
                    left = data.limit() + 4;
                }
            }
            _ => {}
        }
        file.seek_relative(left as i64)?; // within 2^32 + 4
    }
}

/// Whether the CRC of a chunk of `kind` holds over its kind and the `length` bytes of data the reader stands at; the reader then stands at the next chunk.
fn crc_holds(file: &mut impl BufRead, kind: &[u8], length: u64) -> std::io::Result<bool> {
    let mut crc = flate2::Crc::new();
    crc.update(kind);
    let mut data = file.by_ref().take(length);
    loop {
        let bytes = data.fill_buf()?;
        let read = bytes.len();
        if read == 0 {
            break;
        }
        crc.update(bytes);
        data.consume(read);
    }
    let mut sum = [0; 4];
    match data.limit() {
        0 => file.read_exact(&mut sum)?,
        _ => return Err(std::io::ErrorKind::UnexpectedEof.into()), // cut short
    }
    Ok(crc.sum() == u32::from_be_bytes(sum))
}

/// Whether an iCCP chunk's data holds a profile that inflates past `MAX_TEXT_CHUNK`: the profile's name ended by a NUL within 81 bytes (PNG's names hold 1 to 79), the method, 0 for zlib, then the stream, inflated no further than that; a stream that fails to inflate holds no profile, to Pillow and to the png crate.
fn profile_too_long(mut data: impl BufRead) -> std::io::Result<bool> {
    let (mut name, mut method) = (Vec::new(), [1]);
    data.by_ref().take(81).read_until(0, &mut name)?;
    if name.last() != Some(&0) || data.read(&mut method)? == 0 || method != [0] {
        return Ok(false); // nothing to inflate
    }
    let mut profile = flate2::bufread::ZlibDecoder::new(data).take(MAX_TEXT_CHUNK + 1);
    let inflated = std::io::copy(&mut profile, &mut std::io::sink());
    Ok(inflated.is_ok_and(|bytes| bytes > MAX_TEXT_CHUNK))
}

/// A QOI's one frame, its header measured before the decoder, which refuses one over 400 megapixels as no picture, where Pillow 13 measures it (QoiImagePlugin _open, then Image.open's measure): over `MAX_PIXELS`, `Not::TooLarge`; without QOI's signature (a QOI by its suffix alone), `Not::Image`.
fn qoi_frames(file: &mut impl Read) -> std::result::Result<u32, Not> {
    let mut head = [0; 12]; // qoif, the width, the height
    file.read_exact(&mut head)?;
    if !head.starts_with(b"qoif") {
        return Err(Not::Image); // a QOI by its suffix alone
    }
    let side = |at: usize| u32::from_be_bytes([head[at], head[at + 1], head[at + 2], head[at + 3]]);
    match too_large((side(4), side(8))) {
        true => Err(Not::TooLarge),
        false => Ok(1),
    }
}

/// A walk's reader at places counted from the file's start, moved there by relative seeks: they keep a `BufReader`'s buffer where it reaches, where a seek from the start drops it, a refill a chunk.
struct Walk<'a, R> {
    file: &'a mut R,
    at: Option<u64>, // where the reader stands; `None` after a read that failed
}

impl<R: Read + Seek> Walk<'_, R> {
    /// A walk of `file`, and the file's length.
    fn new(file: &mut R) -> std::io::Result<(Walk<'_, R>, u64)> {
        let end = file.seek(SeekFrom::End(0))?;
        let at = Some(end);
        Ok((Walk { file, at }, end))
    }

    /// `bytes` read from `at`, which lies within 2^62.
    fn read(&mut self, at: u64, bytes: &mut [u8]) -> std::io::Result<()> {
        match self.at.take() {
            Some(now) => self.file.seek_relative(at as i64 - now as i64)?,
            None => drop(self.file.seek(SeekFrom::Start(at))?),
        }
        self.file.read_exact(bytes)?;
        self.at = Some(at + bytes.len() as u64);
        Ok(())
    }
}

/// The box at `at` within a parent that ends at `end`: its kind, where its body starts and where it ends by its size, which can lie past `end`; `None` where no box fits.
fn boxed(
    walk: &mut Walk<'_, impl Read + Seek>,
    at: u64,
    end: u64,
) -> std::io::Result<Option<([u8; 4], u64, u64)>> {
    if at.saturating_add(8) > end {
        return Ok(None);
    }
    let mut head = [0; 8];
    walk.read(at, &mut head)?;
    let kind = [head[4], head[5], head[6], head[7]];
    let (size, body) = match u32::from_be_bytes([head[0], head[1], head[2], head[3]]) {
        0 => (end - at, at + 8), // to the end
        1 => {
            let mut large = [0; 8];
            walk.read(at + 8, &mut large)?;
            (u64::from_be_bytes(large), at + 16)
        }
        size => (u64::from(size), at + 8),
    };
    Ok((size >= body - at).then_some((kind, body, at.saturating_add(size))))
}

/// An AVIF's frames, up to 2, as libavif counts them for Pillow 13 (AvifImagePlugin n_frames): where the major brand is not avif and moov holds a track, the samples of its first AV1 track that is no other's auxiliary, else 1, the primary item; measured by the size its container declares, the primary item's first ispe property (pitm, ipma, ipco), as Pillow 13 measures one (libavif's parse), before the decoder, which learns a size only from the picture dav1d decodes whole: over `MAX_PIXELS`, `Not::TooLarge`; the decoded picture's own size is measured after it. `Not::Image` for a file whose first box is no ftyp, whole and of brands in fours, or whose brands hold no AVIF brand (avif, avis), which mp4parse and libavif refuse; one of a major brand Pillow does not take (AvifImagePlugin _accept: avif, avis, mif1, msf1), the decoder's to tell after its declared size is measured too (N7).
fn avif_frames(file: &mut (impl Read + Seek)) -> std::result::Result<u32, Not> {
    let (mut walk, end) = Walk::new(file)?;
    let Some((kind, body, stop)) = boxed(&mut walk, 0, end)? else {
        return Err(Not::Image);
    };
    if &kind != b"ftyp" || stop > end || stop < body + 8 || (stop - body) % 4 != 0 {
        return Err(Not::Image); // the major brand, the minor version, the compatible brands
    }
    let (mut major, mut brand, mut avif) = ([0; 4], [0; 4], false);
    walk.read(body, &mut major)?;
    for at in (body + 8..stop).step_by(4) {
        walk.read(at, &mut brand)?;
        avif |= matches!(&brand, b"avif" | b"avis");
    }
    if !avif && !matches!(&major, b"avif" | b"avis") {
        return Err(Not::Image);
    }
    let (mut at, mut meta, mut moov) = (stop, None, None);
    while let Some((kind, body, next)) = boxed(&mut walk, at, end)? {
        match &kind {
            b"meta" => meta = meta.or(Some((body + 4, next.min(end)))), // past its version and flags
            b"moov" => moov = moov.or(Some((body, next.min(end)))),
            _ => {}
        }
        at = next;
    }
    let size = match meta {
        Some(meta) => declared(&mut walk, meta)?,
        None => None,
    };
    if size.is_some_and(too_large) {
        return Err(Not::TooLarge);
    }
    if !matches!(&major, b"avif" | b"avis" | b"mif1" | b"msf1") {
        return Ok(1);
    }
    let samples = match moov {
        Some(moov) if &major != b"avif" => samples(&mut walk, moov)?, // libavif takes the track
        _ => None,
    };
    Ok(samples.map_or(1, |count| count.clamp(1, 2)))
}

/// The size an AVIF's meta box, from `at` to `end` past its version and flags, declares: its primary item's first ispe property, as libavif takes it, by its primary item (pitm), each item's properties by their places (each of iprp's ipma boxes) and the properties (iprp's ipco).
fn declared(
    walk: &mut Walk<'_, impl Read + Seek>,
    (mut at, end): (u64, u64),
) -> std::io::Result<Option<(u32, u32)>> {
    let (mut primary, mut iprp) = (None, None);
    while let Some((kind, body, next)) = boxed(walk, at, end)? {
        let next = next.min(end);
        if &kind == b"pitm" {
            let mut head = [0; 6]; // the version and flags, the item's first 2 bytes: all of it in version 0, else half
            walk.read(body, &mut head)?;
            let mut item = u32::from(u16::from_be_bytes([head[4], head[5]]));
            if head[0] > 0 {
                let mut low = [0; 2];
                walk.read(body + 6, &mut low)?;
                item = item << 16 | u32::from(u16::from_be_bytes(low));
            }
            primary = primary.or(Some(item));
        }
        if &kind == b"iprp" {
            iprp = iprp.or(Some((body, next)));
        }
        at = next;
    }
    let (Some(primary), Some((mut at, end))) = (primary, iprp) else {
        return Ok(None);
    };
    let (mut places, mut ipco) = (Vec::new(), None);
    while let Some((kind, body, next)) = boxed(walk, at, end)? {
        match &kind {
            b"ipma" => places.extend(associations(walk, body, next.min(end), primary)?),
            b"ipco" => ipco = ipco.or(Some((body, next.min(end)))),
            _ => {}
        }
        at = next;
    }
    let Some((mut at, last)) = ipco else {
        return Ok(None);
    };
    // of the primary item's properties, the first, in their order, that is an ispe
    let mut sides = vec![None; places.len()];
    let mut place = 1; // the properties are counted from 1
    while let Some((kind, body, next)) = boxed(walk, at, last)? {
        if &kind == b"ispe" && next.min(last) >= body + 12 && places.contains(&place) {
            let mut ispe = [0; 12]; // the version and flags, the width, the height
            walk.read(body, &mut ispe)?;
            let side = |at: usize| {
                u32::from_be_bytes([ispe[at], ispe[at + 1], ispe[at + 2], ispe[at + 3]])
            };
            for (named, seen) in places.iter().zip(&mut sides) {
                if *named == place {
                    *seen = Some((side(4), side(8)));
                }
            }
        }
        (at, place) = (next, place + 1);
    }
    Ok(sides.into_iter().flatten().next())
}

/// What a track's boxes tell of it: whether it holds AV1 (an av01 sample entry in stsd), whether it names another as the one it is the auxiliary of (tref's auxl, as an alpha plane), and its samples (stsz's count).
#[derive(Default)]
struct Track {
    av1: bool,
    auxiliary: bool,
    samples: Option<u32>,
}

/// The samples of the first track in moov's boxes, from `at` to `end`, that holds AV1 and is no other's auxiliary, as libavif takes a sequence's colour track.
fn samples(
    walk: &mut Walk<'_, impl Read + Seek>,
    (mut at, end): (u64, u64),
) -> std::io::Result<Option<u32>> {
    while let Some((kind, body, next)) = boxed(walk, at, end)? {
        let next = next.min(end);
        if &kind == b"trak" {
            let mut track = Track::default();
            told(walk, (body, next), 0, &mut track)?;
            if track.av1 && !track.auxiliary && track.samples.is_some() {
                return Ok(track.samples);
            }
        }
        at = next;
    }
    Ok(None)
}

/// Into `track`, what its boxes from `at` to `end`, `depth` boxes down from trak, tell: down by tref, and by mdia, minf and stbl to stsd and stsz (a full box each: its version and flags, then stsd's count of entries and its first entry's size and kind, stsz's size of every sample and its count).
fn told(
    walk: &mut Walk<'_, impl Read + Seek>,
    (mut at, end): (u64, u64),
    depth: u8,
    track: &mut Track,
) -> std::io::Result<()> {
    while let Some((kind, body, next)) = boxed(walk, at, end)? {
        let next = next.min(end);
        let mut field = [0; 4];
        match (&kind, depth) {
            (b"tref" | b"mdia", 0) | (b"minf", 1) | (b"stbl", 2) => {
                told(walk, (body, next), depth + 1, track)?
            }
            (b"auxl", 1) => track.auxiliary = true,
            (b"stsd", 3) if next >= body + 16 => {
                walk.read(body + 12, &mut field)?;
                track.av1 |= &field == b"av01";
            }
            (b"stsz", 3) if next >= body + 12 => {
                walk.read(body + 8, &mut field)?;
                track.samples = track.samples.or(Some(u32::from_be_bytes(field)));
            }
            _ => {}
        }
        at = next;
    }
    Ok(())
}

/// The places among ipco's properties, counted from 1, of the properties that `item`'s entry in the ipma box from `body` to `end` names, in its order; none where it names none. ipma: its version and flags, the count of entries, then each entry's item (2 bytes in version 0, else 4), the count of its properties and their places (1 byte each, the top bit a flag, or 2 bytes with flag 1).
fn associations(
    walk: &mut Walk<'_, impl Read + Seek>,
    body: u64,
    end: u64,
    item: u32,
) -> std::io::Result<Vec<u64>> {
    let mut head = [0; 8];
    walk.read(body, &mut head)?;
    let id = if head[0] == 0 { 2 } else { 4 }; // an item's bytes
    let wide = head[3] & 1 == 1; // 2 bytes a place
    let (mut at, mut entries) = (
        body + 8,
        u32::from_be_bytes([head[4], head[5], head[6], head[7]]),
    );
    while entries > 0 && at < end {
        let mut entry = [0; 5]; // the item, the count of its properties
        walk.read(at, &mut entry[4 - id..])?;
        let (named, count) = (
            u32::from_be_bytes([entry[0], entry[1], entry[2], entry[3]]),
            entry[4],
        );
        let mut places = vec![0; usize::from(count) << u8::from(wide)];
        at += id as u64 + 1;
        if named == item {
            walk.read(at, &mut places)?;
            return Ok(match wide {
                true => places
                    .chunks(2)
                    .map(|place| u64::from(u16::from_be_bytes([place[0], place[1]]) & 0x7FFF))
                    .collect(),
                false => places.iter().map(|place| u64::from(place & 0x7F)).collect(),
            });
        }
        at += places.len() as u64;
        entries -= 1;
    }
    Ok(Vec::new())
}

/// A WebP's frames as libwebp counts a well-formed one's for Pillow: the ANMF chunks behind a VP8X with the animation flag, up to 2; 1 for any other WebP; libwebp's demuxer leaves out a frame without an image and refuses a file cut short, so a damaged WebP can count otherwise in Pillow (N6, N7). An error for a VP8X whose EXIF, ICCP or XMP chunk runs past the file's end: the decoder takes the first such chunk of each kind whole by the size it declares, with its flag or without, up to 4 GiB allocated before the read fails (the EXIF for the orientation); Pillow's libwebp refuses such a file, or opens it where the chunk lies past the RIFF's end, a damaged WebP's verdict (N6, N7); the chunks walked as image-webp walks them, to 12 bytes short of the RIFF's length past VP8X, so past the RIFF's end. Then `Not::TooLarge` for a VP8X canvas over `MAX_PIXELS`, as Pillow measures libwebp's canvas, where image-webp gives up on one over 2^32 px as no picture; without RIFF's and WebP's signatures (a WebP by its suffix alone), `Not::Image`.
fn webp_frames(file: &mut (impl Read + Seek)) -> std::result::Result<u32, Not> {
    let mut head = [0; 30]; // RIFF, its length, WEBP, the first chunk's kind and length; VP8X's flags, 3 reserved bytes, the canvas's width - 1 and height - 1
    file.read_exact(&mut head[..21])?;
    if &head[..4] != b"RIFF" || &head[8..12] != b"WEBP" {
        return Err(Not::Image); // a WebP by its suffix alone
    }
    if &head[12..16] != b"VP8X" {
        return Ok(1);
    }
    file.read_exact(&mut head[21..])?;
    let side = |at: usize| u32::from_le_bytes([head[at], head[at + 1], head[at + 2], 0]) + 1;
    let canvas = (side(24), side(27));
    let (mut walk, end) = Walk::new(file)?;
    let riff = u32::from_le_bytes([head[4], head[5], head[6], head[7]]);
    let length = u32::from_le_bytes([head[16], head[17], head[18], head[19]]);
    let mut at = 20 + u64::from(length) + u64::from(length % 2); // chunks are padded to even
    let last = at + u64::from(riff).saturating_sub(12);
    let (mut frames, mut chunk) = (0, [0; 8]);
    while at < last {
        if walk.read(at, &mut chunk).is_err() {
            break;
        }
        let length = u64::from(u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]));
        if matches!(&chunk[..4], b"EXIF" | b"ICCP" | b"XMP ") && at + 8 + length > end {
            return Err(Not::Image);
        }
        frames += u32::from(&chunk[..4] == b"ANMF");
        at += 8 + length + length % 2;
    }
    if too_large(canvas) {
        return Err(Not::TooLarge);
    }
    Ok(match head[20] & 0x02 {
        0 => 1, // no animation
        _ => frames.clamp(1, 2),
    })
}

/// A BMP's one frame, its header measured before the decoder (`dib`); without BMP's signature (a BMP by its suffix alone), `Not::Image`.
fn bmp_frames(file: &mut impl Read) -> std::result::Result<u32, Not> {
    let mut head = [0; 14]; // BM, the file's size, two reserved, the pixels' place
    file.read_exact(&mut head)?;
    if !head.starts_with(b"BM") {
        return Err(Not::Image); // a BMP by its suffix alone
    }
    dib(file)
}

/// The DIB header the reader stands at, its sides read as Pillow 13 reads them (BmpImagePlugin _bitmap: 16 bits each in a header of 12 bytes, else the width unsigned and the height unsigned but for one whose top byte is 0xFF, a top-down picture's) and measured before the decoder, which gives up on a side over 65535 px, a negative width and the least height as no picture: over `MAX_PIXELS`, `Not::TooLarge`; a header Pillow does not know is left to the decoder. An icon's BMP is measured so too, its height twice the picture's, the AND mask's with it, as Pillow measures it (IcoImagePlugin IcoFile.frame).
fn dib(file: &mut impl Read) -> std::result::Result<u32, Not> {
    let mut header = [0; 12]; // its size, the width, the height
    file.read_exact(&mut header)?;
    let field = |at: usize| {
        u32::from_le_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]])
    };
    let half = |at: usize| u32::from(u16::from_le_bytes([header[at], header[at + 1]]));
    let size = match field(0) {
        12 => (half(4), half(6)),
        40 | 52 | 56 | 64 | 108 | 124 => match field(8) {
            height if height >> 24 == 0xFF => (field(4), height.wrapping_neg()),
            height => (field(4), height),
        },
        _ => return Ok(1),
    };
    match too_large(size) {
        true => Err(Not::TooLarge),
        false => Ok(1),
    }
}

/// An ICO's one frame, its directory read as the decoder reads it: 6 bytes, then 16 an entry, an entry of over 256 planes or bits refused; the entry the decoder takes, the most bits a pixel, then the most pixels by the directory, the last entry winning ties (image's ico best_entry), walked from its place: a PNG as `png_frames` walks one, its profile bounded and its header measured, as the decoder reads such a PNG by the png crate without limits.
fn ico_frames(file: &mut (impl BufRead + Seek)) -> std::result::Result<u32, Not> {
    let mut head = [0; 6]; // two reserved bytes, the type, the count of entries
    file.read_exact(&mut head)?;
    let (mut best, mut last) = (None, None); // the first best of the entries before the last; the last
    for _ in 0..u16::from_le_bytes([head[4], head[5]]) {
        let mut entry = [0; 16]; // the sides, colours, a reserved byte, planes, bits, bytes, place
        file.read_exact(&mut entry)?;
        let field = |at: usize| u16::from_le_bytes([entry[at], entry[at + 1]]);
        if field(4) > 256 || field(6) > 256 {
            return Err(Not::Image); // as the decoder refuses it
        }
        let side = |byte: u8| if byte == 0 { 256 } else { u32::from(byte) };
        let score = (field(6), side(entry[0]) * side(entry[1]));
        let place = u32::from_le_bytes([entry[12], entry[13], entry[14], entry[15]]);
        if let Some(before) = last.replace((score, place))
            && best.is_none_or(|(most, _)| before.0 > most)
        {
            best = Some(before);
        }
    }
    let Some(last) = last else {
        return Err(Not::Image); // no entry
    };
    let (_, place) = best.filter(|(score, _)| *score > last.0).unwrap_or(last);
    file.seek(SeekFrom::Start(place.into()))?;
    let mut signature = [0; 8];
    file.read_exact(&mut signature)?;
    file.seek_relative(-8)?;
    match &signature {
        b"\x89PNG\r\n\x1a\n" => png_frames(file).map(|_| 1), // an icon's picture, its animation none
        _ => dib(file),
    }
}

/// Bytes a block of an EXR may hold: a picture of `MAX_PIXELS` in four float channels.
const MAX_BLOCK: u64 = MAX_PIXELS * 16;

/// An EXR's parts measured: each one's data window, the pixels its blocks hold, over `MAX_PIXELS`, `Not::TooLarge`, where the decoder's size is the display window, the picture's; a block, its lines or its tile within the data window, in all the channels' bytes over `MAX_BLOCK`, `Not::Image`. The exr crate allocates a block's pixels so before it inflates them. Each part's attributes taken as the exr crate takes them, the last of a name with its kind: dataWindow (box2i), channels (chlist: each a name, a type, 2 bytes a pixel for half, else 4, and 12 bytes more), compression (its lines a block: 1, 16, 32 or 256), tiles (tiledesc: the tile's sides) where the part is tiled (its type, or the version's flag of a single tiled part). Walked once the exr crate has read the headers whole, so as it reads them, its refusals its own: the magic number, the version and flags (long names: 256 bytes a name at most, else 32; several parts: a header each, then a NUL), then each header's attributes, a NUL ending them: a name and a kind, each ended by a NUL, the value's size, the value.
fn exr_measured(path: &Path) -> std::result::Result<(), Not> {
    fn byte(file: &mut impl Read) -> std::io::Result<u8> {
        let mut byte = [0];
        file.read_exact(&mut byte).map(|()| byte[0])
    }
    // a NUL next: the end of a header or of the headers
    fn ends(file: &mut impl BufRead) -> std::io::Result<bool> {
        let end = file.fill_buf()?.first() == Some(&0);
        file.consume(usize::from(end));
        Ok(end)
    }
    // its first byte whatever it is, as the exr crate reads a text
    fn text(file: &mut impl Read, most: usize) -> std::result::Result<Vec<u8>, Not> {
        let mut text = vec![byte(file)?];
        loop {
            match byte(file)? {
                0 => return Ok(text),
                next if text.len() < most => text.push(next),
                _ => return Err(Not::Image),
            }
        }
    }
    let mut file = BufReader::new(File::open(path)?);
    let mut head = [0; 8];
    file.read_exact(&mut head)?;
    let flags = head[5]; // flag bits 9 to 12: a single tiled part, long names, deep data, several parts
    let (most, parts) = (if flags & 0x04 > 0 { 256 } else { 32 }, flags & 0x10 > 0);
    loop {
        if parts && ends(&mut file)? {
            return Ok(());
        }
        let (mut window, mut bytes, mut lines, mut tile, mut tiled) = ((1, 1), 0, 1, None, None);
        while !ends(&mut file)? {
            let (name, kind) = (text(&mut file, most)?, text(&mut file, most)?);
            let mut size = [0; 4];
            file.read_exact(&mut size)?;
            let mut value = file.by_ref().take(u32::from_le_bytes(size).into());
            let mut field = [0; 16];
            match (&name[..], &kind[..]) {
                (b"dataWindow", b"box2i") => {
                    value.read_exact(&mut field)?; // xMin, yMin, xMax, yMax
                    let at = |at: usize| {
                        i64::from(i32::from_le_bytes([
                            field[at],
                            field[at + 1],
                            field[at + 2],
                            field[at + 3],
                        ]))
                    };
                    let side = |a: i64, b: i64| (a.max(b) - a.min(b) + 1) as u64;
                    window = (side(at(0), at(8)), side(at(4), at(12)));
                }
                (b"channels", b"chlist") => {
                    bytes = 0;
                    loop {
                        let mut channel = Vec::new();
                        value.by_ref().take(257).read_until(0, &mut channel)?;
                        if channel.len() < 2 {
                            break; // a NUL alone: the list's end
                        }
                        value.read_exact(&mut field)?; // its type, then whether linear, 3 reserved bytes, its sampling
                        bytes += if field[..4] == [1, 0, 0, 0] { 2 } else { 4 };
                    }
                }
                (b"compression", b"compression") => {
                    value.read_exact(&mut field[..1])?;
                    lines = match field[0] {
                        0..=2 => 1,
                        3 | 5 => 16,
                        9 | 10 => 256,
                        _ => 32,
                    };
                }
                (b"tiles", b"tiledesc") => {
                    value.read_exact(&mut field[..8])?; // the tile's width and height, then its levels
                    let side = |at: usize| {
                        u64::from(u32::from_le_bytes([
                            field[at],
                            field[at + 1],
                            field[at + 2],
                            field[at + 3],
                        ]))
                    };
                    tile = Some((side(0), side(4)));
                }
                (b"type", b"string") => {
                    let mut named = Vec::new();
                    value.by_ref().take(64).read_to_end(&mut named)?;
                    tiled = Some(named == b"tiledimage" || named == b"deeptile");
                }
                _ => {}
            }
            let left = value.limit();
            file.seek_relative(left as i64)?; // within 2^32
        }
        let (w, h) = window;
        if w * h > MAX_PIXELS {
            return Err(Not::TooLarge);
        }
        let block = match (tiled.unwrap_or(flags & 0x02 > 0), tile) {
            (true, Some((across, down))) => across.min(w) * down.min(h),
            _ => w * lines.min(h),
        };
        if block.saturating_mul(bytes) > MAX_BLOCK {
            return Err(Not::Image);
        }
        if !parts {
            return Ok(());
        }
    }
}

/// The decoder of the raster still at `path`, its header read: the format by content, within `MAX_PIXELS`, no animation; in Pillow's order (Image.open, then lib/ascii_image.py:61): the file read to its first frame, then its size, then its frames; a GIF measured by its blocks before the decoder reads it, whose refusals Pillow does not share; an EXR by its data windows and blocks too, once the decoder has read its headers.
fn header(path: &Path) -> std::result::Result<impl ImageDecoder, Not> {
    let mut reader = ImageReader::open(path)
        .and_then(ImageReader::with_guessed_format)
        .map_err(|_| Not::Image)?;
    reader.no_limits(); // MAX_PIXELS is the limit
    let format = reader.format();
    let frames = frames(path, format)?;
    let decoder = reader.into_decoder().map_err(|_| Not::Image)?;
    if too_large(decoder.dimensions()) {
        return Err(Not::TooLarge);
    }
    if format == Some(ImageFormat::OpenExr) {
        exr_measured(path)?;
    }
    match frames {
        1 => Ok(decoder),
        _ => Err(Not::Animation),
    }
}

/// The raster still at `path` as its decoder gives it, upright by its EXIF orientation.
fn upright(path: &Path) -> std::result::Result<DynamicImage, Not> {
    let mut decoder = header(path)?;
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut image = DynamicImage::from_decoder(decoder).map_err(|_| Not::Image)?;
    image.apply_orientation(orientation);
    Ok(image)
}

/// 8 bits a channel; RGBA only when some alpha is under 255.
fn pic(image: DynamicImage) -> Pic {
    if !image.color().has_alpha() {
        return Pic::Rgb(image.into_rgb8());
    }
    let image = image.into_rgba8();
    if image.pixels().any(|p| p[3] < 255) {
        Pic::Rgba(image)
    } else {
        Pic::Rgb(DynamicImage::ImageRgba8(image).into_rgb8())
    }
}

/// Open the still at `path` (rows S1-S3, S18); a decoder panic counts as `NotImage`.
pub fn open(path: &Path) -> Opened {
    let still = || -> std::result::Result<Still, Not> {
        if !is_svg(path) {
            let pic = pic(upright(path)?);
            return Ok(Still { pic, svg: None });
        }
        let svg = Svg::load(path).map_err(|_| Not::Image)?;
        let (size, scale) = svg.fit(Some(SVG_WIDTH)).ok_or(Not::Image)?;
        if too_large(size) {
            return Err(Not::TooLarge);
        }
        let raster = svg.draw(size, scale).map_err(|_| Not::Image)?;
        let pic = pic(DynamicImage::ImageRgba8(raster));
        let svg = Some(Arc::new(svg));
        Ok(Still { pic, svg })
    };
    match caught(still) {
        Some(Ok(still)) => Opened::Still(still),
        Some(Err(Not::Animation)) => Opened::Animation,
        Some(Err(Not::TooLarge)) => {
            let name = path.file_name().unwrap_or(path.as_os_str());
            let (name, megapixels) = (name.to_string_lossy(), MAX_PIXELS as f64 / 1e6);
            Opened::TooLarge(format!(
                "{name} is too large to open (over {megapixels:.0} megapixels)"
            ))
        }
        Some(Err(Not::Image)) | None => Opened::NotImage,
    }
}

/// Whether `path` holds a still by its header, a GIF's blocks, an APNG's or a WebP's chunks (an SVG by its suffix; an image over the limit counts).
pub fn identifies(path: &Path) -> bool {
    let raster = || caught(|| header(path).map(drop));
    is_svg(path) || matches!(raster(), Some(Ok(()) | Err(Not::TooLarge)))
}

pub fn is_svg(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("svg"))
}

/// Whether the folder listing takes `path` by its extension (the built-in decoders' extensions and `svg`).
pub fn listed(path: &Path) -> bool {
    const EXTENSIONS: &[&str] = &[
        "jpg", "jpeg", "jpe", "jfif", "mpo", "png", "apng", "gif", "webp", "bmp", "dib", "tif",
        "tiff", "ico", "cur", "tga", "icb", "vda", "vst", "qoi", "pbm", "pgm", "ppm", "pnm", "pfm",
        "dds", "hdr", "exr", "avif", "svg",
    ];
    path.extension()
        .is_some_and(|e| EXTENSIONS.iter().any(|known| e.eq_ignore_ascii_case(known)))
}

/// `picture` over the ground, at its size: each channel blended through the alpha and rounded, as Pillow pastes.
fn over(picture: &RgbaImage, ground: Ground) -> RgbImage {
    let (w, h) = picture.dimensions();
    let mut out = match ground {
        Ground::Colour(colour) => RgbImage::from_pixel(w, h, image::Rgb(colour)),
        Ground::Checker => checkerboard(w, h),
    };
    for (under, top) in out.pixels_mut().zip(picture.pixels()) {
        let alpha = top[3] as u32;
        for (under, &top) in under.0.iter_mut().zip(&top.0[..3]) {
            let blend = *under as u32 * (255 - alpha) + top as u32 * alpha;
            *under = ((blend + 127) / 255) as u8; // rounded
        }
    }
    out
}

/// The picture as RGB, its transparency over `background`.
pub fn flatten(pic: &Pic, background: Rgb) -> RgbImage {
    match pic {
        Pic::Rgb(image) => image.clone(),
        Pic::Rgba(image) => over(image, Ground::Colour(background)),
    }
}

/// The picture turned `quarter_turns` clockwise.
pub fn turned(pic: &Pic, quarter_turns: u8) -> Pic {
    fn turn<P: Pixel<Subpixel = u8> + 'static>(
        image: &ImageBuffer<P, Vec<u8>>,
        turns: u8,
    ) -> ImageBuffer<P, Vec<u8>> {
        match turns % 4 {
            0 => image.clone(),
            1 => image::imageops::rotate90(image),
            2 => image::imageops::rotate180(image),
            _ => image::imageops::rotate270(image),
        }
    }
    match pic {
        Pic::Rgb(image) => Pic::Rgb(turn(image, quarter_turns)),
        Pic::Rgba(image) => Pic::Rgba(turn(image, quarter_turns)),
    }
}

/// What shows under transparency.
pub enum Ground {
    Colour(Rgb),
    /// 8 px squares of #666666 and #999999, anchored at the top left.
    Checker,
}

/// One side of a box as (start, length), kept within 0..`max`.
fn span(from: f64, to: f64, max: u32) -> (f64, f64) {
    let from = from.max(0.0).min(max as f64);
    (from, to.max(from).min(max as f64) - from)
}

/// The box of `image` resampled to `size`.
fn scaled<P: Pixel<Subpixel = u8>>(
    image: &ImageBuffer<P, Vec<u8>>,
    bbox: [f64; 4],
    size: (u32, u32),
    how: fir::ResizeAlg,
) -> ImageBuffer<P, Vec<u8>> {
    let (w, h) = image.dimensions();
    let kind = match P::CHANNEL_COUNT {
        4 => fir::PixelType::U8x4,
        _ => fir::PixelType::U8x3,
    };
    let ((left, width), (top, height)) = (span(bbox[0], bbox[2], w), span(bbox[1], bbox[3], h));
    let options = fir::ResizeOptions::new()
        .resize_alg(how)
        .crop(left, top, width, height);
    let mut out = ImageBuffer::new(size.0, size.1);
    if let (Ok(from), Ok(mut to)) = (
        fir::images::ImageRef::new(w, h, image.as_raw(), kind),
        fir::images::Image::from_slice_u8(size.0, size.1, &mut out, kind),
    ) {
        // nothing left to refuse: one pixel type, the box inside the picture
        let _ = fir::Resizer::new().resize(&from, &mut to, &options);
    }
    out
}

/// The box (left, top, right, bottom in the picture's px, fractions allowed) scaled to `size`: nearest neighbour when `nearest`, bicubic otherwise; transparency over `ground`.
pub fn cut(pic: &Pic, bbox: [f64; 4], size: (u32, u32), nearest: bool, ground: Ground) -> RgbImage {
    let how = if nearest {
        fir::ResizeAlg::Nearest
    } else {
        fir::ResizeAlg::Convolution(fir::FilterType::CatmullRom)
    };
    match pic {
        Pic::Rgb(image) => scaled(image, bbox, size, how),
        // over the ground after scaling: the squares keep their size and place (row K2)
        Pic::Rgba(image) => over(&scaled(image, bbox, size, how), ground),
    }
}

/// The ground under transparency in image mode: 8 px squares, #666666 where (x / 8 + y / 8) is even, #999999 where odd.
pub fn checkerboard(w: u32, h: u32) -> RgbImage {
    RgbImage::from_fn(w, h, |x, y| match (x / 8 + y / 8) % 2 {
        0 => image::Rgb([0x66; 3]),
        _ => image::Rgb([0x99; 3]),
    })
}

/// The picture reduced by a box filter to `w` x `h`.
pub fn resize_box(picture: &RgbImage, w: u32, h: u32) -> RgbImage {
    let (pw, ph) = (picture.width() as usize, picture.height() as usize);
    let pixels = box_filter(picture.as_raw(), pw, ph, w as usize, h as usize);
    RgbImage::from_raw(w, h, pixels).expect("sized buffer")
}

/// Whether Pillow 13 resizes `w` x `h` to `ow` x `oh` along y first, as the Python tools ran it: when the height shrinks by more than twice what the width does (libImaging/Resample.c, ImagingResampleInner; Pillow 12 always went along x first). Each pass rounds, so the order shows in the result.
pub fn vertical_first(w: usize, h: usize, ow: usize, oh: usize) -> bool {
    h > oh && (h - oh) as i64 > (w as i64 - ow as i64) * 2
}

/// Packed RGB of `w` x `h` reduced or enlarged to `ow` x `oh` by a box filter, bit for bit as Pillow's BOX: along one axis, then the other (`vertical_first`), each pass in 22-bit fixed point rounded to 8 bits; a pass whose size does not change is skipped.
pub fn box_filter(rgb: &[u8], w: usize, h: usize, ow: usize, oh: usize) -> Vec<u8> {
    const BITS: u32 = 22;
    // per output sample: the first source sample it covers, how many, and the weight of each
    fn spans(size: usize, out: usize) -> Vec<(usize, usize, i32)> {
        let scale = size as f64 / out as f64;
        let span = scale.max(1.0);
        let covered = |at: usize| {
            let centre = (at as f64 + 0.5) * scale;
            let from = ((centre - span / 2.0 + 0.5) as i64).max(0) as usize;
            let to = ((centre + span / 2.0 + 0.5) as i64).min(size as i64) as usize;
            let inside = |x: &usize| {
                let x = (*x as f64 - centre + 0.5) / span;
                x > -0.5 && x <= 0.5
            };
            let first = (from..to).find(inside).unwrap_or(from);
            let count = (first..to).take_while(inside).count();
            let weight = (1 << BITS) as f64 / count as f64 + 0.5;
            (first, count, weight as i32)
        };
        (0..out).map(covered).collect()
    }
    let mean = |sum: i32, weight: i32| ((sum * weight + (1 << (BITS - 1))) >> BITS).min(255) as u8;
    let across = |pixels: &[u8]| {
        let (columns, mut out) = (spans(w, ow), Vec::with_capacity(pixels.len() / w * ow));
        for row in pixels.chunks_exact(w * 3) {
            for &(first, count, weight) in &columns {
                let covered = row[first * 3..(first + count) * 3].as_chunks::<3>().0;
                let sums = covered
                    .iter()
                    .fold([0; 3], |sums, p| [0, 1, 2].map(|c| sums[c] + p[c] as i32));
                out.extend(sums.map(|sum| mean(sum, weight)));
            }
        }
        out
    };
    let down = |pixels: &[u8], width: usize| {
        let (line, mut out) = (width * 3, Vec::with_capacity(width * oh * 3));
        let mut sums = vec![0; line];
        for (first, count, weight) in spans(h, oh) {
            sums.fill(0);
            for row in pixels[first * line..(first + count) * line].chunks_exact(line) {
                sums.iter_mut()
                    .zip(row)
                    .for_each(|(sum, &value)| *sum += value as i32);
            }
            out.extend(sums.iter().map(|&sum| mean(sum, weight)));
        }
        out
    };
    match (ow != w, oh != h) {
        (false, false) => rgb.to_vec(),
        (true, false) => across(rgb),
        (false, true) => down(rgb, w),
        _ if vertical_first(w, h, ow, oh) => across(&down(rgb, w)),
        _ => down(&across(rgb), ow),
    }
}

/// The width to rasterize an SVG of `size` at for `want` px across: `want`, as long as its long side stays within `SVG_MAX`.
pub fn raster_width(want: u32, size: (u32, u32)) -> u32 {
    want.min((SVG_MAX as u64 * size.0 as u64 / size.0.max(size.1) as u64).max(1) as u32)
}

/// A parsed SVG, rasterized on demand.
pub struct Svg {
    tree: usvg::Tree,
}

/// The system's fonts, loaded once a process: only an SVG with text needs them.
fn fonts() -> Arc<usvg::fontdb::Database> {
    static FONTS: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    let load = || {
        let mut all = usvg::fontdb::Database::new();
        all.load_system_fonts();
        Arc::new(all)
    };
    FONTS.get_or_init(load).clone()
}

/// The whole px that hold a size, as the resvg program sizes its picture.
fn px(size: tiny_skia::Size) -> (u32, u32) {
    (size.width().ceil() as u32, size.height().ceil() as u32)
}

impl Svg {
    /// Parsed as the resvg program parses: gzip unpacked, links relative to the file's folder, fonts loaded only for text.
    pub fn load(path: &Path) -> Result<Svg> {
        let mut data = std::fs::read(path)?;
        if data.starts_with(&[0x1f, 0x8b]) {
            data = usvg::decompress_svgz(&data)?;
        }
        let parsing = usvg::roxmltree::ParsingOptions {
            allow_dtd: true,
            ..Default::default()
        };
        let text = std::str::from_utf8(&data)?;
        let xml = usvg::roxmltree::Document::parse_with_options(text, parsing)?;
        let mut options = usvg::Options {
            resources_dir: std::fs::canonicalize(path)?.parent().map(Path::to_path_buf),
            ..Default::default()
        };
        let text = ("http://www.w3.org/2000/svg", "text");
        if xml.descendants().any(|node| node.has_tag_name(text)) {
            options.fontdb = fonts();
        }
        let tree = usvg::Tree::from_xmltree(&xml, &options)?;
        Ok(Svg { tree })
    }

    /// The size the SVG declares, rounded up to whole px.
    pub fn size(&self) -> (u32, u32) {
        px(self.tree.size())
    }

    /// The px size and the scale of the whole SVG `width` px wide (`None`: as declared), as the resvg program fits it.
    fn fit(&self, width: Option<u32>) -> Option<((u32, u32), tiny_skia::Transform)> {
        let declared = self.tree.size();
        let size = match width {
            Some(width) => declared.scale_to_width(width as f32)?,
            None => declared,
        };
        let (sx, sy) = (
            size.width() / declared.width(),
            size.height() / declared.height(),
        );
        Some((px(size), tiny_skia::Transform::from_scale(sx, sy)))
    }

    /// The SVG through `transform` on `w` x `h` px; colours not premultiplied, as the resvg program writes them.
    fn draw(&self, (w, h): (u32, u32), transform: tiny_skia::Transform) -> Result<RgbaImage> {
        let Some(mut pixmap) = tiny_skia::Pixmap::new(w, h) else {
            crate::err!("no room to draw {w} x {h} px")
        };
        if caught(|| resvg::render(&self.tree, transform, &mut pixmap.as_mut())).is_none() {
            crate::err!("resvg failed")
        }
        let mut out = RgbaImage::new(w, h);
        for (to, from) in out.pixels_mut().zip(pixmap.pixels()) {
            let colour = from.demultiply();
            to.0 = [colour.red(), colour.green(), colour.blue(), colour.alpha()];
        }
        Ok(out)
    }

    /// The whole SVG `width` px wide (`None`: at its declared size).
    pub fn raster(&self, width: Option<u32>) -> Result<RgbaImage> {
        let Some((size, scale)) = self.fit(width) else {
            crate::err!("no SVG raster {width:?} px wide")
        };
        self.draw(size, scale)
    }

    /// The box (left, top, right, bottom) of the SVG drawn `logical` px and turned `turns` quarters clockwise, rasterized alone at `size`.
    pub fn part(
        &self,
        logical: (u32, u32),
        turns: u8,
        bbox: [f64; 4],
        size: (u32, u32),
    ) -> Result<RgbaImage> {
        let [left, top, right, bottom] = bbox;
        let scale = match self.fit(Some(logical.0)) {
            Some((_, scale)) if left < right && top < bottom => scale,
            _ => crate::err!("no part {bbox:?} of an SVG {logical:?} px"),
        };
        // one transform: the SVG onto the raster, the raster turned, the box onto the output
        let (sx, sy) = (scale.sx as f64, scale.sy as f64);
        let (w, h) = (logical.0 as f64, logical.1 as f64);
        // the raster's (x, y) turned: (a x + b y + e, c x + d y + f)
        let (a, b, c, d, e, f) = match turns % 4 {
            0 => (1.0, 0.0, 0.0, 1.0, 0.0, 0.0),
            1 => (0.0, -1.0, 1.0, 0.0, h, 0.0),
            2 => (-1.0, 0.0, 0.0, -1.0, w, h),
            _ => (0.0, 1.0, -1.0, 0.0, 0.0, w),
        };
        let (kx, ky) = (
            size.0 as f64 / (right - left),
            size.1 as f64 / (bottom - top),
        );
        let transform = tiny_skia::Transform::from_row(
            (kx * a * sx) as f32,
            (ky * c * sx) as f32,
            (kx * b * sy) as f32,
            (ky * d * sy) as f32,
            (kx * (e - left)) as f32,
            (ky * (f - top)) as f32,
        );
        self.draw(size, transform)
    }
}

/// Why `y` copied nothing.
pub enum NoCopy {
    TooLarge,
    Failed,
}

/// Fast compression: a copy or a screenshot is made while the user waits.
fn encoder(out: &mut Vec<u8>) -> PngEncoder<&mut Vec<u8>> {
    PngEncoder::new_with_quality(out, CompressionType::Fast, FilterType::Adaptive)
}

/// The still at `path` read again, at its own size (an SVG at the size it declares), upright and turned: PNG bytes, transparency kept, and its (width, height) (row S14).
pub fn original_png(path: &Path, turns: u8) -> std::result::Result<(Vec<u8>, (u32, u32)), NoCopy> {
    let copy = || {
        let image = if is_svg(path) {
            let svg = Svg::load(path).map_err(|_| NoCopy::Failed)?;
            if too_large(svg.size()) {
                return Err(NoCopy::TooLarge);
            }
            DynamicImage::ImageRgba8(svg.raster(None).map_err(|_| NoCopy::Failed)?)
        } else {
            match upright(path) {
                Ok(image) => image,
                Err(Not::TooLarge) => return Err(NoCopy::TooLarge),
                Err(_) => return Err(NoCopy::Failed),
            }
        };
        let image = match turns % 4 {
            0 => image,
            1 => image.rotate90(),
            2 => image.rotate180(),
            _ => image.rotate270(),
        };
        // PNG holds 8 and 16 bits a channel: float pictures (HDR, EXR) go to 8
        let image = match image.color() {
            ColorType::Rgb32F => DynamicImage::ImageRgb8(image.into_rgb8()),
            ColorType::Rgba32F => DynamicImage::ImageRgba8(image.into_rgba8()),
            _ => image,
        };
        let mut out = Vec::new();
        image
            .write_with_encoder(encoder(&mut out))
            .map_err(|_| NoCopy::Failed)?;
        Ok((out, (image.width(), image.height())))
    };
    caught(copy).unwrap_or(Err(NoCopy::Failed))
}

/// PNG bytes of an RGB picture, fast compression.
pub fn png(picture: &RgbImage) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    picture.write_with_encoder(encoder(&mut out))?;
    Ok(out)
}

/// FNV-1a 64 of `bytes`: how the tests name many bytes in one number.
#[cfg(test)]
pub(crate) fn fnv(bytes: impl IntoIterator<Item = u8>) -> u64 {
    let step = |hash: u64, byte: u8| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
    bytes.into_iter().fold(0xcbf2_9ce4_8422_2325, step)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::codecs::gif::GifEncoder;
    use image::codecs::jpeg::JpegEncoder;
    use image::codecs::png::PngEncoder;
    use image::codecs::webp::WebPEncoder;
    use image::{ColorType, DynamicImage, Frame, ImageEncoder, ImageFormat, Rgba};
    use std::io::{Cursor, Write};
    use std::path::PathBuf;

    const GREY: Rgb = [128, 128, 128];
    const SQUARE: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"><rect x="25" y="10" width="50" height="30"/></svg>"#;

    /// A folder of its own for a test, removed with it.
    struct Dir(PathBuf);

    impl Dir {
        fn new(test: &str) -> Dir {
            let dir =
                std::env::temp_dir().join(format!("asciier-picture-{}-{test}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Dir(dir)
        }

        fn file(&self, name: &str, data: impl AsRef<[u8]>) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, data).unwrap();
            path
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn encoded(image: &DynamicImage, format: ImageFormat) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        image.write_to(&mut out, format).unwrap();
        out.into_inner()
    }

    fn png_of(image: impl Into<DynamicImage>) -> Vec<u8> {
        encoded(&image.into(), ImageFormat::Png)
    }

    /// A PNG file from its chunks: (type, data).
    fn png_chunks(chunks: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        for (kind, data) in chunks {
            let mut crc = flate2::Crc::new();
            crc.update(*kind);
            crc.update(data);
            out.extend((data.len() as u32).to_be_bytes());
            out.extend(*kind);
            out.extend(data);
            out.extend(crc.sum().to_be_bytes());
        }
        out
    }

    fn ihdr(w: u32, h: u32, depth: u8, colour: u8) -> Vec<u8> {
        [
            &w.to_be_bytes()[..],
            &h.to_be_bytes(),
            &[depth, colour, 0, 0, 0],
        ]
        .concat()
    }

    fn zlib(raw: &[u8]) -> Vec<u8> {
        let mut out = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        out.write_all(raw).unwrap();
        out.finish().unwrap()
    }

    /// A PNG whose header declares `w` x `h` px of 1-bit grey and whose body holds none.
    fn png_header(w: u32, h: u32) -> Vec<u8> {
        png_chunks(&[
            (b"IHDR", ihdr(w, h, 1, 0)),
            (b"IDAT", zlib(b"")),
            (b"IEND", vec![]),
        ])
    }

    /// A 10 x 10 palette PNG, index 0 transparent black, index 1 white: the left half 0, the right half 1.
    fn palette_png() -> Vec<u8> {
        let rows: Vec<u8> = (0..10)
            .flat_map(|_| [0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1])
            .collect();
        png_chunks(&[
            (b"IHDR", ihdr(10, 10, 8, 3)),
            (b"PLTE", vec![0, 0, 0, 255, 255, 255]),
            (b"tRNS", vec![0]),
            (b"IDAT", zlib(&rows)),
            (b"IEND", vec![]),
        ])
    }

    /// An 8 x 8 APNG of `frames` frames of #010203: the first in the IDAT, or each in an fdAT behind a default image in the IDAT, outside the animation (`default`).
    fn apng(frames: u32, default: bool) -> Vec<u8> {
        let row = [&[0][..], &[1, 2, 3].repeat(8)].concat(); // filter 0
        let rows = zlib(&row.repeat(8));
        let frame = |sequence: u32| {
            [
                &sequence.to_be_bytes()[..],
                &8u32.to_be_bytes(),
                &8u32.to_be_bytes(),
                &[0; 8],
                &[0, 1, 0, 10, 0, 0],
            ]
            .concat()
        };
        let mut chunks = vec![
            (b"IHDR", ihdr(8, 8, 8, 2)),
            (b"acTL", [frames.to_be_bytes(), 0u32.to_be_bytes()].concat()),
        ];
        if !default {
            chunks.push((b"fcTL", frame(0)));
        }
        chunks.push((b"IDAT", rows.clone()));
        for sequence in (u32::from(!default)..frames).map(|at| 2 * at - u32::from(!default)) {
            chunks.push((b"fcTL", frame(sequence)));
            chunks.push((b"fdAT", [&(sequence + 1).to_be_bytes()[..], &rows].concat()));
        }
        chunks.push((b"IEND", vec![]));
        png_chunks(&chunks)
    }

    /// An 8 x 8 PNG of one frame whose acTL holds 4 of its 8 bytes: the frame count, 1, without the plays.
    fn short_actl() -> Vec<u8> {
        let row = [&[0][..], &[1, 2, 3].repeat(8)].concat(); // filter 0
        png_chunks(&[
            (b"IHDR", ihdr(8, 8, 8, 2)),
            (b"acTL", 1u32.to_be_bytes().to_vec()),
            (b"IDAT", zlib(&row.repeat(8))),
            (b"IEND", vec![]),
        ])
    }

    /// An 8 x 8 animated WebP of a lossless frame of each red: the container written here around the encoder's frames.
    fn animated_webp(reds: &[u8]) -> Vec<u8> {
        let chunk = |kind: &[u8; 4], data: &[u8]| {
            let mut out = kind.to_vec();
            out.extend((data.len() as u32).to_le_bytes());
            out.extend(data);
            out.extend(&[0u8][..data.len() % 2]);
            out
        };
        let size = [7, 0, 0, 7, 0, 0]; // width - 1, height - 1
        let mut body = b"WEBP".to_vec();
        body.extend(chunk(b"VP8X", &[&[0x02, 0, 0, 0][..], &size].concat())); // the animation flag
        body.extend(chunk(b"ANIM", &[0; 6]));
        for &red in reds {
            let mut frame = Vec::new();
            RgbaImage::from_pixel(8, 8, Rgba([red, 0, 0, 255]))
                .write_with_encoder(WebPEncoder::new_lossless(&mut frame))
                .unwrap();
            let head = [&[0; 6][..], &size, &[100, 0, 0, 0]].concat(); // offset, size, duration, flags
            body.extend(chunk(b"ANMF", &[&head[..], &frame[12..]].concat()));
        }
        [&b"RIFF"[..], &(body.len() as u32).to_le_bytes(), &body].concat()
    }

    /// An EXIF block holding the orientation alone.
    fn exif(orientation: u8) -> Vec<u8> {
        let mut out = b"II*\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0".to_vec();
        out.extend([orientation, 0, 0, 0, 0, 0, 0, 0]);
        out
    }

    /// The pictures of the oracle runs: red (31 x + 17 y) % 256, green (7 x + 59 y) % 256, blue 9 (x ^ y) % 256, alpha (23 x + 41 y) % 256.
    fn formula(w: u32, h: u32) -> RgbaImage {
        RgbaImage::from_fn(w, h, |x, y| {
            Rgba([
                (x * 31 + y * 17) as u8,
                (x * 7 + y * 59) as u8,
                ((x ^ y) * 9) as u8,
                (x * 23 + y * 41) as u8,
            ])
        })
    }

    fn opaque(image: &RgbaImage) -> RgbImage {
        DynamicImage::ImageRgba8(image.clone()).into_rgb8()
    }

    fn crc(bytes: &[u8]) -> u32 {
        let mut crc = flate2::Crc::new();
        crc.update(bytes);
        crc.sum()
    }

    /// What `open` makes of the file, in a word.
    fn kind(path: &Path) -> &'static str {
        match open(path) {
            Opened::Still(_) => "still",
            Opened::Animation => "animation",
            Opened::TooLarge(_) => "too large",
            Opened::NotImage => "no image",
        }
    }

    fn still(path: &Path) -> Still {
        match open(path) {
            Opened::Still(still) => still,
            _ => panic!("{path:?}: {}", kind(path)),
        }
    }

    /// A little-endian TIFF of 2 x 2 grey pages, each of one value.
    fn tiff(pages: &[u8]) -> Vec<u8> {
        let mut out = b"II*\0\x08\0\0\0".to_vec();
        for (i, &value) in pages.iter().enumerate() {
            // a directory of 9 entries (2 + 9 * 12 + 4 bytes), then its 4 px
            let pixels = out.len() as u32 + 114;
            let next = if i + 1 < pages.len() { pixels + 4 } else { 0 };
            // width, height, bits, no compression, black is zero, strip offset, samples, rows a strip, strip bytes
            let entries = [
                (256, 2),
                (257, 2),
                (258, 8),
                (259, 1),
                (262, 1),
                (273, pixels),
                (277, 1),
                (278, 2),
                (279, 4),
            ];
            out.extend(9u16.to_le_bytes());
            for (tag, content) in entries {
                out.extend([tag as u8, (tag >> 8) as u8, 3, 0, 1, 0, 0, 0]); // the tag, type SHORT, count 1
                out.extend(content.to_le_bytes());
            }
            out.extend(next.to_le_bytes());
            out.extend([value; 4]);
        }
        out
    }

    fn rgb(pic: &Pic) -> &RgbImage {
        match pic {
            Pic::Rgb(image) => image,
            Pic::Rgba(_) => panic!("RGBA"),
        }
    }

    fn rgba(pic: &Pic) -> &RgbaImage {
        match pic {
            Pic::Rgba(image) => image,
            Pic::Rgb(_) => panic!("RGB"),
        }
    }

    /// (least, most) alpha.
    fn alpha(image: &RgbaImage) -> (u8, u8) {
        let all = image.pixels().map(|p| p[3]);
        (all.clone().min().unwrap(), all.max().unwrap())
    }

    fn colours(image: &RgbImage) -> std::collections::BTreeSet<[u8; 3]> {
        image.pixels().map(|p| p.0).collect()
    }

    // tests/test_image.py:23
    #[test]
    fn alpha_kept() {
        let dir = Dir::new("alpha_kept");
        let src = dir.file("clear.png", png_of(RgbaImage::new(40, 30)));
        let opened = still(&src);
        let picture = rgba(&opened.pic);
        assert_eq!(picture.dimensions(), (40, 30));
        assert_eq!(picture.get_pixel(1, 1).0, [0, 0, 0, 0]);
        assert!(opened.svg.is_none());
    }

    // tests/test_image.py:30
    #[test]
    fn palette_transparency_kept() {
        let dir = Dir::new("palette_transparency_kept");
        let opened = still(&dir.file("pal.png", palette_png()));
        let picture = rgba(&opened.pic);
        assert_eq!(picture.get_pixel(0, 0).0, [0, 0, 0, 0]);
        assert_eq!(picture.get_pixel(9, 9).0, [255, 255, 255, 255]);
    }

    #[test]
    fn a_colour_key_is_transparency() {
        // row S3: open_picture of lib/ascii_image.py:109 at tag python-final: a colour key (tRNS of an RGB or a grey PNG) is transparency, clear on the key's pixels alone; these files give these pixels there too
        let dir = Dir::new("colour_key");
        // 4 x 1 px each, a row of filter 0, the key black: black, near black, grey, white
        let row = [0, 0, 0, 0, 0, 0, 1, 128, 128, 128, 255, 255, 255];
        let rgb = png_chunks(&[
            (b"IHDR", ihdr(4, 1, 8, 2)),
            (b"tRNS", vec![0; 6]),
            (b"IDAT", zlib(&row)),
            (b"IEND", vec![]),
        ]);
        let grey = png_chunks(&[
            (b"IHDR", ihdr(4, 1, 8, 0)),
            (b"tRNS", vec![0; 2]),
            (b"IDAT", zlib(&[0, 0, 1, 128, 255])),
            (b"IEND", vec![]),
        ]);
        let seen = [("rgb.png", rgb), ("grey.png", grey)].map(|(name, data)| {
            let opened = still(&dir.file(name, data));
            rgba(&opened.pic).pixels().map(|p| p.0).collect::<Vec<_>>()
        });
        let (white, grey) = ([255, 255, 255, 255], [128, 128, 128, 255]);
        assert_eq!(seen[0], [[0, 0, 0, 0], [0, 0, 1, 255], grey, white]);
        assert_eq!(seen[1], [[0, 0, 0, 0], [1, 1, 1, 255], grey, white]);
    }

    // tests/test_image.py:38
    #[test]
    fn opaque_is_rgb() {
        let dir = Dir::new("opaque_is_rgb");
        let plain = png_of(RgbImage::from_pixel(10, 10, [9, 8, 7].into()));
        let solid = png_of(RgbaImage::from_pixel(10, 10, [9, 8, 7, 255].into())); // an alpha band with nothing transparent in it
        let grey = png_of(image::GrayImage::from_pixel(10, 10, [5].into()));
        let seen = [("a.png", plain), ("b.png", solid), ("c.png", grey)]
            .map(|(name, data)| rgb(&still(&dir.file(name, data)).pic).get_pixel(3, 3).0);
        assert_eq!(seen, [[9, 8, 7], [9, 8, 7], [5, 5, 5]]);
    }

    // 16 bits are rounded to 8: Pillow kept the high byte (one apart at most) and clipped 16-bit greys at 255
    #[test]
    fn sixteen_bits_and_grey_alpha_become_eight_bit_rgb() {
        let dir = Dir::new("sixteen_bits");
        let deep = image::ImageBuffer::<image::Rgb<u16>, _>::from_pixel(
            4,
            4,
            [0xffff, 0x8000, 0x0081].into(),
        );
        let deep = still(&dir.file("deep.png", png_of(deep)));
        assert_eq!(rgb(&deep.pic).get_pixel(0, 0).0, [255, 128, 1]);
        let grey = image::ImageBuffer::<image::Luma<u16>, _>::from_pixel(4, 4, [0x8000].into());
        let grey = still(&dir.file("grey.png", png_of(grey)));
        assert_eq!(rgb(&grey.pic).get_pixel(0, 0).0, [128; 3]);
        let veil = image::GrayAlphaImage::from_pixel(4, 4, [200, 100].into());
        let veil = still(&dir.file("veil.png", png_of(veil)));
        assert_eq!(rgba(&veil.pic).get_pixel(0, 0).0, [200, 200, 200, 100]);
    }

    // tests/test_image.py:46
    #[test]
    fn svg_opens_as_its_1600_px_raster() {
        let dir = Dir::new("svg");
        let clear = dir.file("clear.svg", SQUARE);
        let solid = dir.file(
            "solid.SVG",
            SQUARE.replace(
                "<rect",
                r#"<rect width="100" height="50" fill="white"/><rect"#,
            ),
        );
        let opened = still(&clear);
        let picture = rgba(&opened.pic);
        assert_eq!(picture.dimensions(), (SVG_WIDTH, 800));
        assert_eq!(picture.get_pixel(5, 5)[3], 0);
        assert_eq!(picture.get_pixel(800, 400).0, [0, 0, 0, 255]);
        let svg = opened.svg.expect("the SVG behind the raster");
        assert_eq!(svg.size(), (100, 50));
        assert_eq!(svg.raster(Some(300)).unwrap().dimensions(), (300, 150));
        assert_eq!(rgb(&still(&solid).pic).dimensions(), (1600, 800));
        assert_eq!(
            (
                raster_width(500, (100, 50)),
                raster_width(1_000_000, (100, 50)),
                raster_width(1_000_000, (50, 100))
            ),
            (500, SVG_MAX, SVG_MAX / 2)
        );
    }

    // tests/test_image.py:59
    #[test]
    fn svg_part_in_four_turns() {
        let dir = Dir::new("svg_part");
        // the rect: x 25..75, y 10..40 of 100 x 50; the name: any file name loads
        let svg = Svg::load(&dir.file("a b#1 &'\".svg", SQUARE)).unwrap();
        // (turns, box, size, least and most alpha)
        let cases = [
            (0, [25.0, 10.0, 75.0, 40.0], (500, 300), (255, 255)),
            (0, [0.0, 0.0, 20.0, 50.0], (40, 100), (0, 0)),
            // turned a quarter right: (x, y) -> (50 - y, x), the rect at x 10..40, y 25..75 of 50 x 100
            (1, [10.0, 25.0, 40.0, 75.0], (30, 50), (255, 255)),
            (1, [0.0, 0.0, 50.0, 20.0], (50, 20), (0, 0)),
            (2, [25.0, 10.0, 75.0, 40.0], (50, 30), (255, 255)),
            (3, [10.0, 25.0, 40.0, 75.0], (30, 50), (255, 255)),
        ];
        for (turns, bbox, size, expected) in cases {
            let part = svg.part((100, 50), turns, bbox, size).unwrap();
            let seen = (part.dimensions(), alpha(&part));
            assert_eq!(seen, (size, expected), "turn {turns}, {bbox:?}");
        }
        // the rect's left half at x 25..50
        let half = svg.part((100, 50), 0, [0.0, 0.0, 50.0, 50.0], (50, 50));
        let half = half.unwrap();
        let within = |x0: u32, y0: u32, x1: u32, y1: u32| {
            alpha(&image::imageops::crop_imm(&half, x0, y0, x1 - x0, y1 - y0).to_image())
        };
        assert_eq!(within(0, 0, 24, 50), (0, 0));
        assert_eq!(within(26, 11, 50, 39), (255, 255));
    }

    #[test]
    fn svg_part_agrees_with_the_raster() {
        let dir = Dir::new("svg_part_agrees");
        let drawing = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"><rect width="30" height="20" fill="#c00"/><circle cx="60" cy="25" r="18" fill="#06c" fill-opacity="0.7"/><path d="M5 45 L95 5" stroke="#000" stroke-width="3"/></svg>"##;
        let svg = Svg::load(&dir.file("drawing.svg", drawing)).unwrap();
        let whole = Pic::Rgba(svg.raster(Some(400)).unwrap());
        for turns in 0..4 {
            // the middle half of the turned picture, at the raster's 4 px per logical px
            let (w, h) = if turns % 2 == 1 { (50, 100) } else { (100, 50) };
            let bbox = [
                w as f64 / 4.0,
                h as f64 / 4.0,
                w as f64 * 0.75,
                h as f64 * 0.75,
            ];
            let part = svg.part((100, 50), turns, bbox, (w * 2, h * 2)).unwrap();
            let Pic::Rgba(full) = turned(&whole, turns) else {
                unreachable!()
            };
            let same = flatten(
                &Pic::Rgba(image::imageops::crop_imm(&full, w, h, w * 2, h * 2).to_image()),
                GREY,
            );
            assert!(
                alpha(&part).0 < alpha(&part).1,
                "turn {turns}: the part shows the drawing"
            );
            // as shown, over a ground: the sum of all bytes' distances
            let apart = |part: RgbaImage| -> usize {
                let shown = flatten(&Pic::Rgba(part), GREY);
                let gaps = shown.as_raw().iter().zip(same.as_raw());
                gaps.map(|(a, b)| a.abs_diff(*b) as usize).sum()
            };
            // edge px differ by a little coverage (resvg clips edges to what it draws on): measured under 0.7 % of bytes, by 18 at most, 0.034 a byte
            let bytes = same.as_raw().len();
            let sum = apart(part);
            assert!(sum * 4 < bytes, "turn {turns}: {sum} apart in all");
            // one px to the side is seen
            let beside = [bbox[0] + 0.25, bbox[1], bbox[2] + 0.25, bbox[3]];
            let sum = apart(svg.part((100, 50), turns, beside, (w * 2, h * 2)).unwrap());
            assert!(sum > bytes, "turn {turns}: {sum} apart, one px beside");
        }
    }

    #[test]
    fn svg_size_rounds_up_and_a_broken_one_is_no_image() {
        let dir = Dir::new("svg_size");
        let odd = r#"<svg xmlns="http://www.w3.org/2000/svg" width="10.2" height="5.5"><rect width="5" height="5"/></svg>"#;
        let svg = Svg::load(&dir.file("odd.svg", odd)).unwrap();
        assert_eq!(svg.size(), (11, 6));
        assert_eq!(svg.raster(None).unwrap().dimensions(), (11, 6));
        assert!(Svg::load(&dir.file("broken.svg", "<svg")).is_err());
        assert_eq!(kind(&dir.0.join("broken.svg")), "no image");
        assert_eq!(kind(&dir.0.join("missing.svg")), "no image");
    }

    #[test]
    fn svg_resolves_relative_links_and_draws_text() {
        let dir = Dir::new("svg_links");
        dir.file(
            "dot.png",
            png_of(RgbImage::from_pixel(2, 2, [255, 0, 0].into())),
        );
        let linked = r#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20"><image href="dot.png" width="20" height="20"/></svg>"#;
        let svg = Svg::load(&dir.file("linked.svg", linked)).unwrap();
        assert_eq!(
            svg.raster(None).unwrap().get_pixel(10, 10).0,
            [255, 0, 0, 255]
        );
        let words = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"><text x="5" y="40" font-size="40" font-family="{}">Mg</text></svg>"#,
            crate::font::FAMILY
        );
        let svg = Svg::load(&dir.file("words.svg", words)).unwrap();
        assert_eq!(alpha(&svg.raster(None).unwrap()), (0, 255));
        // zoomed parts come from the same tree: text stays
        let part = svg
            .part((100, 50), 0, [0.0, 0.0, 50.0, 50.0], (200, 200))
            .unwrap();
        assert_eq!(alpha(&part), (0, 255));
    }

    #[test]
    fn an_svg_goes_between_threads() {
        fn shared<T: Send + Sync>() {}
        shared::<Svg>();
    }

    // tests/test_image.py:74, :101
    #[test]
    fn flatten_puts_transparency_over_the_colour() {
        let mut clear = RgbaImage::new(4, 4);
        clear.put_pixel(0, 0, Rgba([0, 0, 0, 255]));
        let flat = flatten(&Pic::Rgba(clear), GREY);
        assert_eq!(
            (flat.get_pixel(0, 0).0, flat.get_pixel(3, 3).0),
            ([0, 0, 0], GREY)
        );
        let mut two = RgbaImage::new(2, 1);
        two.put_pixel(0, 0, Rgba([255, 0, 0, 255]));
        let flat = flatten(&Pic::Rgba(two), [1, 2, 3]);
        assert_eq!(
            (flat.get_pixel(0, 0).0, flat.get_pixel(1, 0).0),
            ([255, 0, 0], [1, 2, 3])
        );
        let plain = RgbImage::from_pixel(2, 2, [7, 7, 7].into());
        assert_eq!(flatten(&Pic::Rgb(plain.clone()), [0, 0, 0]), plain);
    }

    // (colour, alpha, background) -> result of ascii_image.flatten (Pillow's paste through the alpha)
    #[test]
    fn flatten_blends_as_pillow() {
        let cases = [
            (0, 0, 128, 128),
            (0, 255, 128, 0),
            (255, 128, 0, 128),
            (255, 127, 0, 127),
            (10, 1, 200, 199),
            (200, 254, 10, 199),
            (37, 99, 201, 137),
            (255, 1, 0, 1),
            (0, 128, 255, 127),
            (129, 128, 128, 129),
        ];
        for (colour, alpha, background, expected) in cases {
            let top = RgbaImage::from_pixel(1, 1, Rgba([colour, colour, colour, alpha]));
            let flat = flatten(&Pic::Rgba(top), [background; 3]);
            assert_eq!(
                flat.get_pixel(0, 0).0,
                [expected; 3],
                "{colour} {alpha} {background}"
            );
        }
    }

    /// A 3 x 2 PNG whose pixel (x, y) has red y * 3 + x, with an EXIF orientation.
    fn oriented(orientation: u8) -> Vec<u8> {
        let source = RgbImage::from_fn(3, 2, |x, y| [(y * 3 + x) as u8, 100, 200].into());
        let mut out = Vec::new();
        let mut encoder = PngEncoder::new(&mut out);
        encoder.set_exif_metadata(exif(orientation)).unwrap();
        source.write_with_encoder(encoder).unwrap();
        out
    }

    /// (size, each pixel's red) of a picture.
    fn layout(image: &RgbImage) -> ((u32, u32), String) {
        let reds = image.pixels().map(|p| p[0].to_string()).collect();
        (image.dimensions(), reds)
    }

    // expected: ascii_image.open_picture (Pillow's exif_transpose) on the same eight files
    #[test]
    fn exif_orientations_come_upright() {
        let dir = Dir::new("exif_orientations");
        let expected = [
            ((3, 2), "012345"),
            ((3, 2), "210543"),
            ((3, 2), "543210"),
            ((3, 2), "345012"),
            ((2, 3), "031425"),
            ((2, 3), "304152"),
            ((2, 3), "524130"),
            ((2, 3), "251403"),
        ];
        for (n, (size, reds)) in (1..).zip(expected) {
            let opened = still(&dir.file(&format!("o{n}.png"), oriented(n)));
            assert_eq!(
                layout(rgb(&opened.pic)),
                (size, reds.to_string()),
                "orientation {n}"
            );
        }
    }

    fn jpeg(image: &RgbImage, orientation: Option<u8>) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = JpegEncoder::new(&mut out);
        if let Some(n) = orientation {
            encoder.set_exif_metadata(exif(n)).unwrap();
        }
        image.write_with_encoder(encoder).unwrap();
        out
    }

    // tests/test_image.py:81
    #[test]
    fn exif_orientation_of_a_jpeg_and_a_webp() {
        let dir = Dir::new("exif_jpeg");
        let cam = dir.file("cam.jpg", jpeg(&RgbImage::new(400, 200), Some(6)));
        let plain = dir.file(
            "plain.jpg",
            jpeg(&RgbImage::from_pixel(400, 200, [10, 20, 30].into()), None),
        );
        assert_eq!(still(&cam).pic.size(), (200, 400));
        assert_eq!(still(&plain).pic.size(), (400, 200));
        let mut webp = Vec::new();
        let mut encoder = WebPEncoder::new_lossless(&mut webp);
        encoder.set_exif_metadata(exif(8)).unwrap();
        RgbImage::new(40, 20).write_with_encoder(encoder).unwrap();
        assert_eq!(still(&dir.file("cam.webp", webp)).pic.size(), (20, 40));
    }

    #[test]
    fn every_built_in_decoder_opens() {
        let dir = Dir::new("decoders");
        let picture = opaque(&formula(16, 8));
        let exact = [
            ("png", ImageFormat::Png),
            ("webp", ImageFormat::WebP),
            ("bmp", ImageFormat::Bmp),
            ("tif", ImageFormat::Tiff),
            ("ico", ImageFormat::Ico),
            ("tga", ImageFormat::Tga), // no magic number: told by its suffix
            ("qoi", ImageFormat::Qoi),
            ("ppm", ImageFormat::Pnm),
        ];
        for (suffix, format) in exact {
            // with an alpha band, opaque: an icon's picture has to have one
            let source = DynamicImage::ImageRgb8(picture.clone()).into_rgba8();
            let src = dir.file(&format!("a.{suffix}"), encoded(&source.into(), format));
            assert_eq!(rgb(&still(&src).pic), &picture, "{suffix}");
            assert!(listed(&src) && identifies(&src), "{suffix}");
        }
        let jpg = dir.file("a.jpg", encoded(&picture.clone().into(), ImageFormat::Jpeg));
        assert_eq!(still(&jpg).pic.size(), (16, 8));
        // floats (HDR, EXR) come as 8 bits, also in a copy
        let float = image::Rgb32FImage::from_fn(16, 8, |x, y| {
            [x as f32 / 15.0, y as f32 / 7.0, 0.2].into()
        });
        for (suffix, format) in [("hdr", ImageFormat::Hdr), ("exr", ImageFormat::OpenExr)] {
            let src = dir.file(
                &format!("a.{suffix}"),
                encoded(&float.clone().into(), format),
            );
            let opened = still(&src);
            let [r, g, b] = rgb(&opened.pic).get_pixel(15, 0).0;
            assert!(
                r >= 254 && g == 0 && b.abs_diff(51) <= 1,
                "{suffix}: {r} {g} {b}"
            );
            assert_eq!(copied(&src, 0).color(), ColorType::Rgb8, "{suffix}");
        }
        // tests/golden/picture-quarters.*: Pillow's 16 x 8 picture of four flat quarters (the image crate writes neither format)
        let quarters = [[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 255]];
        let avif: &[u8] = include_bytes!("../tests/golden/picture-quarters.avif");
        let dds: &[u8] = include_bytes!("../tests/golden/picture-quarters.dds");
        for (name, bytes, slack) in [("a.avif", avif, 6), ("a.dds", dds, 0)] {
            let opened = still(&dir.file(name, bytes));
            let picture = rgb(&opened.pic);
            assert_eq!(picture.dimensions(), (16, 8), "{name}");
            for (colour, (x, y)) in quarters.iter().zip([(2, 1), (12, 1), (2, 6), (12, 6)]) {
                let seen = picture.get_pixel(x, y).0;
                let near = seen
                    .iter()
                    .zip(colour)
                    .all(|(a, b)| a.abs_diff(*b) <= slack);
                assert!(near, "{name} at ({x}, {y}): {seen:?}");
            }
        }
    }

    fn gif(frames: u8) -> Vec<u8> {
        let mut out = Vec::new();
        let frames =
            (0..frames).map(|i| Frame::new(RgbaImage::from_pixel(8, 8, Rgba([80 * i, 0, 0, 255]))));
        GifEncoder::new(&mut out).encode_frames(frames).unwrap();
        out
    }

    /// A GIF87a of a `screen` with Pillow's palette of red and three blacks, then `blocks`, then the trailer.
    fn gif_of(screen: (u16, u16), blocks: &[&[u8]]) -> Vec<u8> {
        let mut out = b"GIF87a".to_vec();
        out.extend(screen.0.to_le_bytes());
        out.extend(screen.1.to_le_bytes());
        out.extend([&[0x81, 0, 0, 200][..], &[0; 11]].concat()); // a palette of 4, its background 0
        out.extend(blocks.concat());
        out.push(b';');
        out
    }

    /// An image descriptor of `w` x `h` px at (`x`, `y`), then Pillow's data of an 8 x 8 frame of red.
    fn gif_frame(x: u16, y: u16, w: u16, h: u16) -> Vec<u8> {
        let mut out = vec![b','];
        for value in [x, y, w, h] {
            out.extend(value.to_le_bytes());
        }
        let data = [
            0, 1, 8, 0x1c, 0x48, 0xb0, 0xa0, 0xc1, 0x83, 8, 0x13, 0x2a, 0x4c, 0x18, 0x10,
        ];
        out.extend([&[0, 8, 15][..], &data, &[0]].concat()); // no palette, 8 bits a code, a sub-block, its terminator
        out
    }

    #[test]
    fn animations_and_non_images_are_no_stills() {
        // rows S1, V17: tests/test_image.py:93 at tag python-final: a GIF, an APNG or an animated WebP of more than one frame is an animation, no still, also an APNG of one frame behind a default image outside the animation, two frames to Pillow's n_frames (lib/ascii_image.py:61; it opened as a still while frames were counted by decoding, which passes over that image), and a GIF cut short in its second frame, a frame to Pillow 13 from its image descriptor on (GifImagePlugin _seek); text, a clip, a missing file, a folder are no picture; so is a PNG whose acTL is cut short, which Pillow 13 refuses (PngImagePlugin chunk_acTL: `APNG contains truncated acTL chunk`, a ValueError, no picture to open_picture of lib/ascii_image.py:105; seen 2026-11-13)
        let dir = Dir::new("animations");
        assert_eq!(kind(&dir.file("a.gif", gif(3))), "animation");
        let two = gif(2);
        let cut = dir.file("cut.gif", &two[..two.len() - 3]);
        assert_eq!(kind(&cut), "animation");
        assert_eq!(kind(&dir.file("a.png", apng(2, false))), "animation");
        assert_eq!(kind(&dir.file("default.png", apng(1, true))), "animation");
        assert_eq!(
            kind(&dir.file("a.webp", animated_webp(&[0, 255]))),
            "animation"
        );
        assert_eq!(kind(&dir.file("x.png", "not a picture")), "no image");
        assert_eq!(kind(&dir.file("cut-actl.png", short_actl())), "no image");
        assert_eq!(kind(&dir.file("clip.mp4", [0u8; 64])), "no image");
        assert_eq!(kind(&dir.0.join("missing.png")), "no image");
        assert_eq!(kind(&dir.0), "no image");
    }

    #[test]
    fn an_avif_sequence_is_an_animation() {
        // rows S1, V17: is_animation of lib/ascii_image.py:61 at tag python-final: an AVIF image sequence of 2 frames is an animation, no still, and identifies no still (lib/ascii_image.py:67), as Pillow 13 counts its frames (AvifImagePlugin n_frames: libavif takes a sequence's track where the major brand is avis, mif1 or msf1, the primary item where it is avif; seen 2026-11-16, Pillow 13.0.0 and libavif 1.4.2: 2 frames, 2 under mif1, 1 under avif); tests/golden/picture-sequence.avif: avifenc 1.4.2 of two 16 x 8 px frames of four flat quarters, a sequence behind its first frame as the primary item, which the decoder shows
        let dir = Dir::new("avif_sequence");
        let sequence: &[u8] = include_bytes!("../tests/golden/picture-sequence.avif");
        assert_eq!(&sequence[8..12], b"avis"); // the major brand
        for (brand, seen) in [
            ("avis", "animation"),
            ("mif1", "animation"),
            ("avif", "still"),
        ] {
            let file = [&sequence[..8], brand.as_bytes(), &sequence[12..]].concat();
            let file = dir.file("sequence.avif", file);
            assert_eq!(
                (kind(&file), identifies(&file)),
                (seen, seen == "still"),
                "{brand}"
            );
        }
    }

    #[test]
    fn one_frame_of_an_animated_format_is_a_still() {
        // rows S1, V17: is_animation of lib/ascii_image.py:61 at tag python-final: more than one frame (Pillow's n_frames) makes an animation; a GIF, an APNG or an animated WebP of one frame is a still (the last two went to mpv while their headers decided); so is a GIF of one frame without its trailer, whose blocks Pillow 13 walks to the file's end (GifImagePlugin _seek; seen 2026-11-13), where decoding took the missing trailer for a second frame
        let dir = Dir::new("one_frame");
        let one = still(&dir.file("one.gif", gif(1)));
        assert_eq!(rgb(&one.pic).dimensions(), (8, 8));
        let whole = gif(1);
        let open = still(&dir.file("open.gif", &whole[..whole.len() - 1]));
        assert_eq!(rgb(&open.pic).get_pixel(7, 7).0, [0, 0, 0]);
        let source = RgbImage::from_pixel(8, 8, [1, 2, 3].into());
        let one = still(&dir.file("one.webp", encoded(&source.into(), ImageFormat::WebP)));
        assert_eq!(rgb(&one.pic).get_pixel(7, 7).0, [1, 2, 3]);
        let one = still(&dir.file("one.png", apng(1, false)));
        assert_eq!(rgb(&one.pic).get_pixel(7, 7).0, [1, 2, 3]);
        // the frame laid on the canvas: 199 by image-webp for 200, which libwebp (Pillow's) keeps
        let one = still(&dir.file("anim.webp", animated_webp(&[200])));
        let [red, green, blue] = rgb(&one.pic).get_pixel(7, 7).0;
        assert!(
            red.abs_diff(200) <= 1 && green == 0 && blue == 0,
            "{red} {green} {blue}"
        );
    }

    #[test]
    fn more_pictures_in_a_tiff_or_behind_a_jpeg_leave_a_still() {
        // row N7: the first page shows (the Python tools played the pages as an animation); a JPEG with more pictures behind it (MPO) shows its first
        let dir = Dir::new("more_pictures");
        let pages = still(&dir.file("pages.tif", tiff(&[10, 200])));
        assert_eq!(colours(rgb(&pages.pic)), [[10, 10, 10]].into());
        assert!(identifies(&dir.0.join("pages.tif")));
        let first = jpeg(&RgbImage::from_pixel(16, 8, [200, 0, 0].into()), None);
        let second = jpeg(&RgbImage::new(8, 16), None);
        let stereo = still(&dir.file("stereo.mpo", [first, second].concat()));
        assert_eq!(stereo.pic.size(), (16, 8));
    }

    #[test]
    fn the_content_names_the_format_not_the_suffix() {
        let dir = Dir::new("content");
        let src = dir.file(
            "really-a-png.jpg",
            png_of(RgbImage::from_pixel(5, 4, [1, 2, 3].into())),
        );
        assert_eq!(rgb(&still(&src).pic).get_pixel(4, 3).0, [1, 2, 3]);
        assert!(identifies(&src));
    }

    // tests/golden/picture-cmyk.jpg: Pillow's Image.new("CMYK", (50, 40), (0, 255, 255, 0)), red
    #[test]
    fn cmyk_opens_as_rgb() {
        let dir = Dir::new("cmyk_opens");
        let src = dir.file(
            "print.jpg",
            include_bytes!("../tests/golden/picture-cmyk.jpg"),
        );
        let opened = still(&src);
        let [r, g, b] = rgb(&opened.pic).get_pixel(5, 5).0;
        assert_eq!(opened.pic.size(), (50, 40));
        assert!(r > 250 && g < 5 && b < 5, "{r} {g} {b}");
    }

    #[test]
    fn over_the_limit_is_too_large_not_unopenable() {
        // row S18: tests/test_image.py:116 at tag python-final: a still over twice Pillow's MAX_IMAGE_PIXELS is not opened, too large with `<name> is too large to open (over 179 megapixels)` (lib/ascii_image.py:23, :95), and identifies all the same (lib/ascii_image.py:76); here also its copy refused as too large, and one at the limit given to the decoder
        let dir = Dir::new("too_large");
        // 2 * 5 * 29 * 43 = 12470 and 113 * 127 = 14351: their product is MAX_PIXELS
        assert_eq!(12470 * 14351, MAX_PIXELS);
        let big = dir.file("big.png", png_header(12470, 14352));
        let Opened::TooLarge(why) = open(&big) else {
            panic!("not too large")
        };
        assert_eq!(why, "big.png is too large to open (over 179 megapixels)");
        assert!(identifies(&big)); // an image all the same: stepping lands on it and says so
        assert!(matches!(original_png(&big, 0), Err(NoCopy::TooLarge)));
        // at the limit the decoder is tried: this file holds no pixels
        let full = dir.file("full.png", png_header(12470, 14351));
        assert_eq!(kind(&full), "no image");
        assert!(identifies(&full));
    }

    #[test]
    fn a_size_its_decoder_gives_up_on_is_too_large() {
        // row S18: Pillow 13 measures a PNG or a QOI by its header once it has read to the image data (PngImagePlugin and QoiImagePlugin _open, then Image.open's _decompression_bomb_check): over the limit it is too large to open and identifies all the same (lib/ascii_image.py:76, :95 at tag python-final; seen 2026-11-15, Pillow 13.0.0: a PNG of 2147483647 x 2147483647 px, the largest a PNG's header holds, of 8 bits a channel, and QOIs of 20025 x 20025 px and of 4294967295 x 4294967295 px); the png crate gives up on that PNG's header (its limits, though none were set) and the qoi crate on one over 400 megapixels, as no picture, before a size is measured; so too a BMP, an icon and a WebP, as Pillow measures them (BmpImagePlugin _bitmap, the sides unsigned, a height whose top byte is 0xFF top-down; IcoImagePlugin IcoFile.frame, the entry's PNG or BMP, the BMP's height twice the picture's; libwebp's canvas; seen 2026-11-16, Pillow 13.0.0: BMPs of 2147483647 x 1 px, of 8 px by the least height, -2^31, of the width -8 px and of 65536 x 2731 px, an icon whose BMP declares 60000 x 120000 px or whose PNG the largest sides, a WebP of a canvas of 2^24 px a side over a frame of 8 x 4 px), where the bmp decoder gives up on a side over 65535 px, a negative width and the least height, the ico decoder as the png crate does, and image-webp on a canvas over 2^32 px
        let dir = Dir::new("decoder_gives_up");
        let side = png_chunks(&[
            (b"IHDR", ihdr(0x7FFF_FFFF, 0x7FFF_FFFF, 8, 2)),
            (b"IDAT", zlib(b"")),
            (b"IEND", vec![]),
        ]);
        let qoi = |side: u32| {
            let sides = [side.to_be_bytes(), side.to_be_bytes()].concat();
            [
                &b"qoif"[..],
                &sides,
                &[4, 0],
                &[0; 8],
                &[0, 0, 0, 0, 0, 0, 0, 1],
            ]
            .concat()
        };
        // a DIB header of 40 bytes: its size, the sides, one plane of `bits`, no compression; then pixels
        let dib = |w: i32, h: i32, bits: u8| {
            let sides = [w.to_le_bytes(), h.to_le_bytes()].concat();
            [
                &40u32.to_le_bytes()[..],
                &sides,
                &[1, 0, bits, 0],
                &[0; 24],
                &[0; 64],
            ]
            .concat()
        };
        let bmp = |w: i32, h: i32| {
            let dib = dib(w, h, 24);
            let sizes = [
                (14 + dib.len() as u32).to_le_bytes(),
                [0; 4],
                54u32.to_le_bytes(),
            ];
            [&b"BM"[..], &sizes.concat(), &dib].concat() // the file's size, two reserved, the pixels' place
        };
        let mut canvas = webp_with(b"abcd", 0, &[], 0);
        canvas[24..30].fill(0xFF); // the canvas's width - 1 and height - 1
        let cases = [
            ("side.png", side.clone()),
            ("over.qoi", qoi(20025)),
            ("most.qoi", qoi(u32::MAX)),
            ("wide.bmp", bmp(0x7FFF_FFFF, 1)),
            ("least.bmp", bmp(8, i32::MIN)),
            ("negative.bmp", bmp(-8, 8)),
            ("over.bmp", bmp(65536, 2731)),
            ("dib.ico", ico_of(&dib(60000, 120000, 32))),
            ("side.ico", ico_of(&side)),
            ("canvas.webp", canvas),
        ];
        let seen = cases.map(|(name, data)| {
            let file = dir.file(name, data);
            let said = match open(&file) {
                Opened::TooLarge(why) => {
                    why == format!("{name} is too large to open (over 179 megapixels)")
                }
                _ => false,
            };
            let refused = matches!(original_png(&file, 0), Err(NoCopy::TooLarge));
            (name, said, identifies(&file), refused, kind(&file))
        });
        let all = seen
            .iter()
            .all(|&(_, said, id, refused, _)| said && id && refused);
        assert!(all, "{seen:?}"); // (name, too large with its message, identifies, its copy refused as too large, open)
    }

    /// An EXR of one part: three half channels B, G, R and `floats` float channels more, in ZIP's blocks of 16 lines, a display window and a data window (xMin, yMin, xMax, yMax), the offset table, then each block: its first line, its size, `block`.
    fn exr(display: [i32; 4], data: [i32; 4], block: &[u8], floats: usize) -> Vec<u8> {
        let attribute = |name: &str, kind: &str, value: &[u8]| {
            let size = (value.len() as i32).to_le_bytes();
            [name.as_bytes(), b"\0", kind.as_bytes(), b"\0", &size, value].concat()
        };
        let window = |bounds: [i32; 4]| bounds.map(i32::to_le_bytes).concat();
        // each: its name, HALF (1) or FLOAT (2), not linear, 3 reserved bytes, sampled every pixel across and down
        let channel = |name: &[u8], kind: u8| {
            [
                name,
                b"\0",
                &[kind, 0, 0, 0],
                &[0; 4],
                &[1, 0, 0, 0],
                &[1, 0, 0, 0],
            ]
            .concat()
        };
        let mut channels = [channel(b"B", 1), channel(b"G", 1), channel(b"R", 1)].concat();
        for more in 0..floats {
            channels.extend(channel(format!("c{more:04}").as_bytes(), 2)); // after the capitals, as the names go
        }
        channels.push(0);
        let mut out = vec![0x76, 0x2f, 0x31, 0x01, 2, 0, 0, 0]; // the magic number, version 2, no flags
        out.extend(attribute("channels", "chlist", &channels));
        out.extend(attribute("compression", "compression", &[3])); // ZIP
        out.extend(attribute("dataWindow", "box2i", &window(data)));
        out.extend(attribute("displayWindow", "box2i", &window(display)));
        out.extend(attribute("lineOrder", "lineOrder", &[0]));
        out.extend(attribute("pixelAspectRatio", "float", &1f32.to_le_bytes()));
        out.extend(attribute("screenWindowCenter", "v2f", &[0; 8]));
        out.extend(attribute("screenWindowWidth", "float", &1f32.to_le_bytes()));
        out.push(0); // the header's end
        let blocks = (data[3] - data[1]) / 16 + 1;
        let first = out.len() as u64 + 8 * blocks as u64;
        for at in 0..blocks as u64 {
            out.extend((first + at * (8 + block.len() as u64)).to_le_bytes());
        }
        for at in 0..blocks {
            out.extend((data[1] + 16 * at).to_le_bytes());
            out.extend((block.len() as i32).to_le_bytes());
            out.extend(block);
        }
        out
    }

    #[test]
    fn an_exr_is_measured_by_its_data_window_too() {
        // rows S18, N7: an EXR, a format of the built-in decoders that Pillow 13 does not read, is held to the limit by its data window, the pixels its blocks hold, as well as by its display window, its picture's size (owner 2026-11-16): too large to open, identifies, its copy refused; the exr crate allocates a block's pixels by the data window before it inflates them (zune-inflate's output, by its size hint), 103 GB for a ZIP block of 16 lines 1073741821 px wide behind a display window of 16 x 8 px; a data window at the limit goes to the decoder, which finds its blocks empty; one of 16 x 8 px over a zero picture opens, black; a block's pixels are allocated in all the channels' bytes, so a block, its lines or its tile within the data window, past the bytes of a picture at the limit in four float channels is no picture: 430 GB for a ZIP block of 16 lines 11184810 px wide in 3 half channels and 600 float ones
        let dir = Dir::new("exr_window");
        let small = [0, 0, 15, 7];
        let zero = zlib(&[&[0][..], &[128; 767]].concat()); // 16 x 8 px of 3 half channels at 0: each byte its difference from the last plus 128
        let opened = still(&dir.file("small.exr", exr(small, small, &zero, 0)));
        assert_eq!(rgb(&opened.pic).dimensions(), (16, 8));
        assert_eq!(colours(rgb(&opened.pic)), [[0, 0, 0]].into());
        assert_eq!(12470 * 14351, MAX_PIXELS);
        for (name, data) in [
            ("past.exr", [0, 0, 12469, 14351]),
            ("wide.exr", [0, 0, 1073741820, 15]),
        ] {
            let file = dir.file(name, exr(small, data, &zlib(b""), 0));
            let Opened::TooLarge(why) = open(&file) else {
                panic!("{name}: {}", kind(&file))
            };
            assert_eq!(
                why,
                format!("{name} is too large to open (over 179 megapixels)")
            );
            assert!(identifies(&file), "{name}");
            assert!(
                matches!(original_png(&file, 0), Err(NoCopy::TooLarge)),
                "{name}"
            );
        }
        let at = dir.file("at.exr", exr(small, [0, 0, 12469, 14350], &zlib(b""), 0));
        assert_eq!((kind(&at), identifies(&at)), ("no image", true));
        // a block of a data window within the limit, its pixels in all their channels' bytes past a picture's at the limit in four float channels
        let channels = dir.file(
            "channels.exr",
            exr(small, [0, 0, 11184809, 15], &zlib(b""), 600),
        );
        assert_eq!(
            (kind(&channels), identifies(&channels)),
            ("no image", false)
        );
    }

    /// tests/golden/picture-quarters.avif, its ispe property declaring `w` x `h` px.
    fn quarters_avif(w: u32, h: u32) -> Vec<u8> {
        let mut avif = include_bytes!("../tests/golden/picture-quarters.avif").to_vec();
        let at = avif.windows(4).position(|kind| kind == b"ispe").unwrap() + 8; // past the kind, the version and flags
        avif[at..at + 8].copy_from_slice(&[w.to_be_bytes(), h.to_be_bytes()].concat());
        avif
    }

    /// `avif`, laid out as tests/golden/picture-quarters.avif, with `property` after the properties of its ipco box: the boxes around it grown, the picture's place in its iloc box moved on.
    fn with_property(avif: &[u8], property: &[u8]) -> Vec<u8> {
        let place = |out: &[u8], kind: &[u8]| out.windows(4).position(|at| at == kind).unwrap() - 4;
        let grow = |out: &mut Vec<u8>, at: usize| {
            let value =
                u32::from_be_bytes(out[at..at + 4].try_into().unwrap()) + property.len() as u32;
            out[at..at + 4].copy_from_slice(&value.to_be_bytes());
        };
        let ipco = place(avif, b"ipco");
        let end = ipco + u32::from_be_bytes(avif[ipco..ipco + 4].try_into().unwrap()) as usize;
        let mut out = [&avif[..end], property, &avif[end..]].concat();
        for kind in [b"meta", b"iprp", b"ipco"] {
            let at = place(&out, kind);
            grow(&mut out, at);
        }
        let extent = place(&out, b"iloc") + 22; // the header, the version and flags, the sizes, the count, the item, its data reference, its extents
        grow(&mut out, extent);
        out
    }

    #[test]
    fn no_signature_or_a_refused_header_is_no_picture() {
        // rows S1, S18: a file named by a format's suffix without that format's signature is no picture, as to Pillow 13, whose readers take a file by its signature (QoiImagePlugin and PngImagePlugin _accept; seen 2026-11-16, Pillow 13.0.0 through identifies and open_picture of lib/ascii_image.py:67 and :105 at tag python-final), though the bytes where its header would hold the sides declare more than the limit: a text named .qoi, a QOI header whose signature reads qoiF, a PNG header of 60000 x 60000 px behind a signature zeroed or one byte off; so is a PNG whose header's CRC fails or that is cut to 8 bytes, which the png crate and Pillow refuse (PngImagePlugin ChunkStream.crc: a SyntaxError; chunk_IHDR: `Truncated IHDR chunk`, a ValueError), and an AVIF declaring 15000 x 15000 px without its first box, ftyp, or whose ftyp names no AVIF brand (avif, avis), which mp4parse and libavif refuse (Pillow: `cannot identify image file`); so too a WebP whose canvas is 2^24 px a side behind RIFX for RIFF, and a BMP of 2147483647 x 1 px behind BX for BM (seen 2026-11-16, Pillow 13.0.0)
        let dir = Dir::new("no_signature");
        let big = png_header(60000, 60000);
        assert_eq!(kind(&dir.file("whole.png", &big)), "too large");
        let mut zeroed = big.clone();
        zeroed[..8].fill(0);
        let mut off = big.clone();
        off[1] = b'Q';
        let mut sum = big.clone();
        sum[29] ^= 0xFF; // the header's CRC
        let short = png_chunks(&[
            (b"IHDR", ihdr(60000, 60000, 1, 0)[..8].to_vec()),
            (b"IDAT", zlib(b"")),
            (b"IEND", vec![]),
        ]);
        let avif = quarters_avif(15000, 15000);
        assert_eq!(kind(&dir.file("whole.avif", &avif)), "too large");
        let mut rifx = webp_with(b"abcd", 0, &[], 0);
        rifx[24..30].fill(0xFF); // the canvas's width - 1 and height - 1
        rifx[3] = b'X';
        let mut bx = [
            &b"BM"[..],
            &118u32.to_le_bytes(),
            &[0; 4],
            &54u32.to_le_bytes(),
        ]
        .concat(); // the file's size, two reserved, the pixels' place
        bx.extend(
            [
                &40u32.to_le_bytes()[..],
                &0x7FFF_FFFFi32.to_le_bytes(),
                &1i32.to_le_bytes(),
                &[1, 0, 24, 0],
                &[0; 24],
                &[0; 64],
            ]
            .concat(),
        );
        bx[1] = b'X';
        let mut brandless = avif.clone();
        brandless[8..12].copy_from_slice(b"mif1"); // the major brand, then the compatible ones: mif1 miaf MA1B
        brandless[16..20].copy_from_slice(b"mif1");
        let sides = [20025u32.to_be_bytes(), 20025u32.to_be_bytes()].concat();
        let qoif = [
            &b"qoiF"[..],
            &sides,
            &[4, 0],
            &[0; 8],
            &[0, 0, 0, 0, 0, 0, 0, 1],
        ]
        .concat();
        let cases = [
            (
                "text.qoi",
                b"not a picture at all, a text of some length\n".to_vec(),
            ),
            ("qoif.qoi", qoif),
            ("zeroed.png", zeroed),
            ("off.png", off),
            ("sum.png", sum),
            ("short.png", short),
            ("unboxed.avif", avif[32..].to_vec()), // after the ftyp box
            ("brandless.avif", brandless),
            ("rifx.webp", rifx),
            ("bx.bmp", bx),
        ];
        let seen = cases.map(|(name, data)| {
            let file = dir.file(name, data);
            (name, kind(&file), identifies(&file))
        });
        let none = seen
            .iter()
            .all(|&(_, kind, id)| (kind, id) == ("no image", false));
        assert!(none, "{seen:?}");
    }

    #[test]
    fn an_avif_is_measured_by_its_container_before_it_is_decoded() {
        // rows S18, N7: Pillow 13 measures an AVIF by the size its container declares, the primary item's ispe property, before any picture is decoded (AvifImagePlugin, libavif's parse): tests/golden/picture-quarters.avif declaring 15000 x 15000 px is too large to open and identifies (lib/ascii_image.py:76, :95 at tag python-final; seen 2026-11-15, Pillow 13.0.0); the decoder learns a size only from the picture dav1d decodes whole, here 16 x 8 px; N7: a within-limit one whose major brand Pillow does not take (AvifImagePlugin _accept: avif, avis, mif1, msf1), heic or isom before the AVIF brands, is the built-in decoder's still of 16 x 8 px, where Pillow gave no picture (seen 2026-11-16, Pillow 13.0.0: `cannot identify image file`), while its declared size over the limit is refused before decoding too; a property of 15000 x 15000 px that no item takes is not measured, a still of 16 x 8 px to Pillow too, nor an ispe of the primary item cut short before its sides, which libavif and mp4parse refuse
        let dir = Dir::new("avif_extent");
        let avif = quarters_avif(15000, 15000);
        let file = dir.file("big.avif", &avif);
        assert_eq!((kind(&file), identifies(&file)), ("too large", true));
        assert!(matches!(original_png(&file, 0), Err(NoCopy::TooLarge)));
        let ispe = |w: u32, h: u32| {
            [
                &20u32.to_be_bytes()[..],
                b"ispe",
                &[0; 4],
                &w.to_be_bytes(),
                &h.to_be_bytes(),
            ]
            .concat()
        };
        let quarters = quarters_avif(16, 8);
        let unused = dir.file("unused.avif", with_property(&quarters, &ispe(15000, 15000)));
        assert_eq!(rgb(&still(&unused).pic).dimensions(), (16, 8));
        let mut short = avif.clone();
        let at = short.windows(4).position(|kind| kind == b"ispe").unwrap() - 4;
        short[at..at + 4].copy_from_slice(&12u32.to_be_bytes()); // the header, the version and flags
        let short = dir.file("short.avif", short);
        assert_eq!((kind(&short), identifies(&short)), ("no image", false));
        for brand in [b"heic", b"isom"] {
            let mut small = quarters.clone();
            small[8..12].copy_from_slice(brand);
            let file = dir.file("small-other.avif", small);
            assert_eq!(still(&file).pic.size(), (16, 8), "{brand:?}");
            assert!(identifies(&file));
            assert!(matches!(original_png(&file, 0), Ok((_, (16, 8)))));
            let mut other = avif.clone();
            other[8..12].copy_from_slice(brand);
            let file = dir.file("other.avif", other);
            assert_eq!(
                (kind(&file), identifies(&file)),
                ("too large", true),
                "{brand:?}"
            );
            assert!(matches!(original_png(&file, 0), Err(NoCopy::TooLarge)));
        }
    }

    #[test]
    fn an_avif_is_measured_by_associations_in_any_box() {
        // row S18: a primary item's properties can be named by a later ipma of a distinct version or flags, which mp4parse accepts (read_iprp); its first ispe in association order is measured before decoding, including a property index over 127; the decoder's 16 x 8 px picture is no substitute for the container's declared size, for open, identifies or the original PNG
        let dir = Dir::new("avif_associations");
        let ipma = |version: u8, wide: bool, places: &[u16]| {
            let mut body = vec![version, 0, 0, u8::from(wide)];
            body.extend(u32::from(!places.is_empty()).to_be_bytes());
            if !places.is_empty() {
                body.extend(if version == 0 {
                    1u16.to_be_bytes().to_vec()
                } else {
                    1u32.to_be_bytes().to_vec()
                });
                body.push(places.len() as u8);
                for &place in places {
                    if wide {
                        body.extend(place.to_be_bytes());
                    } else {
                        body.push((place & 0x7F) as u8 | ((place & 0x8000) >> 8) as u8);
                    }
                }
            }
            [
                ((body.len() + 8) as u32).to_be_bytes().as_slice(),
                b"ipma",
                &body,
            ]
            .concat()
        };
        for (version, wide) in [(1, false), (0, false), (0, true)] {
            for (w, h) in [(16, 8), (15000, 15000)] {
                let mut avif = quarters_avif(if wide { 16 } else { w }, if wide { 8 } else { h });
                let first = if wide {
                    let mut padding = [8u32.to_be_bytes().as_slice(), b"free"]
                        .concat()
                        .repeat(125);
                    padding.extend(
                        [
                            20u32.to_be_bytes().as_slice(),
                            b"ispe",
                            &[0; 4],
                            &w.to_be_bytes(),
                            &h.to_be_bytes(),
                        ]
                        .concat(),
                    );
                    avif = with_property(&avif, &padding);
                    130
                } else {
                    1
                };
                let associations = [
                    ipma(u8::from(version == 0), false, &[]),
                    ipma(version, wide, &[first, 2, 0x8003, 4]),
                ]
                .concat();
                let at = avif.windows(4).position(|kind| kind == b"ipma").unwrap() - 4;
                let length = u32::from_be_bytes(avif[at..at + 4].try_into().unwrap()) as usize;
                let extra = (associations.len() - length) as u32;
                avif.splice(at..at + length, associations);
                for kind in [b"meta", b"iprp"] {
                    let at = avif.windows(4).position(|seen| seen == kind).unwrap() - 4;
                    let length = u32::from_be_bytes(avif[at..at + 4].try_into().unwrap()) + extra;
                    avif[at..at + 4].copy_from_slice(&length.to_be_bytes());
                }
                let extent = avif.windows(4).position(|kind| kind == b"iloc").unwrap() + 18;
                let offset =
                    u32::from_be_bytes(avif[extent..extent + 4].try_into().unwrap()) + extra;
                avif[extent..extent + 4].copy_from_slice(&offset.to_be_bytes());
                let file = dir.file(&format!("{version}-{wide}-{w}.avif"), avif);
                assert!(identifies(&file), "{file:?}");
                if w == 15000 {
                    assert_eq!(kind(&file), "too large", "{file:?}");
                    assert!(matches!(original_png(&file, 0), Err(NoCopy::TooLarge)));
                } else {
                    assert_eq!(still(&file).pic.size(), (16, 8));
                    assert!(matches!(original_png(&file, 0), Ok((_, (16, 8)))));
                }
            }
        }
    }

    #[test]
    fn a_gif_reaches_as_far_as_its_first_frame() {
        // row S18: Pillow 13 grows a GIF to its first frame's extent and holds that to the limit (GifImagePlugin _seek, _decompression_bomb_check; seen 2026-11-13, Pillow 13.0.0 through lib/ascii_image.py): on a screen of 8 x 8 px, a first frame of 65288 x 65535 px, of 8 x 8 px at (60000, 60000), or reaching one row past 12470 x 14351 px is too large to open, and identifies (lib/ascii_image.py:76), also behind blocks the decoder refuses and with the file ended before the frame's flags, as is a screen over the limit; one reaching to the limit opens; the decoder knows the screen's size alone and allocates the frame whole, 17 GB for this file of 54 bytes; Pillow measures a file once it has read its first frame: a screen over the limit whose first frame is cut short, and a PNG over it whose acTL is, are refused, no picture to identify; so is a file named .gif without GIF's signature (seen 2026-11-13, Pillow 13.0.0), which is not read by its blocks, though read as a GIF its first frame or its screen is over the limit
        let dir = Dir::new("gif_extent");
        assert_eq!(12470 * 14351, MAX_PIXELS);
        for (name, frame) in [
            ("big.gif", gif_frame(0, 0, 65288, 65535)),
            ("far.gif", gif_frame(60000, 60000, 8, 8)),
            ("past.gif", gif_frame(12462, 14344, 8, 8)),
        ] {
            let file = dir.file(name, gif_of((8, 8), &[&frame]));
            assert_eq!(kind(&file), "too large", "{name}");
            assert!(identifies(&file), "{name}");
        }
        let at = dir.file("at.gif", gif_of((8, 8), &[&gif_frame(12462, 14343, 8, 8)]));
        assert_eq!(kind(&at), "still");
        // read by its blocks, before the decoder, which refuses a block of an unknown label or a graphic control block of 6 bytes, where Pillow reads on, and reads a frame's extent before its flags
        let big = gif_frame(0, 0, 65288, 65535);
        let mut flagless = gif_of((8, 8), &[&big[..9]]);
        flagless.pop(); // the file ends before the descriptor's flags
        for (name, file) in [
            ("unknown.gif", gif_of((8, 8), &[&[0x21, 0, 1, 7, 0], &big])),
            (
                "control-6.gif",
                gif_of((8, 8), &[&[0x21, 0xF9, 6, 0, 0, 0, 0, 0, 0, 0], &big]),
            ),
            (
                "screen.gif",
                gif_of(
                    (20000, 20000),
                    &[&[0x21, 0, 1, 7, 0], &gif_frame(0, 0, 8, 8)],
                ),
            ),
            ("flagless.gif", flagless),
        ] {
            let file = dir.file(name, file);
            assert_eq!(
                (kind(&file), identifies(&file)),
                ("too large", true),
                "{name}"
            );
        }
        let mut cut = gif_of((20000, 20000), &[&gif_frame(0, 0, 8, 8)[..10]]);
        cut.pop(); // the file ends with the descriptor
        let cut = dir.file("cut.gif", cut);
        assert_eq!(kind(&cut), "no image");
        assert!(!identifies(&cut));
        for (name, screen, frame) in [
            ("unsigned.gif", (8, 8), big.clone()),
            ("unsigned-screen.gif", (20000, 20000), gif_frame(0, 0, 8, 8)),
        ] {
            let mut file = gif_of(screen, &[&frame]);
            file[3..6].copy_from_slice(b"00a"); // GIF00a
            let file = dir.file(name, file);
            let seen = (kind(&file), identifies(&file));
            assert_eq!(seen, ("no image", false), "{name}");
        }
        let actl = png_chunks(&[
            (b"IHDR", ihdr(20000, 20000, 8, 2)),
            (b"acTL", 1u32.to_be_bytes().to_vec()),
            (b"IDAT", vec![]),
            (b"IEND", vec![]),
        ]);
        let actl = dir.file("cut-actl.png", actl);
        assert_eq!(kind(&actl), "no image");
        assert!(!identifies(&actl));
    }

    #[test]
    fn a_gif_frame_past_its_screen_is_cut_to_it() {
        // rows S1, N41: a GIF whose first frame reaches past its screen, within the limit, opens as a still of the screen's size, the frame cut to it and transparent outside it (the image crate's decoder): an 8 x 8 frame of red at (4, 4) on a screen of 8 x 8 px; the old tools grew the picture to the frame (Pillow 13, GifImagePlugin _seek): through open_picture of lib/ascii_image.py:99 (tag python-final) this file was a still of 12 x 12 px, palette index 0 outside the frame, red here (seen 2026-11-13, Pillow 13.0.0)
        let dir = Dir::new("gif_past_screen");
        let file = dir.file("past.gif", gif_of((8, 8), &[&gif_frame(4, 4, 8, 8)]));
        let opened = still(&file);
        let picture = rgba(&opened.pic);
        assert_eq!(picture.dimensions(), (8, 8));
        let red = [200, 0, 0, 255];
        assert_eq!(picture.get_pixel(4, 4).0, red); // the frame's corner
        assert_eq!(picture.get_pixel(7, 7).0, red);
        for (x, y) in [(0, 0), (3, 3), (3, 7), (7, 3)] {
            assert_eq!(picture.get_pixel(x, y).0[3], 0, "({x}, {y})"); // outside the frame
        }
    }

    #[test]
    fn a_png_profile_over_a_mebibyte_is_no_picture() {
        // row S1: Pillow 13 inflates a PNG's colour profile (iCCP) to 1 MiB at most and refuses a file whose profile holds more, also one over the limit, as it measures a file once it has read to the image data (PngImagePlugin chunk_iCCP and _safe_zlib_decompress, MAX_TEXT_CHUNK: a ValueError, no picture to identifies and open_picture of lib/ascii_image.py:67 and :105 at tag python-final; seen 2026-11-15, Pillow 13.0.0: a profile of 1 MiB opens, one of 1 MiB and a byte does not, 8 x 8 px or 60000 x 60000 px); a profile that does not inflate is none to it (zlib.error); the png crate inflates the first profile whole while the decoder is made, before any size is known: 2 GiB from a file of 2 MB; it passes over a profile whose CRC fails and inflates the next one, so a small profile of a broken CRC before one over 1 MiB is no picture too, as Pillow refuses a chunk whose CRC fails (PngImagePlugin ChunkStream.crc: a SyntaxError; seen 2026-11-16, Pillow 13.0.0); either is no picture as an icon's PNG too, which Pillow reads as a PNG (IcoImagePlugin IcoFile.frame; seen 2026-11-16, Pillow 13.0.0)
        let dir = Dir::new("png_profile");
        let row = [&[0][..], &[1, 2, 3].repeat(8)].concat(); // filter 0
        let profiled = |(w, h): (u32, u32), profile: &[u8]| {
            png_chunks(&[
                (b"IHDR", ihdr(w, h, 8, 2)),
                (b"iCCP", [&b"p\0\0"[..], profile].concat()),
                (b"IDAT", zlib(&row.repeat(8))),
                (b"IEND", vec![]),
            ])
        };
        let mebibyte = 1 << 20;
        let at = dir.file("at.png", profiled((8, 8), &zlib(&vec![0; mebibyte])));
        assert_eq!(rgb(&still(&at).pic).get_pixel(7, 7).0, [1, 2, 3]);
        let past = zlib(&vec![0; mebibyte + 1]);
        for (name, size) in [("past.png", (8, 8)), ("past-big.png", (60000, 60000))] {
            let file = dir.file(name, profiled(size, &past));
            let seen = (kind(&file), identifies(&file));
            assert_eq!(seen, ("no image", false), "{name}");
        }
        let small = [&b"p\0\0"[..], &zlib(&[0; 4096])].concat();
        let mut second = png_chunks(&[
            (b"IHDR", ihdr(8, 8, 8, 2)),
            (b"iCCP", small.clone()),
            (b"iCCP", [&b"q\0\0"[..], &past].concat()),
            (b"IDAT", zlib(&row.repeat(8))),
            (b"IEND", vec![]),
        ]);
        let crc = 8 + 25 + 8 + small.len(); // past the signature, the header and the first profile's length, kind and data
        second[crc] ^= 0xFF;
        let second = dir.file("second.png", second);
        assert_eq!((kind(&second), identifies(&second)), ("no image", false));
        let mut broken = zlib(&vec![0; 2 * mebibyte]);
        broken[0] = 0; // no zlib header
        let broken = dir.file("broken.png", profiled((8, 8), &broken));
        assert_eq!(rgb(&still(&broken).pic).get_pixel(7, 7).0, [1, 2, 3]);
        // so too in an icon, whose PNG the ICO decoder reads by the png crate without limits (an RGBA PNG, as an icon's has to be)
        let rgba = [&[0][..], &[1, 2, 3, 255].repeat(8)].concat();
        let icon = |profiles: &[&[u8]]| {
            let mut chunks = vec![(b"IHDR", ihdr(8, 8, 8, 6))];
            chunks.extend(profiles.iter().map(|data| (b"iCCP", data.to_vec())));
            chunks.extend([(b"IDAT", zlib(&rgba.repeat(8))), (b"IEND", vec![])]);
            ico_of(&png_chunks(&chunks))
        };
        assert_eq!(kind(&dir.file("plain.ico", icon(&[]))), "still");
        let named = [&b"q\0\0"[..], &past].concat();
        let mut second = icon(&[&small, &named]);
        second[22 + crc] ^= 0xFF; // past the icon's header and its entry
        for (name, file) in [("past.ico", icon(&[&named])), ("second.ico", second)] {
            let file = dir.file(name, file);
            assert_eq!(
                (kind(&file), identifies(&file)),
                ("no image", false),
                "{name}"
            );
        }
    }

    /// An icon of one entry of 8 x 8 px and 32 bits a pixel, `image`: a PNG, or a BMP's header and pixels.
    fn ico_of(image: &[u8]) -> Vec<u8> {
        let mut out = vec![0, 0, 1, 0, 1, 0, 8, 8, 0, 0, 1, 0, 32, 0]; // the type, one entry: its sides, colours, planes, bits
        out.extend((image.len() as u32).to_le_bytes());
        out.extend(22u32.to_le_bytes()); // past the header and the entry
        out.extend(image);
        out
    }

    /// An 8 x 4 WebP of #010203 behind a VP8X of `flags`, then a `kind` chunk of `data` that declares `declared` bytes.
    fn webp_with(kind: &[u8; 4], flags: u8, data: &[u8], declared: u32) -> Vec<u8> {
        let mut frame = Vec::new();
        RgbaImage::from_pixel(8, 4, Rgba([1, 2, 3, 255]))
            .write_with_encoder(WebPEncoder::new_lossless(&mut frame))
            .unwrap();
        let mut body = b"WEBPVP8X\x0a\0\0\0".to_vec();
        body.extend([flags, 0, 0, 0, 7, 0, 0, 3, 0, 0]); // the flags, width - 1, height - 1
        body.extend(&frame[12..]); // the VP8L chunk
        body.extend(kind);
        body.extend(declared.to_le_bytes());
        body.extend(data);
        [&b"RIFF"[..], &(body.len() as u32).to_le_bytes(), &body].concat()
    }

    #[test]
    fn a_webp_whose_metadata_runs_past_its_end_is_no_picture() {
        // rows S1, S2: Pillow 13's libwebp refuses a WebP whose EXIF, ICCP or XMP chunk declares more bytes than the file holds: no picture to identifies and open_picture of lib/ascii_image.py:67 and :105 at tag python-final (seen 2026-11-15, Pillow 13.0.0, a chunk of 64 bytes declaring 4294967040 behind an 8 x 4 frame: `could not create decoder object`, an OSError); the WebP decoder takes such a chunk whole by its declared size, the EXIF for the orientation: 4 GiB allocated before its read fails, an abort past the memory at hand; an EXIF the file holds is read, its orientation a quarter turn right, upright as Pillow shows it
        let dir = Dir::new("webp_metadata");
        for (name, chunk, flag) in [
            ("exif.webp", b"EXIF", 0x08),
            ("iccp.webp", b"ICCP", 0x20),
            ("xmp.webp", b"XMP ", 0x04),
        ] {
            let file = dir.file(name, webp_with(chunk, flag, &[0; 64], 0xFFFF_FF00));
            let seen = (kind(&file), identifies(&file));
            assert_eq!(seen, ("no image", false), "{name}");
        }
        let turned = exif(6); // a quarter right
        let whole = webp_with(b"EXIF", 0x08, &turned, turned.len() as u32);
        let whole = still(&dir.file("whole.webp", whole));
        assert_eq!(whole.pic.size(), (4, 8));
        assert_eq!(rgb(&whole.pic).get_pixel(3, 7).0, [1, 2, 3]);
    }

    #[test]
    fn identifies_by_the_header_alone() {
        // identifies of lib/ascii_image.py:67 at tag python-final, which the folder's steps take (S7, S8: their tests step in tests/stills.rs): a still by its header, also one cut short that then does not open (a GIF too, cut in its only frame's data: one frame to Pillow 13's walk of its blocks, two to decoding), and an SVG by its suffix, unread; an animation of more than one frame (an APNG's default image outside the animation counted, as Pillow counts it), text, a clip or a missing file is passed over, and so is a PNG whose acTL is cut short, a ValueError of Pillow 13 that identifies takes for no still (lib/ascii_image.py:78)
        let dir = Dir::new("identifies");
        let whole = png_of(RgbImage::new(40, 30));
        assert!(identifies(&dir.file("whole.png", &whole)));
        // tests/test_viewer.py:499: a header, cut short
        let cut = dir.file("cut.png", &whole[..60]);
        assert!(identifies(&cut));
        assert_eq!(kind(&cut), "no image");
        let one = gif(1);
        let cut = dir.file("cut.gif", &one[..one.len() - 3]);
        assert!(identifies(&cut));
        assert_eq!(kind(&cut), "no image");
        assert!(!identifies(&dir.file("text.png", "not a picture")));
        assert!(!identifies(&dir.file("cut-actl.png", short_actl())));
        assert!(!identifies(&dir.file("a.gif", gif(3))));
        assert!(!identifies(&dir.file("a.png", apng(2, false))));
        assert!(!identifies(&dir.file("default.png", apng(1, true))));
        assert!(!identifies(&dir.file("a.webp", animated_webp(&[0, 255]))));
        assert!(identifies(&dir.file("one.gif", gif(1))));
        assert!(identifies(&dir.file("one.png", apng(1, false))));
        assert!(identifies(&dir.file("one.webp", animated_webp(&[200]))));
        assert!(!identifies(&dir.file("clip.mp4", [0u8; 64])));
        assert!(!identifies(&dir.0.join("missing.png")));
        assert!(identifies(&dir.0.join("any.svg"))); // by its suffix, unread
    }

    #[test]
    fn listed_by_extension() {
        let known = "jpg jpeg jpe jfif mpo png apng gif webp bmp dib tif tiff ico cur tga icb vda vst qoi pbm pgm ppm pnm pfm dds hdr exr avif svg";
        for extension in known.split(' ') {
            assert!(listed(Path::new(&format!("a.{extension}"))), "{extension}");
            assert!(
                listed(Path::new(&format!("dir.d/A.{}", extension.to_uppercase()))),
                "{extension}"
            );
        }
        for name in [
            "clip.mp4",
            "song.flac",
            "notes.txt",
            "png",
            ".png",
            "a.png.bak",
            "a.",
            "a.pam",
            "a.svgz",
        ] {
            assert!(!listed(Path::new(name)), "{name}");
        }
        assert!(is_svg(Path::new("a.SVG")) && !is_svg(Path::new("svg")));
    }

    #[test]
    fn a_decoder_panic_is_caught() {
        assert_eq!(caught(|| 7), Some(7));
        assert_eq!(caught(|| -> u8 { panic!("a decoder's") }), None);
    }

    /// Bytes in memory that count the reads made of them.
    struct Counted(Cursor<Vec<u8>>, usize);

    impl Read for Counted {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.1 += 1;
            self.0.read(buf)
        }
    }

    impl Seek for Counted {
        fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
            self.0.seek(to)
        }
    }

    #[test]
    fn a_walk_reads_its_file_about_once() {
        // a walk steps from chunk to chunk, block to block or box to box by relative seeks, which keep its reader's buffer: 100000 empty chunks, sub-blocks or boxes cost reads by the file's size, 8 KiB a read, not a read each (a seek from the start drops the buffer: a GIF of 16 MB took 11.7 s to identify, a WebP of 32 MB 39.7 s, seen round 8)
        let many = 100_000;
        let mut chunks = vec![(b"IHDR", ihdr(8, 8, 8, 2))];
        chunks.extend(std::iter::repeat_n((b"abCd", vec![]), many));
        chunks.extend([(b"IDAT", zlib(b"")), (b"IEND", vec![])]);
        let png = png_chunks(&chunks);
        let comment = [&[0x21, 0xFE][..], &[1, b'.'].repeat(many), &[0]].concat();
        let gif = gif_of((8, 8), &[&comment, &gif_frame(0, 0, 8, 8)]);
        let mut webp = webp_with(b"abcd", 0, &[], 0);
        webp.extend(b"abcd\0\0\0\0".repeat(many));
        let riff = (webp.len() as u32 - 8).to_le_bytes();
        webp[4..8].copy_from_slice(&riff);
        let quarters = include_bytes!("../tests/golden/picture-quarters.avif");
        let free = b"\0\0\0\x08free".repeat(many);
        let avif = [&quarters[..32], &free, &quarters[32..]].concat(); // after the ftyp box
        type Walked = fn(&mut BufReader<Counted>) -> std::result::Result<u32, Not>;
        let walks: [(&str, Vec<u8>, Walked); 4] = [
            ("png", png, |file| png_frames(file)),
            ("gif", gif, |file| gif_frames(file)),
            ("webp", webp, |file| webp_frames(file)),
            ("avif", avif, |file| avif_frames(file)),
        ];
        // (walk, reads, reads at most)
        let seen = walks.map(|(name, bytes, walk)| {
            let size = bytes.len();
            let mut file = BufReader::new(Counted(Cursor::new(bytes), 0));
            assert!(matches!(walk(&mut file), Ok(1)), "{name}");
            (name, file.get_ref().1, size / 4096 + 8)
        });
        let within = seen.iter().all(|(_, reads, most)| reads <= most);
        assert!(within, "{seen:?}");
    }

    // tests/test_viewer.py:815; the checksum: zlib.crc32 of the viewer's checkerboard((37, 21))
    #[test]
    fn checkerboard_squares_of_8_px() {
        let board = checkerboard(24, 16);
        let at = |x, y| board.get_pixel(x, y).0;
        let (dark, light) = ([0x66; 3], [0x99; 3]);
        assert_eq!(
            [at(0, 0), at(7, 7), at(8, 0), at(0, 8), at(8, 8), at(23, 15)],
            [dark, dark, light, light, dark, light]
        );
        assert_eq!(crc(checkerboard(37, 21).as_raw()), 3041628301);
    }

    // expected: Pillow's rotate(-90 * turns, expand=True) of the same picture
    #[test]
    fn turned_clockwise_by_quarters() {
        let source = RgbImage::from_fn(3, 2, |x, y| [(y * 3 + x) as u8, 100, 200].into());
        let expected = [
            ((3, 2), "012345"),
            ((2, 3), "304152"),
            ((3, 2), "543210"),
            ((2, 3), "251403"),
        ];
        for (turns, (size, reds)) in (0..).zip(expected) {
            let pic = turned(&Pic::Rgb(source.clone()), turns);
            assert_eq!(layout(rgb(&pic)), (size, reds.to_string()), "{turns} turns");
            assert_eq!(
                layout(rgb(&turned(&Pic::Rgb(source.clone()), turns + 4))).1,
                reds
            );
        }
        let mut clear = RgbaImage::new(3, 2);
        clear.put_pixel(0, 0, Rgba([1, 2, 3, 4]));
        assert_eq!(
            rgba(&turned(&Pic::Rgba(clear), 1)).get_pixel(1, 0).0,
            [1, 2, 3, 4]
        ); // top left -> top right
    }

    /// The whole picture scaled to `size`, bicubic.
    fn fitted(pic: &Pic, size: (u32, u32), ground: Ground) -> RgbImage {
        let (w, h) = pic.size();
        cut(pic, [0.0, 0.0, w as f64, h as f64], size, false, ground)
    }

    /// The box enlarged to `size` by nearest neighbour, over the checkerboard.
    fn zoomed(pic: &Pic, bbox: [f64; 4], size: (u32, u32)) -> RgbImage {
        cut(pic, bbox, size, true, Ground::Checker)
    }

    // tests/test_viewer.py:1092
    #[test]
    fn cut_enlarges_by_nearest_and_reduces_smoothly() {
        let checker = |w, h, cell_w, cell_h| {
            let value = move |x, y| 255 * ((x / cell_w + y / cell_h) & 1) as u8;
            Pic::Rgb(RgbImage::from_fn(w, h, |x, y| [value(x, y); 3].into()))
        };
        let shown = zoomed(&checker(40, 30, 1, 1), [0.0, 0.0, 40.0, 30.0], (160, 120));
        assert_eq!(shown.dimensions(), (160, 120));
        assert!(shown.as_raw().iter().all(|&v| v == 0 || v == 255));
        // each image px a 4 x 4 block: the first row's red, every fourth px
        let first = shown.as_raw()[..160 * 3].iter().step_by(12).copied();
        assert!(first.eq((0..40).map(|x| 255 * (x & 1))));
        let halved = fitted(&checker(1800, 1200, 45, 40), (900, 600), Ground::Checker);
        let values: std::collections::BTreeSet<u8> = halved.as_raw().iter().copied().collect();
        assert!(values.len() > 2, "blended");
    }

    // tests/test_viewer.py:1054
    #[test]
    fn cut_shows_the_box() {
        let (red, blue) = ([255, 0, 0], [0, 0, 255]);
        let quarter = |x, y| if x < 900 && y < 600 { red } else { blue };
        let picture = Pic::Rgb(RgbImage::from_fn(1800, 1200, |x, y| quarter(x, y).into()));
        let fit = fitted(&picture, (900, 600), Ground::Checker);
        assert_eq!(fit.dimensions(), (900, 600));
        assert_eq!(fit.get_pixel(0, 0).0, red);
        assert_eq!(fit.get_pixel(899, 599).0, blue);
        for nearest in [true, false] {
            let shown = |bbox| colours(&cut(&picture, bbox, (900, 600), nearest, Ground::Checker));
            assert_eq!(shown([225.0, 150.0, 675.0, 450.0]), [red].into());
            assert_eq!(shown([1125.0, 750.0, 1575.0, 1050.0]), [blue].into());
        }
    }

    // tests/test_viewer.py:878
    #[test]
    fn cut_puts_transparency_over_the_colour() {
        // clear, but for an opaque black square at 30..70
        let alpha = |x, y| 255 * ((30..70).contains(&x) && (30..70).contains(&y)) as u8;
        let picture = RgbaImage::from_fn(100, 100, |x, y| Rgba([0, 0, 0, alpha(x, y)]));
        let picture = Pic::Rgba(picture);
        let whole = fitted(&picture, (100, 100), Ground::Colour(GREY));
        assert_eq!(colours(&whole), [[0, 0, 0], GREY].into());
        let reduced = fitted(&picture, (50, 50), Ground::Colour(GREY));
        assert_eq!(reduced.get_pixel(2, 2).0, GREY);
        assert_eq!(reduced.get_pixel(25, 25).0, [0, 0, 0]);
        // the edge: black through the alpha, no other colour
        let greys = |&[r, g, b]: &[u8; 3]| r == g && g == b && r <= 128;
        assert!(colours(&reduced).iter().all(greys));
    }

    // tests/test_viewer.py:1146
    #[test]
    fn cut_puts_transparency_over_a_checkerboard_in_screen_px() {
        let picture = Pic::Rgba(RgbaImage::new(900, 600));
        let fit = fitted(&picture, (900, 600), Ground::Checker);
        assert_eq!(fit, checkerboard(900, 600));
        // squares stay 8 screen px, however far in
        let zoom = zoomed(&picture, [131.8, 87.9, 768.2, 512.1], (900, 600));
        assert_eq!(zoom, fit);
    }

    // the checksums: zlib.crc32 of the viewer's cut() on the same picture, box and size (scale 4.7: nearest)
    // no output px centre lies on a px boundary here: there Pillow and fast_image_resize may take either side
    #[test]
    fn cut_by_nearest_is_the_viewers() {
        let picture = formula(17, 13);
        let (bbox, size) = ([2.3, 1.7, 13.9, 9.2], (55, 35));
        let plain = zoomed(&Pic::Rgb(opaque(&picture)), bbox, size);
        assert_eq!(crc(plain.as_raw()), 3286408094);
        let clear = Pic::Rgba(picture);
        assert_eq!(crc(zoomed(&clear, bbox, size).as_raw()), 2692167904);
        let over_grey = cut(&clear, bbox, size, true, Ground::Colour(GREY));
        assert_eq!(crc(over_grey.as_raw()), 2344486725);
        // the whole picture at its own size: no resampling, each alpha blended as stored
        let whole = fitted(&clear, (17, 13), Ground::Checker);
        assert_eq!(crc(whole.as_raw()), 1380037019);
    }

    #[test]
    fn cut_keeps_the_box_inside_the_picture() {
        let picture = Pic::Rgb(opaque(&formula(100, 50)));
        let whole = zoomed(&picture, [0.0, 0.0, 100.0, 50.0], (100, 50));
        assert_eq!(&whole, rgb(&picture));
        let beyond = zoomed(&picture, [-5.0, -5.0, 105.0, 55.0], (100, 50));
        assert_eq!(beyond, whole);
        // a box that ends at the edge, from any start, is drawn
        for start in (1..1000).map(|i| i as f64 / 10.0) {
            let side = zoomed(&picture, [start, 0.0, 100.0, 50.0], (7, 50));
            assert!(side.as_raw().iter().any(|&v| v != 0), "{start}");
        }
        // no number: no panic
        let odd = zoomed(&picture, [f64::NAN, 0.0, 100.0, 50.0], (4, 4));
        assert_eq!(odd.dimensions(), (4, 4));
    }

    #[test]
    fn box_filter_is_pillows() {
        // flat 4 x 4 blocks: the mean is the block's colour
        let block = |x, y| [(x * 40) as u8, (y * 90) as u8, 7];
        let blocks = RgbImage::from_fn(12, 8, |x, y| block(x / 4, y / 4).into());
        let means = RgbImage::from_fn(3, 2, |x, y| block(x, y).into());
        assert_eq!(resize_box(&blocks, 3, 2), means);
        let flat = RgbImage::from_pixel(640, 400, [10, 200, 77].into());
        assert_eq!(
            colours(&resize_box(&flat, 160, 100)),
            [[10, 200, 77]].into()
        );
        // Pillow's resize(Image.BOX) of the same pictures: the bytes, or their FNV-1a hash
        let small = resize_box(&opaque(&formula(12, 8)), 3, 2);
        let pillow = [
            73, 100, 14, 181, 128, 50, 81, 156, 86, 141, 128, 50, 121, 108, 14, 133, 136, 122,
        ];
        assert_eq!(small.as_raw(), &pillow);
        assert_eq!(box_filter(&[0, 1, 2, 1, 2, 3], 2, 1, 1, 1), [1, 2, 3]);
        assert_eq!(
            box_filter(&[0, 0, 0, 1, 1, 1, 2, 2, 2], 3, 1, 2, 1),
            [1, 1, 1, 2, 2, 2]
        );
        assert_eq!(box_filter(&[7, 8, 9], 1, 1, 2, 2), [7, 8, 9].repeat(4));
        let pattern = |w: usize, h: usize| -> Vec<u8> {
            (0..w * h * 3).map(|i| (i * 7919 % 251) as u8).collect()
        };
        for ((w, h), (ow, oh), hash) in [
            ((37, 23), (20, 12), 0x3121_9cfe_4171_c83a),
            ((7, 5), (12, 8), 0x66c0_4872_64a1_0d43),
            ((16, 8), (8, 4), 0x9094_5ccd_ac0f_9f0e),
            ((30, 20), (12, 8), 0xc879_3067_1827_de96),
            ((1, 1), (4, 4), 0x3b76_a1a9_a213_d495),
            ((5, 7), (4, 4), 0xfe6e_9de2_0fa9_6627),
            ((9, 4), (4, 8), 0x8485_f521_c283_7bb5),
            ((1920, 1080), (1280, 720), 0x043a_2ef8_b3e4_638e),
            ((640, 360), (640, 90), 0x0cde_5960_9eaf_63c4),
            ((333, 211), (100, 211), 0xba04_cc7a_e414_f4ad),
            // Pillow 13 goes along y first here, and the other order rounds some bytes apart
            ((90, 160), (64, 51), 0x0f3d_6031_f3f2_fd7a),
            ((900, 1600), (640, 508), 0xbdd2_58ba_1235_99df),
        ] {
            assert_eq!(
                fnv(box_filter(&pattern(w, h), w, h, ow, oh)),
                hash,
                "{w} x {h} to {ow} x {oh}"
            );
        }
    }

    #[test]
    fn the_pass_order_is_pillow_13s() {
        // rows L6, G7: libImaging/Resample.c of Pillow 13, ImagingResampleInner: along y first when the height shrinks by more than twice what the width does
        assert!(vertical_first(900, 1600, 640, 508));
        assert!(
            !vertical_first(100, 160, 70, 100),
            "a tie goes along x first"
        );
        assert!(vertical_first(100, 161, 70, 100));
        assert!(
            vertical_first(50, 160, 60, 51),
            "a widened width counts below zero"
        );
        assert!(
            !vertical_first(1280, 720, 640, 808),
            "a taller output goes along x first"
        );
        assert!(!vertical_first(640, 360, 320, 360));
    }

    #[test]
    fn png_holds_the_pixels() {
        let picture = opaque(&formula(31, 17));
        let bytes = png(&picture).unwrap();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&bytes[16..26], [0, 0, 0, 31, 0, 0, 0, 17, 8, 2]); // 31 x 17, 8 bits, RGB
        let back = image::load_from_memory(&bytes).unwrap();
        assert_eq!(back.into_rgb8(), picture);
    }

    /// The copy `y` makes of the file, decoded.
    fn copied(path: &Path, turns: u8) -> DynamicImage {
        let Ok((bytes, size)) = original_png(path, turns) else {
            panic!("{path:?}: no copy")
        };
        assert_eq!(image::guess_format(&bytes).unwrap(), ImageFormat::Png);
        let picture = image::load_from_memory(&bytes).unwrap();
        assert_eq!((picture.width(), picture.height()), size);
        picture
    }

    fn shape(picture: &DynamicImage) -> (ColorType, u32, u32) {
        (picture.color(), picture.width(), picture.height())
    }

    // tests/test_viewer.py:766
    #[test]
    fn original_png_native_size_alpha_and_turn() {
        let dir = Dir::new("original_native");
        let mut picture = RgbaImage::from_pixel(3000, 1000, Rgba([255, 0, 0, 128]));
        picture.put_pixel(0, 0, Rgba([0, 0, 255, 255]));
        let src = dir.file("big.png", png_of(picture.clone()));
        assert_eq!(copied(&src, 0), DynamicImage::ImageRgba8(picture));
        let turned = copied(&src, 1);
        assert_eq!(shape(&turned), (ColorType::Rgba8, 1000, 3000));
        // top left -> top right
        let corner = turned.as_rgba8().unwrap().get_pixel(999, 0);
        assert_eq!(corner.0, [0, 0, 255, 255]);
    }

    // tests/test_viewer.py:776, :783, :788
    #[test]
    fn original_png_upright_rgb_and_svg() {
        let dir = Dir::new("original_kinds");
        let cam = dir.file("cam.jpg", jpeg(&RgbImage::new(400, 200), Some(6)));
        assert_eq!(copied(&cam, 0).width(), 200);
        assert_eq!(copied(&cam, 3).width(), 400);
        let cmyk: &[u8] = include_bytes!("../tests/golden/picture-cmyk.jpg");
        let copy = copied(&dir.file("print.jpg", cmyk), 0);
        assert_eq!(shape(&copy), (ColorType::Rgb8, 50, 40));
        let icon = r#"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="32"><rect width="64" height="32"/></svg>"#;
        let copy = copied(&dir.file("icon.svg", icon), 0);
        assert_eq!(shape(&copy), (ColorType::Rgba8, 64, 32));
    }

    #[test]
    fn original_png_keeps_what_png_holds_in_8_bits_or_16() {
        let dir = Dir::new("original_modes");
        // a palette and a 1-bit picture come as 8 bits a channel (row N20)
        let copy = copied(&dir.file("pal.png", palette_png()), 0);
        assert_eq!(copy.color(), ColorType::Rgba8);
        assert_eq!(copy.as_rgba8().unwrap().get_pixel(0, 0).0, [0, 0, 0, 0]);
        let bits = png_chunks(&[
            (b"IHDR", ihdr(8, 2, 1, 0)),
            (b"IDAT", zlib(&[0, 0b1010_1010, 0, 0b0101_0101])),
            (b"IEND", vec![]),
        ]);
        let copy = copied(&dir.file("bits.png", bits), 0);
        assert_eq!(copy.color(), ColorType::L8);
        assert_eq!(copy.as_bytes()[..3], [255, 0, 255]);
        let grey = image::GrayImage::from_fn(4, 4, |x, y| [(x * 16 + y) as u8].into());
        let copy = copied(&dir.file("grey.png", png_of(grey.clone())), 0);
        assert_eq!(copy, DynamicImage::ImageLuma8(grey));
        let deep =
            image::ImageBuffer::<image::Rgb<u16>, _>::from_pixel(4, 4, [0xffff, 0x1234, 0].into());
        let copy = copied(&dir.file("deep.png", png_of(deep.clone())), 0);
        assert_eq!(copy, DynamicImage::ImageRgb16(deep));
        // an opaque alpha band stays: the copy is the file's picture, not the screen's
        let solid = RgbaImage::from_pixel(4, 4, Rgba([9, 8, 7, 255]));
        let copy = copied(&dir.file("solid.png", png_of(solid)), 0);
        assert_eq!(copy.color(), ColorType::Rgba8);
    }

    #[test]
    fn original_png_fails_for_what_is_no_still() {
        let dir = Dir::new("original_fails");
        let refused = |path: PathBuf| match original_png(&path, 0) {
            Ok(_) => "copied",
            Err(NoCopy::TooLarge) => "too large",
            Err(NoCopy::Failed) => "failed",
        };
        assert_eq!(refused(dir.file("x.png", "not a picture")), "failed");
        assert_eq!(refused(dir.file("a.gif", gif(3))), "failed");
        assert_eq!(refused(dir.0.join("missing.svg")), "failed");
        let vast = r#"<svg xmlns="http://www.w3.org/2000/svg" width="20000" height="10000"/>"#;
        assert_eq!(refused(dir.file("vast.svg", vast)), "too large");
        assert_eq!(
            refused(dir.file("big.png", png_header(12470, 14352))),
            "too large"
        );
    }
}

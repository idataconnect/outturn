//! The images a document draws, sent beside its markdown.
//!
//! The renderer reaches nothing, so whoever asks sends the bytes of every
//! image the markdown names, keyed by the destination it names them by. What
//! is not sent is not drawn, and its alt text stands in.
//!
//! A JPEG goes into the PDF as it is, never decoded: a phone photo costs its
//! file size and nothing more. PNG, GIF and WebP are decoded to be written, and
//! a decoded image is the one thing here whose memory is not bounded by the
//! bytes sent -- a small file can unpack to a vast bitmap -- so their pixels are
//! counted from the header first, and refused before any are decoded.

use std::cell::RefCell;
use std::collections::HashMap;

use krilla::Data;
use krilla::geom::Transform;
use krilla::image::Image;

/// The most images one document may send.
pub const MAX_IMAGES: usize = 32;

/// The most image bytes one document may send, all images together.
pub const MAX_IMAGE_BYTES: usize = 24 * 1024 * 1024;

/// The most pixels in one image that is decoded to be written. Sixteen
/// million is a large screenshot or chart several times over, and decodes to
/// 64 MB at most; a JPEG is not decoded and is not held to it.
pub const MAX_PIXELS: u64 = 16_000_000;

/// The content type of a document sent with its images. Each part is a name,
/// then its bytes: a big-endian u16 length and the name in UTF-8, a big-endian
/// u32 length and the bytes. The first part is the markdown, named "".
pub const BUNDLE: &str = "application/x-outturn-bundle";

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Png,
    Jpeg,
    Gif,
    Webp,
}

/// An image's size and which way up it is, read without decoding it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Picture {
    kind: Kind,
    /// As stored, before any turn.
    stored: (u32, u32),
    /// The EXIF orientation, 1 to 8: how a camera that held the phone on its
    /// side says the stored pixels should be turned to be upright.
    orientation: u8,
}

impl Picture {
    /// Its size the right way up, in pixels.
    pub(crate) fn upright(&self) -> (u32, u32) {
        let (w, h) = self.stored;
        if self.orientation >= 5 {
            (h, w)
        } else {
            (w, h)
        }
    }

    /// Where the stored image goes so that it is upright in the box at
    /// `(x, y)`, `w` by `h`. krilla draws an image into its own size, so the
    /// turn is applied to that size and then scaled to the box.
    pub(crate) fn placement(&self, x: f32, y: f32, w: f32, h: f32) -> (Transform, (f32, f32)) {
        // The stored image is drawn `iw` by `ih`; once turned it fills `w` by `h`.
        let (iw, ih) = if self.orientation >= 5 {
            (h, w)
        } else {
            (w, h)
        };
        // u = sx*a + kx*b + tx, v = ky*a + sy*b + ty, for a point (a, b) of
        // the stored image and (u, v) of the box.
        let (sx, ky, kx, sy, tx, ty) = match self.orientation {
            2 => (-1.0, 0.0, 0.0, 1.0, iw, 0.0),
            3 => (-1.0, 0.0, 0.0, -1.0, iw, ih),
            4 => (1.0, 0.0, 0.0, -1.0, 0.0, ih),
            5 => (0.0, 1.0, 1.0, 0.0, 0.0, 0.0),
            6 => (0.0, 1.0, -1.0, 0.0, ih, 0.0),
            7 => (0.0, -1.0, -1.0, 0.0, ih, iw),
            8 => (0.0, -1.0, 1.0, 0.0, 0.0, iw),
            _ => (1.0, 0.0, 0.0, 1.0, 0.0, 0.0),
        };
        (
            Transform::from_row(sx, ky, kx, sy, x + tx, y + ty),
            (iw, ih),
        )
    }
}

/// The images sent with one document.
#[derive(Default)]
pub struct Images {
    files: HashMap<String, Data>,
    /// Each image as krilla holds it, made once however often it is drawn:
    /// making one is what decodes it.
    made: RefCell<HashMap<String, Image>>,
}

impl Images {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, name: String, bytes: Vec<u8>) {
        self.files.insert(name, bytes.into());
    }

    pub(crate) fn has(&self, name: &str) -> bool {
        self.files.contains_key(name)
    }

    /// What `name` is, or why it cannot be drawn.
    pub(crate) fn measure(&self, name: &str) -> Result<Picture, String> {
        let bytes = self
            .files
            .get(name)
            .ok_or_else(|| format!("{name} was not sent"))?
            .as_ref();
        let unreadable = || format!("{name} could not be read as an image");
        let kind = match imagesize::image_type(bytes).map_err(|_| unreadable())? {
            imagesize::ImageType::Png => Kind::Png,
            imagesize::ImageType::Jpeg => Kind::Jpeg,
            imagesize::ImageType::Gif => Kind::Gif,
            imagesize::ImageType::Webp => Kind::Webp,
            _ => {
                return Err(format!(
                    "{name} is not an image that can be drawn; PNG, JPEG, GIF and WebP can"
                ));
            }
        };
        let size = imagesize::blob_size(bytes).map_err(|_| unreadable())?;
        let stored = (size.width as u32, size.height as u32);
        if stored.0 == 0 || stored.1 == 0 {
            return Err(unreadable());
        }
        let pixels = u64::from(stored.0) * u64::from(stored.1);
        if kind != Kind::Jpeg && pixels > MAX_PIXELS {
            return Err(format!(
                "{name} is {} by {} pixels; an image is drawn from at most {MAX_PIXELS} pixels, \
                 or any size as a JPEG",
                stored.0, stored.1
            ));
        }
        let orientation = if kind == Kind::Jpeg {
            exif_orientation(bytes).unwrap_or(1)
        } else {
            1
        };
        Ok(Picture {
            kind,
            stored,
            orientation,
        })
    }

    /// The image as krilla draws it, made the first time it is asked for.
    pub(crate) fn image(&self, name: &str, picture: &Picture) -> Result<Image, String> {
        if let Some(made) = self.made.borrow().get(name) {
            return Ok(made.clone());
        }
        let data = self
            .files
            .get(name)
            .ok_or_else(|| format!("{name} was not sent"))?
            .clone();
        let made = match picture.kind {
            Kind::Png => Image::from_png(data, true),
            Kind::Jpeg => Image::from_jpeg(data, true),
            Kind::Gif => Image::from_gif(data, true),
            Kind::Webp => Image::from_webp(data, true),
        }
        .map_err(|_| format!("{name} could not be read as an image"))?;
        self.made
            .borrow_mut()
            .insert(name.to_string(), made.clone());
        Ok(made)
    }
}

/// Splits a bundle into its markdown and its images, refusing one over the
/// limits before anything in it is used.
pub fn unbundle(body: &[u8]) -> Result<(String, Images), String> {
    let malformed = || "the request is not a well-formed bundle".to_string();
    let mut at = 0;
    let mut take = |n: usize| -> Result<&[u8], String> {
        let part = body.get(at..at + n).ok_or_else(malformed)?;
        at += n;
        Ok(part)
    };
    let mut markdown = None;
    let mut images = Images::new();
    let mut total = 0;
    loop {
        let head = match take(2) {
            Ok(h) => h,
            Err(_) if markdown.is_some() => break,
            Err(e) => return Err(e),
        };
        let name_len = u16::from_be_bytes([head[0], head[1]]) as usize;
        let name = String::from_utf8(take(name_len)?.to_vec()).map_err(|_| malformed())?;
        let len = take(4)?;
        let len = u32::from_be_bytes([len[0], len[1], len[2], len[3]]) as usize;
        let bytes = take(len)?.to_vec();
        match markdown {
            None if name.is_empty() => {
                markdown = Some(
                    String::from_utf8(bytes)
                        .map_err(|_| "the markdown is not UTF-8".to_string())?,
                );
            }
            None => return Err(malformed()),
            Some(_) => {
                total += bytes.len();
                if images.files.len() == MAX_IMAGES {
                    return Err(format!(
                        "a document may draw at most {MAX_IMAGES} different images"
                    ));
                }
                if total > MAX_IMAGE_BYTES {
                    return Err(format!(
                        "the images come to more than {MAX_IMAGE_BYTES} bytes; a document's \
                         images may come to at most that"
                    ));
                }
                images.insert(name, bytes);
            }
        }
    }
    Ok((markdown.unwrap_or_default(), images))
}

/// The orientation a JPEG's EXIF data gives, if it gives one.
///
/// A phone stores a photo as its sensor saw it and says in EXIF how to turn
/// it, so a portrait photo is stored on its side. A JPEG is written into the
/// PDF without being decoded, so the turn has to be read here and applied
/// when it is drawn, or every portrait photo lies down.
fn exif_orientation(jpeg: &[u8]) -> Option<u8> {
    let mut at = 2; // past the start-of-image marker
    while at + 4 <= jpeg.len() {
        if jpeg[at] != 0xFF {
            return None;
        }
        let marker = jpeg[at + 1];
        // Start of scan: the headers are over.
        if marker == 0xDA {
            return None;
        }
        let len = u16::from_be_bytes([jpeg[at + 2], jpeg[at + 3]]) as usize;
        let segment = jpeg.get(at + 4..at + 2 + len)?;
        if marker == 0xE1
            && let Some(tiff) = segment.strip_prefix(b"Exif\0\0")
        {
            return tiff_orientation(tiff);
        }
        at += 2 + len;
    }
    None
}

fn tiff_orientation(tiff: &[u8]) -> Option<u8> {
    let big = match tiff.get(0..2)? {
        b"MM" => true,
        b"II" => false,
        _ => return None,
    };
    let u16_at = |i: usize| -> Option<u16> {
        let b = tiff.get(i..i + 2)?;
        Some(if big {
            u16::from_be_bytes([b[0], b[1]])
        } else {
            u16::from_le_bytes([b[0], b[1]])
        })
    };
    let u32_at = |i: usize| -> Option<u32> {
        let b = tiff.get(i..i + 4)?;
        Some(if big {
            u32::from_be_bytes([b[0], b[1], b[2], b[3]])
        } else {
            u32::from_le_bytes([b[0], b[1], b[2], b[3]])
        })
    };
    let ifd = u32_at(4)? as usize;
    let entries = u16_at(ifd)? as usize;
    for n in 0..entries {
        let entry = ifd + 2 + n * 12;
        if u16_at(entry)? == 0x0112 {
            let value = u16_at(entry + 8)?;
            return (1..=8).contains(&value).then_some(value as u8);
        }
    }
    None
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A bundle, as a caller would write one.
    pub(crate) fn bundle(markdown: &str, images: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut part = |name: &str, bytes: &[u8]| {
            out.extend_from_slice(&(name.len() as u16).to_be_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
            out.extend_from_slice(bytes);
        };
        part("", markdown.as_bytes());
        for (name, bytes) in images {
            part(name, bytes);
        }
        out
    }

    /// A PNG of `w` by `h` pixels, all one color.
    pub(crate) fn png(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, w, h);
        encoder.set_color(png::ColorType::Rgb);
        let mut writer = encoder.write_header().unwrap();
        writer
            .write_image_data(&vec![0x44; (w * h * 3) as usize])
            .unwrap();
        drop(writer);
        out
    }

    /// The smallest JPEG header that says how big it is and which way up,
    /// with no image after it: enough to measure.
    fn jpeg_header(w: u16, h: u16, orientation: u16, big_endian: bool) -> Vec<u8> {
        let u16b = |v: u16| {
            if big_endian {
                v.to_be_bytes()
            } else {
                v.to_le_bytes()
            }
        };
        let u32b = |v: u32| {
            if big_endian {
                v.to_be_bytes()
            } else {
                v.to_le_bytes()
            }
        };
        let mut tiff = if big_endian {
            b"MM".to_vec()
        } else {
            b"II".to_vec()
        };
        tiff.extend_from_slice(&u16b(42));
        tiff.extend_from_slice(&u32b(8));
        tiff.extend_from_slice(&u16b(1));
        tiff.extend_from_slice(&u16b(0x0112));
        tiff.extend_from_slice(&u16b(3));
        tiff.extend_from_slice(&u32b(1));
        tiff.extend_from_slice(&u16b(orientation));
        tiff.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend(tiff);
        let mut out = vec![0xFF, 0xD8, 0xFF, 0xE1];
        out.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
        out.extend(app1);
        // A baseline frame header: precision, height, width, one component.
        out.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x0B, 0x08]);
        out.extend_from_slice(&h.to_be_bytes());
        out.extend_from_slice(&w.to_be_bytes());
        out.extend_from_slice(&[0x01, 0x01, 0x11, 0x00]);
        out.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x02]);
        out
    }

    fn with(name: &str, bytes: Vec<u8>) -> Images {
        let mut images = Images::new();
        images.insert(name.into(), bytes);
        images
    }

    #[test]
    fn a_bundle_comes_apart_as_it_was_put_together() {
        let body = bundle("# Karl\n", &[("session/a.png", b"one"), ("b.jpg", b"two")]);
        let (markdown, images) = unbundle(&body).unwrap();
        assert_eq!(markdown, "# Karl\n");
        assert_eq!(images.files["session/a.png"].as_ref(), b"one");
        assert_eq!(images.files["b.jpg"].as_ref(), b"two");
    }

    #[test]
    fn a_truncated_bundle_is_refused() {
        let body = bundle("# Karl\n", &[("session/a.png", b"one")]);
        assert!(unbundle(&body[..body.len() - 1]).is_err());
    }

    #[test]
    fn a_bundle_past_the_image_limits_is_refused() {
        let many: Vec<(String, &[u8])> = (0..=MAX_IMAGES)
            .map(|i| (format!("{i}.png"), &b"x"[..]))
            .collect();
        let refs: Vec<(&str, &[u8])> = many.iter().map(|(n, b)| (n.as_str(), *b)).collect();
        assert!(unbundle(&bundle("", &refs)).is_err());
        let big = vec![0u8; MAX_IMAGE_BYTES + 1];
        assert!(unbundle(&bundle("", &[("big.png", &big)])).is_err());
    }

    #[test]
    fn a_png_is_measured_without_being_decoded() {
        let images = with("a.png", png(30, 20));
        let picture = images.measure("a.png").unwrap();
        assert_eq!(picture.upright(), (30, 20));
        assert!(images.made.borrow().is_empty());
    }

    #[test]
    fn a_decoded_image_past_the_pixel_cap_is_refused_and_a_jpeg_is_not() {
        // A PNG's header is all it takes to know, so a fake one says enough.
        let mut header = png(1, 1);
        header[16..20].copy_from_slice(&5000u32.to_be_bytes());
        header[20..24].copy_from_slice(&5000u32.to_be_bytes());
        let err = with("huge.png", header).measure("huge.png").unwrap_err();
        assert!(err.contains("5000 by 5000"), "{err}");

        let jpeg = jpeg_header(5000, 5000, 1, true);
        assert!(with("huge.jpg", jpeg).measure("huge.jpg").is_ok());
    }

    #[test]
    fn a_photo_taken_on_its_side_is_turned_upright() {
        for big_endian in [true, false] {
            let jpeg = jpeg_header(400, 300, 6, big_endian);
            let picture = with("p.jpg", jpeg).measure("p.jpg").unwrap();
            assert_eq!(picture.orientation, 6);
            assert_eq!(picture.upright(), (300, 400));
        }
    }

    #[test]
    fn a_turned_image_lands_inside_its_box() {
        for orientation in 1..=8 {
            let picture = Picture {
                kind: Kind::Jpeg,
                stored: (40, 30),
                orientation,
            };
            let (w, h) = (picture.upright().0 as f32, picture.upright().1 as f32);
            let (transform, (iw, ih)) = picture.placement(100.0, 200.0, w, h);
            for (a, b) in [(0.0, 0.0), (iw, 0.0), (0.0, ih), (iw, ih)] {
                let mut p = krilla::geom::Point::from_xy(a, b);
                let t = transform;
                p = krilla::geom::Point::from_xy(
                    t.sx() * p.x + t.kx() * p.y + t.tx(),
                    t.ky() * p.x + t.sy() * p.y + t.ty(),
                );
                assert!(
                    (100.0..=100.0 + w).contains(&p.x) && (200.0..=200.0 + h).contains(&p.y),
                    "orientation {orientation} put a corner at {p:?}"
                );
            }
        }
    }

    #[test]
    fn what_is_not_an_image_is_named() {
        let err = with("notes.txt", b"hello".to_vec())
            .measure("notes.txt")
            .unwrap_err();
        assert!(err.contains("notes.txt"), "{err}");
    }
}

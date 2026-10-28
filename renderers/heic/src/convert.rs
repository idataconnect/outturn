//! A HEIC photo, as a JPEG.
//!
//! Decoding is libheif's, linked from the system rather than built here: the
//! HEVC decoder it needs is a codec nobody should be reimplementing.

use libheif_rs::{ColorSpace, HeifContext, LibHeif, RgbChroma};

/// The longest side a converted photo keeps.
///
/// Vision models scale anything larger down before they look, so pixels past
/// this are bytes sent for nothing -- and a full-resolution phone photo as
/// JPEG can pass a provider's per-image limit on its own. Enough for a
/// browser to show it full-screen.
const MAX_SIDE: u32 = 2048;

const QUALITY: u8 = 85;

/// Whether these bytes are a HEIC photo, by the brands in its `ftyp` box.
///
/// Every brand is read, major and compatible, because a file may name its
/// HEVC brand only among the compatible ones. The generic HEIF brands count
/// when nothing says the image is AV1: an AVIF carries them too, and the
/// decoder here is HEVC's alone. The API sniffs the same way
/// (crates/api/src/heic.rs); the two are separate workspaces, so it is
/// written twice rather than shared.
pub fn is_heif(bytes: &[u8]) -> bool {
    if bytes.len() < 16 || &bytes[4..8] != b"ftyp" {
        return false;
    }
    let size = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    let end = size.clamp(16, bytes.len());
    let (major, _) = bytes[8..].as_chunks::<4>();
    let (compatible, _) = bytes[16..end].as_chunks::<4>();
    let brands = major.iter().take(1).chain(compatible);
    let (mut hevc, mut generic, mut av1) = (false, false, false);
    for brand in brands {
        match brand {
            b"heic" | b"heix" | b"heim" | b"heis" | b"hevc" | b"hevx" => hevc = true,
            b"mif1" | b"mif2" | b"msf1" => generic = true,
            b"avif" | b"avis" => av1 = true,
            _ => {}
        }
    }
    hevc || (generic && !av1)
}

/// The most pixels a photo may have before it is decoded.
///
/// A decoded frame is three bytes a pixel, held whole beside libheif's own
/// planes: a photo at this bound peaks near 350MB. Past it, the file is
/// refused from its header rather than decoded into a pod that cannot hold it.
pub const MAX_PIXELS: u64 = 50_000_000;

/// The primary image of a HEIF file, as JPEG.
///
/// Rotation and mirroring stored in the file are applied, as a phone's own
/// viewer would: a portrait photo is otherwise shown to the model on its side.
/// CPU-bound and allocating a full frame, so the service runs it off the
/// async threads.
pub fn to_jpeg(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let lib = LibHeif::new();
    // libheif's own words name its internals, which are no help to the person
    // whose photo it was; those go to the log, and they are told what failed.
    let unreadable = |e: libheif_rs::HeifError| {
        tracing::info!(error = %e, "a photo did not decode");
        "the file is damaged or is not a photo".to_string()
    };
    let context = HeifContext::read_from_bytes(bytes).map_err(unreadable)?;
    let handle = context.primary_image_handle().map_err(unreadable)?;
    let pixels = handle.width() as u64 * handle.height() as u64;
    if pixels > MAX_PIXELS {
        return Err(format!(
            "the photo is {}x{}; at most {MAX_PIXELS} pixels are converted",
            handle.width(),
            handle.height()
        ));
    }
    let image = lib
        .decode(&handle, ColorSpace::Rgb(RgbChroma::Rgb), None)
        .map_err(unreadable)?;
    let plane = image
        .planes()
        .interleaved
        .ok_or("decoded image has no RGB plane")?;

    let (rgb, width, height) = shrink(
        plane.data,
        plane.stride,
        plane.width as usize,
        plane.height as usize,
    );

    // A JPEG's sides are sixteen bits. Only a strip could pass that inside
    // MAX_PIXELS, and it is refused rather than wrapped.
    let (Ok(width), Ok(height)) = (u16::try_from(width), u16::try_from(height)) else {
        return Err(format!(
            "the photo is {width}x{height}; a JPEG side is at most 65535 pixels"
        ));
    };
    let mut out = Vec::new();
    jpeg_encoder::Encoder::new(&mut out, QUALITY)
        .encode(&rgb, width, height, jpeg_encoder::ColorType::Rgb)
        .map_err(|e| e.to_string())?;
    Ok(out)
}

/// Packed RGB rows, averaged down by a whole factor until the longest side
/// fits under [`MAX_SIDE`].
///
/// A box average rather than libheif's own scaling, which is nearest-neighbor
/// and turns the small print in a photographed receipt to noise -- the very
/// thing somebody asks about.
fn shrink(data: &[u8], stride: usize, width: usize, height: usize) -> (Vec<u8>, usize, usize) {
    // Never so far that the short side vanishes: a strip a few pixels tall
    // keeps them, and stays longer than MAX_SIDE instead.
    let factor = width
        .max(height)
        .div_ceil(MAX_SIDE as usize)
        .min(width.min(height))
        .max(1);
    let (w, h) = (width / factor, height / factor);
    let mut out = Vec::with_capacity(w * h * 3);
    let area = (factor * factor) as u32;
    for y in 0..h {
        for x in 0..w {
            let mut sum = [0u32; 3];
            for dy in 0..factor {
                let row = &data[(y * factor + dy) * stride..];
                for dx in 0..factor {
                    let px = &row[(x * factor + dx) * 3..][..3];
                    for c in 0..3 {
                        sum[c] += px[c] as u32;
                    }
                }
            }
            out.extend(sum.map(|s| ((s + area / 2) / area) as u8));
        }
    }
    (out, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_heif_file_is_known_by_its_brand() {
        assert!(is_heif(b"\0\0\0\x18ftypheic\0\0\0\0"));
        assert!(is_heif(b"\0\0\0\x18ftypmif1\0\0\0\0"));
        // An MP4 has an ftyp box too, under a brand that is not HEIF's.
        assert!(!is_heif(b"\0\0\0\x18ftypisom\0\0\0\0"));
        assert!(!is_heif(b"\xff\xd8\xff\xe0 jpeg"));
        assert!(!is_heif(b"\0\0\0\x18ftyp"));
    }

    #[test]
    fn a_heic_brand_among_the_compatible_ones_counts_and_an_avif_does_not() {
        let mut compatible = b"\0\0\0\x18ftypmif1\0\0\0\0".to_vec();
        compatible.extend_from_slice(b"heic");
        assert!(is_heif(&compatible));
        let mut avif = b"\0\0\0\x1cftypavif\0\0\0\0".to_vec();
        avif.extend_from_slice(b"mif1miaf");
        assert!(!is_heif(&avif));
    }

    #[test]
    fn a_thin_strip_keeps_its_short_side() {
        // 6000x2 would shrink by three and leave no rows at all.
        let data = vec![9u8; 6000 * 2 * 3];
        let (out, w, h) = shrink(&data, 6000 * 3, 6000, 2);
        assert_eq!((w, h), (3000, 1));
        assert_eq!(out.len(), 3000 * 3);
    }

    #[test]
    fn a_small_image_is_kept_at_its_size() {
        // Two pixels, with a stride wider than the row as libheif pads them.
        let data = [1, 2, 3, 4, 5, 6, 0, 0];
        assert_eq!(shrink(&data, 8, 2, 1), (vec![1, 2, 3, 4, 5, 6], 2, 1));
    }

    #[test]
    fn a_large_image_is_averaged_rather_than_sampled() {
        // 4096 wide halves to 2048: each pixel out is the mean of a 2x2 block.
        let (width, height) = (4096, 2);
        let mut data = vec![0u8; width * height * 3];
        for (i, v) in data.iter_mut().enumerate() {
            // Alternate columns black and white, which sampling would keep
            // one of and averaging turns grey.
            *v = if (i / 3) % 2 == 0 { 0 } else { 200 };
        }
        let (out, w, h) = shrink(&data, width * 3, width, height);
        assert_eq!((w, h), (2048, 1));
        assert!(out.iter().all(|&v| v == 100));
    }

    #[test]
    fn a_phone_photo_comes_out_as_a_jpeg_of_the_same_size() {
        // 64x32, encoded by libheif's own heif-enc. Small enough to commit,
        // real enough to go through the HEVC decoder.
        let heic = include_bytes!("../tests/gradient.heic");
        assert!(is_heif(heic));
        let jpeg = to_jpeg(heic).expect("decodes");
        assert!(jpeg.starts_with(b"\xff\xd8\xff"));
        // SOF0 carries height then width, big-endian, five bytes in.
        let sof = jpeg
            .windows(2)
            .position(|w| w == [0xff, 0xc0])
            .expect("SOF0");
        assert_eq!(&jpeg[sof + 5..sof + 9], &[0, 32, 0, 64]);
    }

    #[test]
    fn something_that_is_not_heif_is_refused_rather_than_guessed_at() {
        assert!(to_jpeg(b"\0\0\0\x18ftypheic\0\0\0\0").is_err());
    }
}

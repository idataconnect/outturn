//! A phone's HEIC photo, made a JPEG when it is uploaded.
//!
//! Phones save photos as HEIC, and that is what arrives when one is uploaded
//! straight off a phone or dragged out of a desktop photo library. Few models
//! read it and only one browser shows it, so a HEIC that reached an agent
//! would fail later, somewhere the person who uploaded it cannot see.
//! Converted here instead, once: the JPEG is stored beside the original, and
//! everything after the upload -- agents, models, the browser -- sees that.
//!
//! The conversion is a service of its own (renderers/heic), deployed only
//! where an operator asks for it: HEVC is a large native decoder fed
//! untrusted bytes, and a codec with licensing of its own. `OUTTURN_HEIC_URL`
//! unset, a HEIC upload is refused with the reason. See docs/heic.md.

use std::time::Duration;

/// How long an upload waits for its photo. A phone photo converts in about a
/// second, and the converter gives up on one at thirty; it answers a request
/// it has no room for at once rather than queueing it, so this is never
/// spent waiting for a turn.
const TIMEOUT: Duration = Duration::from_secs(45);

/// The most a reply may be. A JPEG of the largest photo the converter keeps
/// is a few megabytes; past this, the reply is not one.
const MAX_JPEG_BYTES: usize = 25 * 1024 * 1024;

/// How much of the converter's own explanation is passed on.
const MAX_REASON_CHARS: usize = 300;

/// Where the converter is, if one is deployed.
pub fn url() -> Option<String> {
    std::env::var("OUTTURN_HEIC_URL")
        .ok()
        .map(|u| u.trim_end_matches('/').to_string())
        .filter(|u| !u.is_empty())
}

/// Whether these bytes are a HEIC photo, by the brands in its `ftyp` box.
///
/// Sniffed rather than taken from the name: a photo can arrive as
/// `image.jpg` from a phone that never converted it. Every brand is read,
/// major and compatible, because a file may name its HEVC brand only among
/// the compatible ones. The generic HEIF brands count when nothing says the
/// image is AV1 -- an AVIF carries them too, and is not this converter's.
pub fn is_heif(bytes: &[u8]) -> bool {
    if bytes.len() < 16 || &bytes[4..8] != b"ftyp" {
        return false;
    }
    let size = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    let end = size.clamp(16, bytes.len());
    // The major brand, then the compatible ones after the minor version.
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

/// A path cut at its name's extension: `session/IMG.HEIC` is
/// (`session/IMG`, `Some("HEIC")`).
fn split_extension(path: &str) -> (&str, Option<&str>) {
    let name_starts = path.rfind('/').map_or(0, |i| i + 1);
    match path[name_starts..].rfind('.') {
        Some(dot) if dot > 0 => (
            &path[..name_starts + dot],
            Some(&path[name_starts + dot + 1..]),
        ),
        _ => (path, None),
    }
}

/// Where the original is stored: where it was sent, if its name says HEIC;
/// otherwise under its name ending `.heic`, so that a photo named `photo.jpg`
/// is not stored as a HEIC calling itself a JPEG.
pub fn original_path(path: &str) -> String {
    match split_extension(path) {
        (_, Some(ext)) if ext.eq_ignore_ascii_case("heic") || ext.eq_ignore_ascii_case("heif") => {
            path.to_string()
        }
        (stem, _) => format!("{stem}.heic"),
    }
}

/// Names a converted file could take, best first: the same name ending
/// `extension`, then numbered. The caller takes the first that is free.
pub fn candidates(path: &str, extension: &str) -> impl Iterator<Item = String> {
    let stem = split_extension(path).0.to_string();
    let extension = extension.to_string();
    std::iter::once(format!("{stem}.{extension}"))
        .chain((2..).map(move |n| format!("{stem}-{n}.{extension}")))
}

/// What became of a conversion, as the person uploading is told it.
#[derive(Debug)]
pub enum Failure {
    /// The converter is down, busy or slow: the photo may be fine.
    Unavailable(String),
    /// The converter read the photo and could not convert it.
    Refused(String),
}

impl From<Failure> for super::router::ApiError {
    fn from(f: Failure) -> Self {
        use axum::http::StatusCode;
        match f {
            Failure::Unavailable(why) => (StatusCode::SERVICE_UNAVAILABLE, why),
            Failure::Refused(why) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("The photo could not be converted: {why}"),
            ),
        }
    }
}

/// The photo as a JPEG, from the converter at `url`.
///
/// The converter is the side that decodes untrusted bytes, so its reply is
/// not trusted either: read only up to a bound, accepted only if it is a
/// JPEG, and its explanations passed on only in part.
pub async fn to_jpeg(url: &str, bytes: bytes::Bytes) -> Result<Vec<u8>, Failure> {
    use outturn_core::http_client::{Capped, read_capped};
    let unavailable =
        || Failure::Unavailable("The photo converter is not answering; try again shortly.".into());

    let response = outturn_core::http_client::reporting_client()
        .post(format!("{url}/v1/convert/jpeg"))
        .timeout(TIMEOUT)
        .body(bytes)
        .send()
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "HEIC converter unreachable");
            unavailable()
        })?;
    let status = response.status();
    let body = read_capped(response, MAX_JPEG_BYTES)
        .await
        .map_err(|e| match e {
            Capped::TooLarge => {
                tracing::warn!("HEIC converter replied with more than {MAX_JPEG_BYTES} bytes");
                Failure::Refused("the converter's reply was too large".into())
            }
            Capped::Stopped(e) => {
                tracing::warn!(error = %e, "HEIC converter stopped partway");
                unavailable()
            }
        })?;

    if status.is_success() {
        if !body.starts_with(b"\xff\xd8\xff") {
            tracing::warn!("HEIC converter replied with something that is not a JPEG");
            return Err(Failure::Refused(
                "the converter's reply was not a JPEG".into(),
            ));
        }
        return Ok(body);
    }
    // Busy and too slow are about the converter, not the photo; and anything
    // in between -- a proxy's error page, a pod restarting -- is said as the
    // converter being unavailable rather than passed on as it came.
    match status.as_u16() {
        503 => Err(Failure::Unavailable(
            "The photo converter is busy; try again shortly.".into(),
        )),
        504 => Err(Failure::Unavailable(
            "The photo took too long to convert; try again shortly.".into(),
        )),
        400..=499 => {
            let said = String::from_utf8_lossy(&body);
            Err(Failure::Refused(
                said.chars().take(MAX_REASON_CHARS).collect(),
            ))
        }
        _ => {
            tracing::warn!(%status, "HEIC converter failed");
            Err(unavailable())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_heif_file_is_known_by_its_brand_rather_than_its_name() {
        assert!(is_heif(b"\0\0\0\x18ftypheic\0\0\0\0"));
        assert!(is_heif(b"\0\0\0\x1cftypheix\0\0\0\0"));
        assert!(is_heif(b"\0\0\0\x18ftypmif1\0\0\0\0"));
        // An MP4 has an ftyp box too, under a brand that is not HEIF's.
        assert!(!is_heif(b"\0\0\0\x18ftypisom\0\0\0\0"));
        assert!(!is_heif(b"\xff\xd8\xff\xe0 jpeg"));
        assert!(!is_heif(b"\0\0\0\x18ftyp"));
    }

    #[test]
    fn a_heic_brand_among_the_compatible_ones_counts() {
        // Major brand mif1, with heic only in the compatible list.
        let mut box_ = b"\0\0\0\x18ftypmif1\0\0\0\0".to_vec();
        box_.extend_from_slice(b"heic");
        assert!(is_heif(&box_));
        assert!(is_heif(b"\0\0\0\x10ftypmif2\0\0\0\0"));
    }

    #[test]
    fn an_avif_is_not_taken_for_a_heic() {
        let mut avif = b"\0\0\0\x1cftypavif\0\0\0\0".to_vec();
        avif.extend_from_slice(b"mif1miaf");
        assert!(!is_heif(&avif));
        let mut marked = b"\0\0\0\x18ftypmif1\0\0\0\0".to_vec();
        marked.extend_from_slice(b"avif");
        assert!(!is_heif(&marked));
    }

    #[test]
    fn the_original_keeps_its_name_only_when_the_name_says_heic() {
        assert_eq!(
            original_path("session/IMG_1234.HEIC"),
            "session/IMG_1234.HEIC"
        );
        assert_eq!(original_path("session/a.heif"), "session/a.heif");
        // The case sniffing exists for: a HEIC sent under a JPEG's name.
        assert_eq!(original_path("session/photo.jpg"), "session/photo.heic");
        assert_eq!(original_path("session/photo.jpeg"), "session/photo.heic");
        assert_eq!(original_path("session/photo"), "session/photo.heic");
        // A dot in a directory is not the name's extension.
        assert_eq!(
            original_path("session/v1.2/photo"),
            "session/v1.2/photo.heic"
        );
    }

    #[test]
    fn a_converted_name_is_offered_plain_then_numbered() {
        let names: Vec<_> = candidates("session/IMG_1234.HEIC", "jpg").take(3).collect();
        assert_eq!(
            names,
            [
                "session/IMG_1234.jpg",
                "session/IMG_1234-2.jpg",
                "session/IMG_1234-3.jpg"
            ]
        );
        assert_eq!(
            candidates("session/.heic", "jpg").next().unwrap(),
            "session/.heic.jpg"
        );
    }
}

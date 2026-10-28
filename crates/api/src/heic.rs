//! A phone's HEIC photo, made a JPEG when it is uploaded.
//!
//! An iPhone saves photos as HEIC, and that is what arrives when one is
//! uploaded straight off a phone or dragged out of a Mac's photo library.
//! Few models read it and only Safari shows it, so a HEIC that reached an
//! agent would fail later, somewhere the person who uploaded it cannot see.
//! Converted here instead, once: the JPEG is stored beside the original, and
//! everything after the upload -- agents, models, the browser -- sees that.
//!
//! The conversion is a service of its own (renderers/heic), deployed only
//! where an operator asks for it: HEVC is a large native decoder fed
//! untrusted bytes, and a codec with licensing of its own. `OUTTURN_HEIC_URL`
//! unset, a HEIC upload is refused with the reason. See docs/heic.md.

use std::time::Duration;

/// How long an upload waits for its photo. A phone photo converts in about a
/// second; the converter gives up on one at thirty.
const TIMEOUT: Duration = Duration::from_secs(45);

/// Where the converter is, if one is deployed.
pub fn url() -> Option<String> {
    std::env::var("OUTTURN_HEIC_URL")
        .ok()
        .map(|u| u.trim_end_matches('/').to_string())
        .filter(|u| !u.is_empty())
}

/// Whether these bytes are a HEIF file, by the brand in its `ftyp` box.
///
/// Sniffed rather than taken from the name: a photo can arrive as
/// `image.jpg` from a phone that never converted it. `mif1` and `msf1` are
/// the generic HEIF brands, which an AVIF also carries; the converter reads
/// both.
pub fn is_heif(bytes: &[u8]) -> bool {
    bytes.len() >= 12
        && &bytes[4..8] == b"ftyp"
        && matches!(
            &bytes[8..12],
            b"heic" | b"heix" | b"heim" | b"heis" | b"hevc" | b"hevx" | b"mif1" | b"msf1"
        )
}

/// Where the JPEG of a photo is stored: the same name, ending `.jpg`.
pub fn jpeg_path(path: &str) -> String {
    let name_starts = path.rfind('/').map_or(0, |i| i + 1);
    let stem = match path[name_starts..].rfind('.') {
        Some(dot) if dot > 0 => &path[..name_starts + dot],
        _ => path,
    };
    format!("{stem}.jpg")
}

/// The photo as a JPEG, from the converter at `url`.
///
/// The error is said to the person who uploaded it, so it says what happened
/// to the photo rather than to the request.
pub async fn to_jpeg(url: &str, bytes: bytes::Bytes) -> Result<Vec<u8>, String> {
    let response = outturn_core::http_client::reporting_client()
        .post(format!("{url}/v1/convert/jpeg"))
        .timeout(TIMEOUT)
        .body(bytes)
        .send()
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "HEIC converter unreachable");
            "the photo converter is not answering".to_string()
        })?;
    let status = response.status();
    let body = response.bytes().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        // The converter's own refusals are written for this person already:
        // too large, too many pixels, a file that does not decode.
        return Err(String::from_utf8_lossy(&body).into_owned());
    }
    Ok(body.to_vec())
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
    fn the_jpeg_sits_beside_the_photo_under_the_same_name() {
        assert_eq!(jpeg_path("session/IMG_1234.HEIC"), "session/IMG_1234.jpg");
        assert_eq!(jpeg_path("session/trip/a.b.heic"), "session/trip/a.b.jpg");
        assert_eq!(jpeg_path("session/photo"), "session/photo.jpg");
        // A dot in a directory is not the name's extension.
        assert_eq!(jpeg_path("session/v1.2/photo"), "session/v1.2/photo.jpg");
        assert_eq!(jpeg_path("session/.heic"), "session/.heic.jpg");
        // Already named as a JPEG: the JPEG takes its place.
        assert_eq!(jpeg_path("session/image.jpg"), "session/image.jpg");
    }
}

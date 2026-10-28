//! Converts HEIC photos to JPEG.
//!
//! An iPhone saves photos as HEIC, and that is what arrives when one is
//! uploaded straight off a phone or dragged out of a Mac's photo library. Few
//! models will read it, and a browser other than Safari will not show it. The
//! API sends each one here at upload and stores the JPEG beside it, so
//! nothing after the upload ever sees HEIC.
//!
//! A service of its own, and optional, because HEVC is a large native decoder
//! fed untrusted bytes, and a codec with licensing of its own besides: the
//! images every deployment runs should carry neither. Served over HTTP by the
//! binary in `src/main.rs`; see docs/heic.md.

mod convert;

pub use convert::{MAX_PIXELS, is_heif, to_jpeg};

pub mod service;

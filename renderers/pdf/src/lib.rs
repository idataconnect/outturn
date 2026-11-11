//! Renders markdown as a PDF.
//!
//! Layout is a flow: blocks one after another, words wrapped greedily to the
//! measure, a new page when the cursor reaches the bottom margin. That is
//! deliberately all -- a report an agent writes is headings, paragraphs,
//! lists, tables and code, and anything more is a typesetting engine, which
//! is a different and much larger thing. Served over HTTP by the binary in
//! `src/main.rs`; see docs/pdf-rendering.md.

mod images;
mod layout;
mod theme;

pub use images::{BUNDLE, Images, unbundle};
pub use layout::{FontFile, render, set_fallback_fonts};
pub use theme::{THEMES, Theme};

pub mod service;

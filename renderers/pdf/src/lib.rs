//! Renders markdown as a PDF.
//!
//! Instantiated by the runtime for one call at a time, in a store of its own
//! (see docs/pdf-rendering.md). Layout is a flow: blocks one after another,
//! words wrapped greedily to the measure, a new page when the cursor reaches
//! the bottom margin. That is deliberately all -- a report an agent writes is
//! headings, paragraphs, lists, tables and code, and anything more is a
//! typesetting engine, which is a different and much larger component.

#[cfg(target_arch = "wasm32")]
#[allow(warnings)]
mod bindings;

mod layout;

pub use layout::render;

#[cfg(target_arch = "wasm32")]
struct Component;

#[cfg(target_arch = "wasm32")]
impl bindings::Guest for Component {
    fn render(markdown: String) -> Result<Vec<u8>, String> {
        layout::render(&markdown)
    }
}

#[cfg(target_arch = "wasm32")]
bindings::export!(Component with_types_in bindings);

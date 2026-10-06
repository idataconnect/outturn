//! Runs the PDF renderer.
//!
//! A component of its own, instantiated per call in a store of its own: its
//! memory cap and fuel are this call's rather than the turn's, and are
//! charged to admission only while a render is running, so a turn that never
//! renders pays nothing for the capability. See docs/pdf-rendering.md.

use std::sync::Arc;

use tokio::sync::Semaphore;
use wasmtime::component::{Component, Linker};
use wasmtime::{Engine, Store};
use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

mod bindings {
    wasmtime::component::bindgen!({
        path: "wit",
        world: "pdf-renderer",
        exports: { default: async },
    });
}

/// The renderer, built from `renderers/pdf` and committed beside the default
/// agent for the same reason: so a build of this binary needs no wasm
/// toolchain. Compiled into the binary rather than read from disk, because a
/// runtime without it is a runtime whose agents are told PDFs are available
/// and then refused.
static RENDERER: &[u8] = include_bytes!("../../assets/pdf_renderer.wasm");

/// Linear memory a render may grow to.
///
/// Markdown at `MAX_MARKDOWN_BYTES` peaks at 10.5 MiB, most of it the fonts:
/// pages are drawn as they are laid out and dropped, so what grows with a
/// document is only the PDF being written. Three times that, for headroom.
pub const MEMORY_LIMIT: usize = 32 * 1024 * 1024;

/// What a render is charged to admission while it runs: its cap, and the
/// document it hands back, which is copied out of the store before the store
/// is dropped.
pub const CHARGE_BYTES: u64 = (MEMORY_LIMIT as u64) * 3 / 2;

/// Instructions a render may spend.
///
/// A runaway guard, not a budget. Markdown at `MAX_MARKDOWN_BYTES` -- a
/// megabyte of PDF, several hundred pages -- spends 33 billion and a few
/// seconds; this is three times that, so no document the cap admits comes
/// near it, and a renderer stuck in a loop is still stopped within seconds. A budget a customer feels
/// belongs in spend controls, priced, rather than here.
pub const FUEL: u64 = 100_000_000_000;

/// The most markdown a render accepts. Already several hundred pages; past
/// it the cost of laying out is better spent asking whether one document is
/// what is wanted.
pub const MAX_MARKDOWN_BYTES: usize = 2 * 1024 * 1024;

/// Renders at once on one pod. Rendering is all CPU; fuel yields to the
/// executor, but a pod running nothing else but renders is not serving turns.
const CONCURRENT: usize = 2;

struct RenderHost {
    wasi: WasiCtx,
    table: ResourceTable,
    limits: Limits,
}

/// Caps the renderer's memory, and remembers the most it asked for.
///
/// The peak is reported because it is the only way to know whether the cap
/// is right: a render that fits says nothing about how close it came.
struct Limits {
    peak: usize,
}

impl wasmtime::ResourceLimiter for Limits {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        if desired > MEMORY_LIMIT {
            return Ok(false);
        }
        self.peak = self.peak.max(desired);
        Ok(true)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        Ok(desired <= 100_000)
    }

    fn instances(&self) -> usize {
        4
    }

    fn tables(&self) -> usize {
        16
    }

    fn memories(&self) -> usize {
        4
    }
}

/// A rendered document, and what making it cost.
#[derive(Debug)]
pub struct Rendered {
    pub pdf: Vec<u8>,
    /// Instructions the renderer spent. What a compute ledger would record.
    pub fuel: u64,
    /// The most linear memory the renderer held, in bytes.
    pub peak_memory: usize,
}

impl WasiView for RenderHost {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

/// The renderer, compiled the first time it is wanted.
///
/// Compiling is the expensive part, and most turns never render: paying for
/// it in every process that runs a turn -- every test, every pod at startup
/// before it can take work -- would be paying for nothing most of the time.
/// `warm` compiles it ahead, for a pod that would rather find out at startup
/// that it cannot.
pub struct LazyRenderer {
    engine: Engine,
    compiled: tokio::sync::OnceCell<Result<Arc<PdfRenderer>, String>>,
}

impl LazyRenderer {
    pub fn new(engine: &Engine) -> Arc<Self> {
        Arc::new(Self {
            engine: engine.clone(),
            compiled: tokio::sync::OnceCell::new(),
        })
    }

    pub async fn get(&self) -> Result<Arc<PdfRenderer>, String> {
        self.compiled
            .get_or_init(|| async {
                let engine = self.engine.clone();
                // Off the executor: compiling is seconds of CPU.
                tokio::task::spawn_blocking(move || PdfRenderer::new(&engine))
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|r| r.map_err(|e| e.to_string()))
                    .inspect_err(|e| tracing::error!(error = %e, "the PDF renderer did not compile"))
                    .map_err(|_| "PDF rendering is not available here".to_string())
            })
            .await
            .clone()
    }

    /// Compiles now and says whether it worked.
    pub async fn warm(&self) -> Result<(), String> {
        self.get().await.map(|_| ())
    }
}

pub struct PdfRenderer {
    engine: Engine,
    pre: bindings::PdfRendererPre<RenderHost>,
    running: Semaphore,
}

impl PdfRenderer {
    /// Compiles the renderer.
    pub fn new(engine: &Engine) -> anyhow::Result<Arc<Self>> {
        let component = Component::new(engine, RENDERER)?;
        let mut linker = Linker::new(engine);
        // Linked because the Rust standard library on wasip2 imports it, not
        // because the renderer uses it: the context behind it has no
        // directories, no environment and no network.
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
        let pre = bindings::PdfRendererPre::new(linker.instantiate_pre(&component)?)?;
        Ok(Arc::new(Self {
            engine: engine.clone(),
            pre,
            running: Semaphore::new(CONCURRENT),
        }))
    }

    /// Renders markdown as a PDF. Errors are said for the guest that asked.
    pub async fn render(&self, markdown: &str) -> Result<Rendered, String> {
        if markdown.len() > MAX_MARKDOWN_BYTES {
            return Err(format!(
                "the markdown is {} bytes; a PDF is rendered from at most {MAX_MARKDOWN_BYTES}",
                markdown.len()
            ));
        }
        let _slot = self
            .running
            .acquire()
            .await
            .map_err(|_| "the renderer is shutting down".to_string())?;

        let mut store = Store::new(
            &self.engine,
            RenderHost {
                wasi: WasiCtxBuilder::new().build(),
                table: ResourceTable::new(),
                limits: Limits { peak: 0 },
            },
        );
        store.limiter(|h| &mut h.limits);
        store.set_fuel(FUEL).map_err(|e| e.to_string())?;
        store
            .fuel_async_yield_interval(Some(super::component::FUEL_YIELD_INTERVAL))
            .map_err(|e| e.to_string())?;

        let renderer = self
            .pre
            .instantiate_async(&mut store)
            .await
            .map_err(|e| format!("the renderer could not start: {e}"))?;
        match renderer.call_render(&mut store, markdown).await {
            Ok(result) => {
                let pdf = result?;
                Ok(Rendered {
                    pdf,
                    fuel: FUEL - store.get_fuel().unwrap_or(0),
                    peak_memory: store.data().limits.peak,
                })
            }
            // A trap is the renderer running out of something -- fuel or
            // memory -- far more often than a bug, and either way the
            // document is the cause. Said as such, and logged for the bug.
            Err(e) => {
                tracing::debug!(error = %e, "the renderer trapped");
                Err("the document is too large or too complex to render".to_string())
            }
        }
    }
}

#[cfg(test)]
mod artifact_guard {
    /// The committed renderer must have been built against the current
    /// interface. See the guard beside the default agent for why this is a
    /// file comparison and what it saves.
    #[test]
    fn the_committed_renderer_was_built_against_this_interface() {
        const BUILT_AGAINST: &str = include_str!("../../assets/pdf_renderer.wit");
        const CURRENT: &str = include_str!("../../wit/renderer.wit");
        assert_eq!(
            CURRENT, BUILT_AGAINST,
            "wit/renderer.wit has changed since assets/pdf_renderer.wasm was built; \
             rebuild it as the README describes"
        );
    }
}

#[cfg(all(test, feature = "slow-tests"))]
mod tests {
    use super::*;

    fn engine() -> Engine {
        let mut config = wasmtime::Config::new();
        config.consume_fuel(true);
        Engine::new(&config).unwrap()
    }

    #[tokio::test]
    async fn renders_a_document_in_its_sandbox() {
        let r = PdfRenderer::new(&engine()).unwrap();
        let pdf = r
            .render("# Hello\n\nA paragraph, a **bold** word.\n\n| a | b |\n|---|--:|\n| 1 | 2 |\n")
            .await
            .expect("rendered");
        assert!(pdf.pdf.starts_with(b"%PDF"));
    }

    /// The largest document the cap is meant for, inside the limits set for
    /// it. If this fails, the limits and the charge above need revisiting
    /// together.
    #[tokio::test]
    async fn a_document_at_the_size_cap_renders_within_its_limits() {
        let para = "Spend rose over the quarter, mostly in three workspaces, \
                    and cached tokens make the totals look smaller than the work. "
            .repeat(6);
        let mut md = String::from("# Report\n\n");
        let mut n = 0;
        while md.len() < MAX_MARKDOWN_BYTES - 4096 {
            n += 1;
            md.push_str(&format!("## Section {n}\n\n{para}\n\n- one\n- two\n\n"));
        }
        let r = PdfRenderer::new(&engine()).unwrap();
        let done = r.render(&md).await.expect("rendered at the cap");
        assert!(done.pdf.starts_with(b"%PDF"));
        eprintln!(
            "at the cap: {} bytes of markdown, {:.2}G fuel, {:.1} MiB peak, {} KiB of PDF",
            md.len(),
            done.fuel as f64 / 1e9,
            done.peak_memory as f64 / 1048576.0,
            done.pdf.len() / 1024
        );
        // Headroom, not just a pass: a cap the largest input nearly reaches
        // is one the next change to the renderer will cross.
        assert!(done.fuel < FUEL / 2, "used {} of {FUEL} fuel", done.fuel);
        assert!(
            done.peak_memory < MEMORY_LIMIT / 2,
            "peaked at {} of {MEMORY_LIMIT} bytes",
            done.peak_memory
        );
    }

    #[tokio::test]
    async fn markdown_past_the_cap_is_refused_before_rendering() {
        let r = PdfRenderer::new(&engine()).unwrap();
        let err = r.render(&"a".repeat(MAX_MARKDOWN_BYTES + 1)).await.unwrap_err();
        assert!(err.contains("at most"), "{err}");
    }
}

pub mod pdf;

use anyhow::Result;
use wasmtime::Linker;

use super::sandbox::SandboxState;

pub fn link_all(linker: &mut Linker<SandboxState>) -> Result<()> {
    pdf::link(linker)?;
    Ok(())
}

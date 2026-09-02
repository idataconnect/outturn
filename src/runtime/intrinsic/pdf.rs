use anyhow::Result;
use std::process::Command;
use wasmtime::{Caller, Linker};

use crate::runtime::sandbox::SandboxState;

pub fn link(linker: &mut Linker<SandboxState>) -> Result<()> {
    linker.func_wrap(
        "env",
        "pdf_extract_text",
        |mut caller: Caller<'_, SandboxState>,
         path_ptr: i32,
         path_len: i32,
         out_ptr: i32,
         out_cap: i32|
         -> i32 {
            let result = (|| -> anyhow::Result<Vec<u8>> {
                let memory = caller
                    .get_export("memory")
                    .and_then(|e| e.into_memory())
                    .ok_or_else(|| anyhow::anyhow!("missing guest memory"))?;

                let data = memory.data(&caller);
                let path_start = path_ptr as usize;
                let path_end = path_start + path_len as usize;
                if path_end > data.len() {
                    anyhow::bail!("path out of bounds");
                }
                let path = std::str::from_utf8(&data[path_start..path_end])?;

                let storage = caller.data().storage.clone();
                let file_data = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current()
                        .block_on(storage.read(path, 0, u32::MAX))
                })?;

                extract_text(&file_data)
            })();

            match result {
                Ok(text) => {
                    let memory = caller
                        .get_export("memory")
                        .and_then(|e| e.into_memory())
                        .unwrap();
                    let cap = out_cap as usize;
                    let len = text.len().min(cap);
                    let dest = out_ptr as usize;
                    if dest + len <= memory.data_size(&caller) {
                        memory.data_mut(&mut caller)[dest..dest + len]
                            .copy_from_slice(&text[..len]);
                    }
                    len as i32
                }
                Err(_) => -1,
            }
        },
    )?;

    Ok(())
}

fn extract_text(pdf_bytes: &[u8]) -> Result<Vec<u8>> {
    let mut child = Command::new("pdftotext")
        .args(["-", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        stdin.write_all(pdf_bytes)?;
    }

    let output = child.wait_with_output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("pdftotext failed: {stderr}");
    }

    Ok(output.stdout)
}

use std::sync::Arc;

use anyhow::Result;
use uuid::Uuid;
use wasmtime::*;
use wasmtime_wasi::p1::WasiP1Ctx;
use wasmtime_wasi::WasiCtxBuilder;

use super::storage::StorageBackend;

#[derive(Clone, Copy, Debug)]
pub enum SecurityTier {
    /// Full access: filesystem, gateway, scoped network
    Privileged,
    /// Standard: filesystem, gateway, no external network
    Standard,
    /// Restricted: filesystem only, no network
    Restricted,
}

pub struct SandboxConfig {
    pub session_id: Uuid,
    pub tenant_id: Uuid,
    pub security_tier: SecurityTier,
    pub gateway_token: String,
    pub gateway_url: String,
    pub memory_limit_bytes: u64,
    pub fuel_limit: u64,
}

pub struct Sandbox {
    engine: Engine,
    config: SandboxConfig,
    storage: Arc<dyn StorageBackend>,
}

pub struct SandboxState {
    pub(crate) wasi: WasiP1Ctx,
    pub(crate) storage: Arc<dyn StorageBackend>,
}

impl Sandbox {
    pub fn new(config: SandboxConfig, storage: Arc<dyn StorageBackend>) -> Result<Self> {
        let mut engine_config = Config::new();
        engine_config.consume_fuel(true);
        engine_config.wasm_component_model(true);

        let engine = Engine::new(&engine_config)?;

        Ok(Self {
            engine,
            config,
            storage,
        })
    }

    pub fn session_id(&self) -> Uuid {
        self.config.session_id
    }

    pub async fn run(&self, wasm_bytes: &[u8]) -> Result<()> {
        let module = Module::new(&self.engine, wasm_bytes)?;

        let mut linker = Linker::<SandboxState>::new(&self.engine);
        wasmtime_wasi::p1::add_to_linker_sync(&mut linker, |state: &mut SandboxState| {
            &mut state.wasi
        })?;

        self.link_storage_functions(&mut linker)?;
        super::intrinsic::link_all(&mut linker)?;

        let wasi = WasiCtxBuilder::new()
            .env("GATEWAY_TOKEN", &self.config.gateway_token)
            .env("GATEWAY_URL", &self.config.gateway_url)
            .env("SESSION_ID", self.config.session_id.to_string())
            .build_p1();

        let state = SandboxState {
            wasi,
            storage: Arc::clone(&self.storage),
        };

        let mut store = Store::new(&self.engine, state);
        store.set_fuel(self.config.fuel_limit)?;

        let instance = linker.instantiate(&mut store, &module)?;
        let func = instance
            .get_typed_func::<(), ()>(&mut store, "_start")
            .or_else(|_| instance.get_typed_func::<(), ()>(&mut store, "main"))?;

        func.call(&mut store, ())?;

        Ok(())
    }

    fn link_storage_functions(&self, linker: &mut Linker<SandboxState>) -> Result<()> {
        linker.func_wrap(
            "env",
            "storage_stat",
            |mut caller: Caller<'_, SandboxState>, path_ptr: i32, path_len: i32| -> i64 {
                let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                    Some(m) => m,
                    None => return -1,
                };
                let data = memory.data(&caller);
                let path = match std::str::from_utf8(
                    &data[path_ptr as usize..(path_ptr + path_len) as usize],
                ) {
                    Ok(p) => p.to_string(),
                    Err(_) => return -1,
                };

                let storage = caller.data().storage.clone();
                let result = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current()
                        .block_on(storage.stat(&path))
                });

                match result {
                    Ok(meta) => meta.size as i64,
                    Err(_) => -1,
                }
            },
        )?;

        linker.func_wrap(
            "env",
            "storage_read",
            |mut caller: Caller<'_, SandboxState>,
             path_ptr: i32,
             path_len: i32,
             offset: i64,
             buf_ptr: i32,
             buf_len: i32|
             -> i32 {
                let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                    Some(m) => m,
                    None => return -1,
                };
                let data = memory.data(&caller);
                let path = match std::str::from_utf8(
                    &data[path_ptr as usize..(path_ptr + path_len) as usize],
                ) {
                    Ok(p) => p.to_string(),
                    Err(_) => return -1,
                };

                let storage = caller.data().storage.clone();
                let result = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current()
                        .block_on(storage.read(&path, offset as u64, buf_len as u32))
                });

                match result {
                    Ok(bytes) => {
                        let len = bytes.len().min(buf_len as usize);
                        let dest = buf_ptr as usize;
                        memory.data_mut(&mut caller)[dest..dest + len]
                            .copy_from_slice(&bytes[..len]);
                        len as i32
                    }
                    Err(_) => -1,
                }
            },
        )?;

        linker.func_wrap(
            "env",
            "storage_write",
            |mut caller: Caller<'_, SandboxState>,
             path_ptr: i32,
             path_len: i32,
             offset: i64,
             data_ptr: i32,
             data_len: i32|
             -> i64 {
                let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                    Some(m) => m,
                    None => return -1,
                };
                let mem_data = memory.data(&caller);
                let path = match std::str::from_utf8(
                    &mem_data[path_ptr as usize..(path_ptr + path_len) as usize],
                ) {
                    Ok(p) => p.to_string(),
                    Err(_) => return -1,
                };
                let src = data_ptr as usize;
                let write_data = mem_data[src..src + data_len as usize].to_vec();

                let storage = caller.data().storage.clone();
                let result = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current()
                        .block_on(storage.write(&path, offset as u64, &write_data))
                });

                match result {
                    Ok(n) => n as i64,
                    Err(_) => -1,
                }
            },
        )?;

        linker.func_wrap(
            "env",
            "storage_delete",
            |mut caller: Caller<'_, SandboxState>, path_ptr: i32, path_len: i32| -> i32 {
                let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                    Some(m) => m,
                    None => return -1,
                };
                let data = memory.data(&caller);
                let path = match std::str::from_utf8(
                    &data[path_ptr as usize..(path_ptr + path_len) as usize],
                ) {
                    Ok(p) => p.to_string(),
                    Err(_) => return -1,
                };

                let storage = caller.data().storage.clone();
                let result = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current()
                        .block_on(storage.delete(&path))
                });

                match result {
                    Ok(()) => 0,
                    Err(_) => -1,
                }
            },
        )?;

        Ok(())
    }
}

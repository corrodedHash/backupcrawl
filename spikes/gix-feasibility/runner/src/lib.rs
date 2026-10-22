//! Native runner for the M1.7 gix feasibility component.
//!
//! Instantiates the wasm component with wasmtime + wasmtime-wasi, a read-only
//! preopen of `/` and an empty environment, then calls `run(root)`.
//!
//! This mirrors what the M1.4 host runtime will do (read-only `/` preopen,
//! empty env) so the spike measures realistic conditions. It is deliberately
//! self-contained: it does not depend on the M1.1 WIT contract or the host.
//!
//! Note: wasmtime 49 returns its own `wasmtime::Error`; with the `anyhow`
//! feature enabled it converts into `anyhow::Error` via `?`, and
//! `wasmtime::error::Context` provides `.context(...)` for wasmtime results.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::error::Context as _;
use wasmtime::{Config, Engine, Error, Result, Store};
use wasmtime_wasi::{FsPerms, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

mod bindings {
    wasmtime::component::bindgen!({
        path: "../component/wit",
        world: "spike",
    });
}

pub use bindings::Spike as SpikeWorld;

/// Store data: WASI context plus its resource table, as required by wasmtime-wasi 49.
pub struct Ctx {
    ctx: WasiCtx,
    table: ResourceTable,
}

impl WasiView for Ctx {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.ctx,
            table: &mut self.table,
        }
    }
}

/// A compiled component plus the linker configured with WASI p2.
pub struct SpikeHost {
    engine: Engine,
    component: Component,
    linker: Linker<Ctx>,
}

/// One call to `run`, with timings split into instantiation and the call itself.
pub struct Call {
    pub json: String,
    /// Time spent creating the store and instantiating the component; `None` for warm calls.
    pub instantiate: Option<Duration>,
    /// Wall time of the `run` call itself.
    pub call: Duration,
}

impl SpikeHost {
    /// Compile the component and set up a linker with WASI p2 (`add_to_linker_sync`).
    pub fn new(component_path: impl AsRef<Path>) -> Result<Self> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        let engine = Engine::new(&config).context("failed to create wasmtime engine")?;
        let component = Component::from_file(&engine, component_path.as_ref()).with_context(|| {
            format!(
                "failed to load component {}",
                component_path.as_ref().display()
            )
        })?;
        let mut linker = Linker::new(&engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker).context("failed to add WASI p2 to linker")?;
        Ok(Self {
            engine,
            component,
            linker,
        })
    }

    fn new_store(&self) -> Result<Store<Ctx>> {
        let mut builder = WasiCtxBuilder::new();
        // Read-only preopen of the whole host filesystem, exactly like the planned
        // host runtime. Guest paths must be absolute for this to be reachable.
        builder
            .preopened_dir("/", "/", FsPerms::ReadOnly)
            .context("failed to preopen / read-only")?;
        let empty: &[(&str, &str)] = &[];
        builder.envs(empty);
        Ok(Store::new(
            &self.engine,
            Ctx {
                ctx: builder.build(),
                table: ResourceTable::new(),
            },
        ))
    }

    /// Instantiate the component once so [`Self::call_warm`] can be measured without setup cost.
    pub fn instantiate(&self) -> Result<(Store<Ctx>, SpikeWorld)> {
        let mut store = self.new_store()?;
        let world = SpikeWorld::instantiate(&mut store, &self.component, &self.linker)
            .context("failed to instantiate the spike component")?;
        Ok((store, world))
    }

    /// Call `run(root)` on an already instantiated component.
    pub fn call_warm(&self, store: &mut Store<Ctx>, world: &SpikeWorld, root: &str) -> Result<Call> {
        let start = Instant::now();
        let json = world
            .call_run(&mut *store, root)
            .context("component trapped during run")?
            .map_err(|err| Error::msg(format!("component returned an error: {err}")))?;
        Ok(Call {
            json,
            instantiate: None,
            call: start.elapsed(),
        })
    }

    /// Instantiate and call once; `Call::instantiate` carries the setup time.
    pub fn run_cold(&self, root: &str) -> Result<Call> {
        let start = Instant::now();
        let (mut store, world) = self.instantiate()?;
        let instantiate = start.elapsed();
        let mut call = self.call_warm(&mut store, &world, root)?;
        call.instantiate = Some(instantiate);
        Ok(call)
    }
}

/// Locate the component artifact:
/// 1. `$GIX_SPIKE_COMPONENT` if set,
/// 2. otherwise `$CARGO_TARGET_DIR/wasm32-wasip2/debug/gix_feasibility.wasm`.
///
/// The wasm32-wasip2 artifact is the one the M1 host will load (no preview1
/// adapter involved); `cargo component build --target wasm32-wasip2` also
/// builds the wasm32-wasip1+adapter variant as a cargo-component 0.21 quirk.
pub fn default_component_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("GIX_SPIKE_COMPONENT") {
        return Ok(PathBuf::from(path));
    }
    let target_dir = std::env::var_os("CARGO_TARGET_DIR").ok_or_else(|| {
        Error::msg("set CARGO_TARGET_DIR (or GIX_SPIKE_COMPONENT); the spike must never build into ./target")
    })?;
    Ok(PathBuf::from(target_dir)
        .join("wasm32-wasip2")
        .join("debug")
        .join("gix_feasibility.wasm"))
}

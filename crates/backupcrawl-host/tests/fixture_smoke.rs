//! M1.1 acceptance: the checked-in fixture detector component instantiates in
//! wasmtime and answers the detector calls with its documented behavior.
//!
//! This is intentionally minimal; M1.4 replaces it with the real runtime
//! (`backupcrawl-host` runtime/imports/limits modules).

use std::path::PathBuf;

use wasmtime::component::{Component, HasSelf, Linker, ResourceTable};
use wasmtime::{Engine, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

/// Bindings for the canonical `plugin` world. Generated code is not ours to
/// lint.
#[allow(warnings)]
mod bindings {
    wasmtime::component::bindgen!({
        path: "../backupcrawl-wit/wit",
        world: "plugin",
        additional_derives: [PartialEq],
    });
}

use bindings::Plugin;
use bindings::backupcrawl::detector::host::{
    ExecRequest, ExecResult, FileInfo, HttpRequest, HttpResponse,
};
use bindings::exports::backupcrawl::detector::detector::{Claim, LinkInfo, ScanInfo};

/// Host state: the WASI context plus the generated detector host imports.
struct State {
    ctx: WasiCtx,
    table: ResourceTable,
}

impl WasiView for State {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.ctx,
            table: &mut self.table,
        }
    }
}

/// Minimal M1.1 host imports: no capability is granted yet, so everything but
/// `log` reports an error. M1.4 implements the real imports.
impl bindings::backupcrawl::detector::host::Host for State {
    fn stat(&mut self, _path: String) -> Result<FileInfo, String> {
        Err("stat is not implemented yet (M1.4)".to_string())
    }

    fn exec(&mut self, _req: ExecRequest) -> Result<ExecResult, String> {
        Err("exec is not implemented yet (M1.4)".to_string())
    }

    fn cache_get(&mut self, _key: String) -> Option<Vec<u8>> {
        None
    }

    fn cache_set(&mut self, _key: String, _value: Vec<u8>) -> Result<(), String> {
        Err("cache is not implemented yet (M1.4)".to_string())
    }

    fn http(&mut self, _req: HttpRequest) -> Result<HttpResponse, String> {
        Err("http is not implemented yet (M1.4)".to_string())
    }

    fn log(&mut self, level: String, message: String) {
        eprintln!("fixture plugin log [{level}]: {message}");
    }
}

impl bindings::backupcrawl::detector::types::Host for State {}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/test-detector.wasm")
}

#[test]
fn fixture_component_instantiates_and_runs() -> anyhow::Result<()> {
    let engine = Engine::default();
    let component = Component::from_file(&engine, fixture_path())?;

    let mut linker = Linker::new(&engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker)?;
    Plugin::add_to_linker::<_, HasSelf<_>>(&mut linker, |state| state)?;

    let mut store = Store::new(
        &engine,
        State {
            ctx: WasiCtxBuilder::new().build(),
            table: ResourceTable::new(),
        },
    );

    let plugin = Plugin::instantiate(&mut store, &component, &linker)?;
    let detector = plugin.backupcrawl_detector_detector();

    let scan_info = ScanInfo {
        root: "/".to_string(),
        user: "fixture".to_string(),
        follow_symlinks: false,
        config: Vec::new(),
    };
    assert_eq!(detector.call_init(&mut store, &scan_info)?, Ok(()));

    assert_eq!(
        detector.call_check_file(&mut store, "/fixture.txt")?,
        Ok(None)
    );
    assert_eq!(detector.call_check_dir(&mut store, "/fixture")?, Ok(None));
    assert_eq!(
        detector.call_check_symlink(
            &mut store,
            &LinkInfo {
                path: "/fixture-link".to_string(),
                target: "/fixture.txt".to_string(),
                dangling: false,
            },
        )?,
        Ok(None)
    );

    let paths = vec!["/a.txt".to_string(), "/b.txt".to_string()];
    let claims: Vec<Option<Claim>> = detector
        .call_check_files(&mut store, &paths)?
        .expect("check-files must succeed");
    assert_eq!(claims, vec![None, None]);

    assert_eq!(detector.call_finish(&mut store)?, Ok(()));

    Ok(())
}

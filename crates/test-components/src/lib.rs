//! Fixture detector component for host tests.
//!
//! Implements the canonical `backupcrawl:detector@1.0.0` `plugin` world with raw
//! `wit-bindgen`. Behavior is selected by the `scenario` entry of the plugin's
//! configuration table delivered to `init`:
//!
//! | `scenario` | behavior |
//! | --- | --- |
//! | *(empty)* / `ok` | `init`/`finish` succeed; checks return no claim |
//! | `init-error` | `init` returns an error (plugin must be disabled) |
//! | `finish-error` | `finish` returns an error |
//! | `error` | checks return an error |
//! | `trap` | checks abort (a wasm trap) |
//! | `loop` | checks spin forever (fuel exhaustion / cancellation) |
//! | `stat` | calls `host.stat` on `stat-path`; returns metadata as attrs |
//! | `read` | reads `read-path` with `std::fs`; returns `content` or an `error` |
//! | `exec` | calls `host.exec` for `program`/`args`; returns code/streams |
//! | `cache` | `cache-get`/`cache-set` round-trip for `cache-key`/`cache-value` |
//! | `http` | calls `host.http` for `url`; returns status/body |
//! | `log` | calls `host.log` |
//!
//! `args` is split on the ASCII unit separator (`\u{1f}`). Failures from a host
//! import are returned as an `unknown` claim with an `error` attribute so the
//! host tests can assert on the message.
//!
//! The `read` scenario is the one place that touches file *content*: it uses
//! `std::fs` directly (WASI preopens), and its success/failure proves the host
//! granted exactly the roots it should. The wasm-only glue is gated with
//! `#[cfg(target_family = "wasm")]` so plain `cargo clippy`/`cargo test`
//! compile this crate as an empty library; only `cargo component build`
//! (wasm32-wasip1) sees the component. Rebuild and refresh the checked-in
//! artifact with `scripts/build-test-components.sh`.

#[cfg(target_family = "wasm")]
mod component {
    use std::cell::RefCell;

    wit_bindgen::generate!({
        path: "wit",
        world: "plugin",
    });

    use backupcrawl::detector::host::{
        ExecRequest, FileKind, HttpRequest, cache_get, cache_set, exec, http, log, stat,
    };
    use backupcrawl::detector::types::{Attr, Status};
    use exports::backupcrawl::detector::detector::{Claim, Guest, LinkInfo, ScanInfo};

    /// Scenario and configuration captured from `init`.
    #[derive(Default, Clone)]
    struct Fixture {
        scenario: String,
        config: Vec<(String, String)>,
    }

    thread_local! {
        static FIXTURE: RefCell<Fixture> = RefCell::new(Fixture::default());
    }

    fn scenario() -> String {
        FIXTURE.with(|fixture| fixture.borrow().scenario.clone())
    }

    fn config(key: &str) -> Option<String> {
        FIXTURE.with(|fixture| {
            fixture
                .borrow()
                .config
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
        })
    }

    fn attr(key: impl Into<String>, value: impl Into<String>) -> Attr {
        Attr {
            key: key.into(),
            value: value.into(),
        }
    }

    fn claim(status: Status, attrs: Vec<Attr>) -> Claim {
        Claim {
            status,
            attrs,
            prune: false,
        }
    }

    fn error_claim(message: &str) -> Claim {
        claim(Status::Unknown, vec![attr("error", message)])
    }

    /// The shared check implementation for every scenario.
    fn handle(path: &str) -> Result<Option<Claim>, String> {
        match scenario().as_str() {
            "" | "ok" => Ok(None),
            "error" => Err("fixture check failure".to_string()),
            "trap" => std::process::abort(),
            "loop" => {
                let mut spin: u64 = 0;
                loop {
                    spin = std::hint::black_box(spin.wrapping_add(1));
                }
            }
            "stat" => {
                let target = config("stat-path").unwrap_or_else(|| path.to_string());
                match stat(&target) {
                    Ok(info) => {
                        let kind = match info.kind {
                            FileKind::File => "file",
                            FileKind::Directory => "directory",
                            FileKind::Symlink => "symlink",
                            FileKind::Other => "other",
                        };
                        Ok(Some(claim(
                            Status::Dirty,
                            vec![
                                attr("kind", kind),
                                attr("size", info.size.to_string()),
                                attr("mode", info.mode.to_string()),
                                attr("uid", info.uid.to_string()),
                                attr("gid", info.gid.to_string()),
                                attr("nlink", info.nlink.to_string()),
                                attr("mtime-ms", info.mtime_ms.to_string()),
                            ],
                        )))
                    }
                    Err(message) => Ok(Some(error_claim(&message))),
                }
            }
            "read" => {
                let target = config("read-path").unwrap_or_else(|| path.to_string());
                match std::fs::read_to_string(&target) {
                    Ok(content) => {
                        let len = content.len().to_string();
                        Ok(Some(claim(
                            Status::Dirty,
                            vec![attr("content", content), attr("len", len)],
                        )))
                    }
                    Err(err) => Ok(Some(error_claim(&format!("read `{target}`: {err}")))),
                }
            }
            "exec" => {
                let program = config("program").unwrap_or_default();
                let args = config("args")
                    .map(|raw| {
                        if raw.is_empty() {
                            Vec::new()
                        } else {
                            raw.split('\u{1f}').map(str::to_string).collect()
                        }
                    })
                    .unwrap_or_default();
                let request = ExecRequest {
                    program,
                    args,
                    cwd: None,
                    env: Vec::new(),
                    stdin: config("stdin").map(String::into_bytes),
                    timeout_ms: config("timeout-ms").and_then(|raw| raw.parse().ok()),
                };
                match exec(&request) {
                    Ok(result) => Ok(Some(claim(
                        Status::Dirty,
                        vec![
                            attr("code", result.code.to_string()),
                            attr(
                                "stdout",
                                String::from_utf8_lossy(&result.stdout).into_owned(),
                            ),
                            attr(
                                "stderr",
                                String::from_utf8_lossy(&result.stderr).into_owned(),
                            ),
                            attr("timed-out", result.timed_out.to_string()),
                        ],
                    ))),
                    Err(message) => Ok(Some(error_claim(&message))),
                }
            }
            "cache" => {
                let key = config("cache-key").unwrap_or_else(|| "default".to_string());
                let value = config("cache-value").unwrap_or_default().into_bytes();
                let (hit, stored) = match cache_get(&key) {
                    Some(existing) => (true, existing),
                    None => {
                        cache_set(&key, &value)?;
                        (false, value)
                    }
                };
                Ok(Some(claim(
                    Status::Dirty,
                    vec![
                        attr("cache-hit", hit.to_string()),
                        attr("cache-value", String::from_utf8_lossy(&stored).into_owned()),
                    ],
                )))
            }
            "http" => {
                let request = HttpRequest {
                    method: "GET".to_string(),
                    url: config("url").unwrap_or_default(),
                    headers: Vec::new(),
                    body: None,
                };
                match http(&request) {
                    Ok(response) => Ok(Some(claim(
                        Status::Dirty,
                        vec![
                            attr("status", response.status.to_string()),
                            attr("body", String::from_utf8_lossy(&response.body).into_owned()),
                        ],
                    ))),
                    Err(message) => Ok(Some(error_claim(&message))),
                }
            }
            "log" => {
                log("info", "fixture log message");
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    struct TestDetector;

    impl Guest for TestDetector {
        fn init(info: ScanInfo) -> Result<(), String> {
            let config: Vec<(String, String)> = info
                .config
                .into_iter()
                .map(|entry| (entry.key, entry.value))
                .collect();
            let selected = config
                .iter()
                .find(|(key, _)| key == "scenario")
                .map(|(_, value)| value.clone())
                .unwrap_or_default();
            FIXTURE.with(|fixture| {
                let mut fixture = fixture.borrow_mut();
                fixture.scenario = selected.clone();
                fixture.config = config;
            });

            if selected == "init-error" {
                return Err("fixture init failure".to_string());
            }
            Ok(())
        }

        fn check_dir(path: String) -> Result<Option<Claim>, String> {
            handle(&path)
        }

        fn check_file(path: String) -> Result<Option<Claim>, String> {
            handle(&path)
        }

        fn check_symlink(link: LinkInfo) -> Result<Option<Claim>, String> {
            handle(&link.path)
        }

        /// Bulk variant: exactly N singular calls.
        fn check_files(paths: Vec<String>) -> Result<Vec<Option<Claim>>, String> {
            paths.into_iter().map(Self::check_file).collect()
        }

        fn finish() -> Result<(), String> {
            if scenario() == "finish-error" {
                return Err("fixture finish failure".to_string());
            }
            Ok(())
        }
    }

    export!(TestDetector);
}

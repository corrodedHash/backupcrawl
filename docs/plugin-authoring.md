# Writing a backupcrawl detector plugin

A **detector** is a WebAssembly component that answers one question for a path:
*has some backup mechanism captured it?* The host binary walks the filesystem,
asks every enabled detector about each path in priority order, and renders the
result. This document is the hands-on guide; the design rationale lives in
[`rust-rewrite-plan.md`](rust-rewrite-plan.md) (§3–§5).

The bundled [`plugins/git`](../plugins/git) detector is the reference
implementation, [`plugins/pacman`](../plugins/pacman) is the second bundled
detector (native database parsing, no `exec`) and
[`plugins/syncthing`](../plugins/syncthing) shows the `http` import talking to a
localhost REST API. The SDK ships a complete worked example at
`crates/backupcrawl-plugin-sdk/example-plugin`.

## The model: three questions per path

Every detector answers the same three questions for whatever it manages
(§3 of the plan):

| Question | Meaning | Example primitives |
| --- | --- | --- |
| **Managed?** | Does this mechanism own the path at all? | `.git` marker, `pacman -Ql`, `chezmoi managed` |
| **Current?** | Does the mechanism's copy match the live content? | `git status`, mtree comparison |
| **Propagated?** | Has the current state left this machine? | `branch.ab`, syncthing `needFiles` |

The verdict is a `Status` on a `Claim`: `clean`, `dirty`, `ahead` (rendered
as "Unsynced"), `unknown` (managed but the state could not be determined) or
`ignored` (deliberately excluded: `.gitignore`, `.stignore`). Returning `Ok(None)`
means "not managed by me"; the first detector that returns `Some(claim)` wins,
so a path belongs to exactly one mechanism (§5.4).

## Crate setup

A plugin is a normal `cdylib` crate whose only dependency is the SDK. The
`wit` directory must point at the canonical contract so the generated exports
match the host:

```
my-plugin/
├── Cargo.toml
├── plugin.toml          # the manifest (see below)
├── src/lib.rs
└── wit -> ../../crates/backupcrawl-wit/wit    # symlink
```

```toml
[package]
name = "backupcrawl-plugin-mine"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[package.metadata.component]
target = { path = "wit", world = "plugin" }

[dependencies]
backupcrawl-plugin-sdk = { path = "../../crates/backupcrawl-plugin-sdk" }
```

Keep the dependency under its canonical crate name (no `package = "..."` rename):
the macro's generated glue refers to `backupcrawl_plugin_sdk::rt`.

## Writing the detector

Implement the SDK `Detector` trait — every method has a default, so override
only what you need — and end the file with `bind_detector!`, which emits the
component export glue for the canonical `plugin` world:

```rust
use std::path::Path;
use backupcrawl_plugin_sdk::{
    Claim, Detector, DetectorError, Host, LinkInfo, ScanInfo, bind_detector,
};

#[derive(Default)]
struct Mine {
    suffix: String,
}

impl Detector for Mine {
    fn init(&mut self, info: &ScanInfo, _host: &mut dyn Host) -> Result<(), DetectorError> {
        // `info.config` is this plugin's `[plugins.mine]` table.
        self.suffix = info.config.get("suffix").unwrap_or(".bak").to_owned();
        Ok(())
    }

    fn check_dir(&mut self, path: &Path, host: &mut dyn Host) -> Result<Option<Claim>, DetectorError> {
        // A directory claim with `prune` stops the descent; without it the
        // walker descends and still asks about the files inside.
        if host.stat(&path.join(".mine")).is_ok() {
            Ok(Some(Claim::clean().attr("kind", "managed").prune()))
        } else {
            Ok(None)
        }
    }

    fn check_file(&mut self, path: &Path, _host: &mut dyn Host) -> Result<Option<Claim>, DetectorError> {
        match path.extension().and_then(|e| e.to_str()) {
            Some(ext) if format!(".{ext}") == self.suffix => {
                Ok(Some(Claim::dirty().attr("reason", "unbacked")))
            }
            _ => Ok(None),
        }
    }

    fn check_symlink(&mut self, link: &LinkInfo, _host: &mut dyn Host) -> Result<Option<Claim>, DetectorError> {
        Ok(link.dangling.then(|| Claim::dirty().attr("reason", "dangling"))))
    }
}

bind_detector!(Mine);
```

Notes:

- `Claim` has builders: `Claim::clean()`, `Claim::dirty()`, `Claim::ahead()`,
  `Claim::unknown()`, `Claim::ignored()`, plus `.attr(key, value)` and `.prune()`.
  Attributes are free-form and shown in `--all` output / JSON.
- `check_files` has a default implementation that is exactly N `check_file`
  calls. **Do not change the observable behavior** if you override it for speed:
  the `@1` contract requires bulk and singular calls to agree.
- `check_symlink` is called for every symlink the walker finds (M2). The link
  is never followed; `LinkInfo.path` is the link itself and `LinkInfo.target` is
  the target made absolute and lexically normalized, with `dangling` set when
  the target does not exist. Unclaimed links become `SymlinkEntry` rows that the
  host always reports.
- `finish` runs once after the scan for diagnostics or flushing caches.
  Returning `Err` from `init` disables your plugin for the scan with a visible
  warning; errors from a check call count as "no claim".

## Host services

Detector logic never touches the OS directly. Every privileged operation goes
through the `Host` trait, and the host grants only what the manifest declares:

| Method | Purpose | Capability |
| --- | --- | --- |
| `stat(path)` | Unix metadata WASI omits: `mode`, `uid`, `gid`, `nlink`, `size`, `mtime` | always |
| `exec(request)` | Run an allowlisted program with args/env/stdin/timeout | `exec = ["prog"]` |
| `cache_get`/`cache_set` | Persistent per-plugin key/value store under `$XDG_CACHE_HOME/backupcrawl/plugins/<name>/` | `cache = true` |
| `http(request)` | HTTP to an allowlisted origin (localhost-only by default) | `http = ["host"]` |
| `log(level, message)` | Structured logging | always |

The syncthing detector is the worked example for `http`: it queries
`GET http://127.0.0.1:8384/rest/db/status?folder=<id>` with an `X-API-Key`
header, maps `state`/`needFiles`/`pullErrors`/`receiveOnlyChanged*`, and caches
one response per folder per scan (the endpoint is documented as expensive).
Its manifest grants `http = ["http://127.0.0.1:8384"]`,
`fs-read = ["~/.local/state/syncthing", "~/.config/syncthing"]` and
`cache = true`; `[plugins.syncthing]` accepts `address`, `api-key`,
`require-daemon`, `assume-marker-only`, `receive-only-changes` and `timeout-ms`
(plus the `config-dir` test hook).

File **content** is read through standard `std::fs` / WASI. Since M2.0 the host
grants read-only preopens for the **crawl root** plus every resolved
`[capabilities].fs-read` root, each mounted at its own absolute path, so
`std::fs` and crates like `gix` work while paths outside the grant stay
invisible and wasmtime rejects every write. The guest environment contains no
host variables except the path variables `HOME`, `STCONFDIR` and the XDG base
directories (`XDG_CONFIG_HOME`, `XDG_STATE_HOME`, `XDG_DATA_HOME`), which is
what `~`-relative config discovery uses. An `fs-read` spec may use a leading
`~` and shell-style globs (`~/.local/state/syncthing*`) and must resolve to an
existing directory; an unresolved spec is a warning, not a load error. A
plugin with genuinely system-wide needs declares `/` and is shown as
`fs: read /`. `exec` is the escape hatch — a spawned child runs with the user's
rights and is *not* sandboxed, which is why `plugins info` and the trust prompt
print it first.

## Concurrency: one instance per worker

Parallel crawling (M4a) runs **one detector instance per worker**, so a plugin
must not assume it is the only instance in the process. The frozen contract:

- `init`/`finish` bracket that instance's lifetime. A scan with N workers calls
  your `init` and `finish` N times; keep both idempotent and side-effect-free
  apart from the host cache.
- Instances share no mutable state — no process-global caches, no static
  `Mutex` state that assumes a single instance. Global preparation (for example
  a package database listing) belongs in the host cache via
  `cache_get`/`cache_set`, where concurrent writers must be benign (last write
  wins is fine).
- The first worker's `init` is serialized and warms the cache; the rest then hit
  it. Do not rely on `init` running exactly once.
- There are no cross-worker ordering guarantees. First-claim-wins is still
  evaluated per path, and every path is handed to exactly one worker.
- Output stays deterministic: the host merges and sorts per-worker claims, so
  `--jobs 1` and `--jobs N` produce identical output.

Treat each instance as independent: build all state in `init`, read shared
inputs through the cache (or `std::fs`), and flush in `finish`.

## Testing natively (no wasm toolchain)

Because the logic talks to the `Host` trait, it runs under plain `cargo test`
with `MockHost`. Stage the responses the detector should see, then assert on
what it did:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use backupcrawl_plugin_sdk::{Config, FileInfo, MockHost};

    #[test]
    fn backup_suffix_is_dirty() {
        let mut detector = Mine::default();
        let mut host = MockHost::new().with_stat(
            "/data/.mine",
            FileInfo { mode: 0o644, nlink: 1, ..MockHost::file_info(0) },
        );
        let info = ScanInfo {
            root: "/data".into(),
            user: "tester".into(),
            follow_symlinks: false,
            config: Config::default(),
        };
        detector.init(&info, &mut host).unwrap();

        let claim = detector.check_file(Path::new("/data/notes.txt.bak"), &mut host).unwrap();
        assert_eq!(claim.unwrap().status, backupcrawl_plugin_sdk::Status::Dirty);
    }
}
```

`MockHost` fails on any call you did not stage, so a test cannot silently pass
on missing setup. It also exposes `logs()`, `exec_requests()`, `http_requests()`
and `cache()` for assertions.

## The manifest (`plugin.toml`)

The manifest is a sidecar next to the `.wasm` (same name as the plugin, or
`plugin.wasm`). Only `[plugin]` is mandatory:

```toml
[plugin]
name = "mine"                     # unique; selects [plugins.mine] and the trust entry
version = "0.1.0"
api = "1"                         # must be major 1 (backupcrawl:detector@1.0.0)
description = "Mine detector"
license = "MIT"

[hooks]                           # which filesystem objects you want to see
dir = true
file = true
symlink = false
priority = 50                     # lower = asked earlier; default 100

[capabilities]
exec = []                         # program allowlist
fs-read = ["~/.config/mine"]      # read-only roots (crawl root always granted)
http = []
cache = false

[config-schema]                   # optional JSON schema for [plugins.mine]
type = "object"
```

Validation rules (§5.2):

- `api` must be major `1`; anything else is a load error.
- An **unknown capability** name (for example a misspelled `execs`) is a load
  error — capability entries are things you *require*.
- Unknown keys in `[plugin]` / `[hooks]` are **warnings**, so a typo like
  `files = true` is visible instead of silently disabling a hook.
- `priority` orders detectors; the user can override it with `[plugins] order`.

## Building

Build with `cargo-component` (0.21.1 in this workspace) from the workspace root:

```console
$ cargo component build -p backupcrawl-plugin-mine --release
```

The component lands under
`<CARGO_TARGET_DIR>/<triple>/<profile>/<crate_name>.wasm` (default triple
`wasm32-wasip1`; pass `--target wasm32-wasip2` for the wasip2 target). The two
scripts are the working reference:

- `scripts/build-example-plugin.sh` — SDK example, default target.
- `scripts/build-git-plugin.sh` — bundled git detector, `--target wasm32-wasip2`,
  reproducible with `CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1`.
- `scripts/build-pacman-plugin.sh` — bundled pacman detector, same pattern.
- `scripts/build-syncthing-plugin.sh` — bundled syncthing detector, same
  pattern; its golden tests run the component against a local fake REST daemon.

> Reproducibility: the committed fixture artifacts are part of the repo and CI
> fails if they are stale. Build the bundled detectors through their scripts:
> they source `scripts/wasm-build-env.sh`, which applies a single release codegen
> unit and remaps the machine-specific `CARGO_HOME`, `RUSTUP_HOME`, checkout and
> target prefixes to stable placeholders. rustc otherwise embeds absolute paths
> in panic locations, so the same source would produce different bytes on a
> different machine and the CI staleness check would fail.

## Installing, discovery and trust

Drop a directory containing `plugin.toml` plus the `.wasm` into one of the
source locations; the first source to define a name wins (§5.2):

1. `--plugin-dir DIR` (repeatable, in order),
2. `$XDG_CONFIG_HOME/backupcrawl/plugins/`,
3. `/usr/lib/backupcrawl/plugins/`,
4. bundled plugins embedded in the binary.

The sidecar is resolved as `<name>.wasm`, then `plugin.wasm`, then the single
`*.wasm` file in the directory.

Bundled plugins are auto-trusted (they ship inside the binary). A third-party
plugin whose manifest hash is not in `[plugins.trust]` triggers a prompt that
prints its capabilities and hash; non-interactive runs either pass
`--trust-plugin=NAME` or report the plugin as untrusted. Manage the set with:

```console
$ backupcrawl plugins list
$ backupcrawl plugins info mine
$ backupcrawl plugins verify
```

## Current limitations (v1)

- `fs-read` is authoritative but coarse: the host enforces preopen-level access
  only (no per-path read auditing), and `stat` is unconditional and
  metadata-only. `exec` children are not sandboxed.
- Symlinks are reported, never followed: `--follow-symlinks` is deferred (M3+).
  The WASI sandbox also refuses `std::fs::read_link` for a link whose *stored
  target is absolute* (`EPERM`); use `LinkInfo.target` from `check_symlink` for
  those, which the walker computed outside the sandbox.
- The host uses singular `check_file` calls rather than `check_files` (bulk calls
  remain an optimization; M4b+).
- Parallel crawling is live: `--jobs N` runs N workers, each with its own instance
  under the per-worker contract above. `--progress` is a host-side stderr display
  and never reaches plugins.
- `[config-schema]` is parsed but not enforced; validate your own config in
  `init` and return a `DetectorError` for bad values.
- `exec` children are not sandboxed; keep the allowlist as small as possible.

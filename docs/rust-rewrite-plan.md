# Rust rewrite: plugin architecture and detection methods

Status: draft for discussion · 2026-10-11 · relates to backupcrawl 0.2.6 (Python)

## 1. Goals and non-goals

Goals:

- Rewrite backupcrawl in Rust: single fast binary, parallel crawl, no Python runtime.
- Every backup mechanism is a separate, replaceable detection module ("detector").
- Detectors are WebAssembly (component model) plugins loaded at runtime, not compiled into the core.
- Core only knows: how to walk the filesystem, how to query plugins, how to aggregate and print.
- The four mechanisms that matter first: **git, pacman, syncthing, chezmoi**. Symlinks are a
  first-class filesystem object handled by the core, with plugins allowed to claim them.
- Replace the Python implementation at the repository root; the Python code lives on only in git
  history (tagged before the rewrite). There are no users, so no compatibility is owed.

Non-goals:

- Performing, configuring or repairing backups.
- Scanning remote machines.
- A plugin marketplace or a TUI permission editor (a trust prompt + config allowlist is enough).

## 2. What the Python version does today (reference behavior)

- `crawler.scan()` walks depth-first. Per directory it runs all `DirChecker`s; the first checker
  that returns a status other than `NONE` claims the whole subtree and it is not descended further.
  Otherwise, per file, all `FileChecker`s run; the first claim wins, unclaimed files become
  "not backed up" (`loose_paths`).
- `SyncStatus`: `NONE` (unmanaged), `CLEAN`, `DIRTY` (managed, content not captured),
  `AHEAD`/`Unsynced` (captured locally, not propagated).
- `git_check`: `.git` directory → `git status --porcelain` for dirty, plus
  `git for-each-ref --format='%(upstream:trackshort)'` for ahead.
- `pacman_check`: `pacman -Ql` builds a filename→package map once; `pacman -Qkk` builds a
  dirty-file set once; unknown "reasons" in `-Qkk` output are collected for diagnostics.
- Symlinks are skipped entirely (neither reported nor followed).
- Output: rich console panels (one per mechanism, split into Dirty/Unsynced/Clean) and JSON.
- Config: `~/.config/backupcrawlrc.json` with `ignore_paths`; CLI flags `path`, `-v`, `-a`,
  `-i`, `-p`, `-f json|console`.

Issues found while reading, which the rewrite should fix or consciously keep:

1. **Ahead detection is wrong for repos without upstream.** `%(upstream:trackshort)` prints `''`
   for "no upstream", and the code counts `''` as ahead. Local-only branches are always "Unsynced".
2. **Ignored files are invisible.** A `.gitignore`d file inside a clean repo is reported clean
   although it is not in the repository.
3. **Symlinks are dropped.** File symlinks and dotfile-farm trees are neither reported nor traversed.
4. **pacman output parsing is fragile.** `pacman -Ql` lines are split with `split(maxsplit=1)`;
   paths containing spaces break the map; directory entries (trailing `/`) are included.
5. **Two `git` processes per clean repo** (`status` + `for-each-ref`); no batching.
6. **Nested repos inside a dirty repo are never visited** because any claim prunes the subtree.
7. Ancestor repositories are ignored: crawling `/home/u/project` where `/home/u` is the repo root
   reports everything as loose.

## 3. The conceptual model: three questions per mechanism

Every detector must answer the same three questions for a path, whatever the mechanism:

| Question | Meaning | Examples of detection primitives |
| --- | --- | --- |
| **1. Managed?** | Is this path owned/tracked by the mechanism at all? | marker files/dirs (`.git`, `.stfolder`), ownership databases (`pacman -Ql`, pacman db), tool listings (`chezmoi managed`) |
| **2. Current?** | Does the mechanism's copy match the live content? | VCS status (`git status`), integrity check (`pacman -Qkk`), metadata/hash compare (mtree), tool diff (`chezmoi status`) |
| **3. Propagated?** | Has the current state left this machine? | `git branch.ab`, syncthing `needFiles`/`state`, "source repo pushed" (chezmoi), package repos (pacman) |

Status mapping (keep the existing enum, lowercase in Rust):

| Managed | Current | Propagated | Status |
| --- | --- | --- | --- |
| no | – | – | *(no claim → unmanaged/loose)* |
| yes | yes | yes | `clean` |
| yes | no | – | `dirty` |
| yes | yes | no | `ahead` |
| yes | unknown | unknown | `unknown` *(new, see below)* |
| yes, but deliberately excluded | – | – | `ignored` *(new, optional, see below)* |

Two status extensions are proposed because the four-value enum cannot express two real cases:

- `unknown`: mechanism is present but the state cannot be determined (syncthing daemon not
  running, git repo in a weird state, tool execution failed). Reported separately; **never**
  silently treated as clean or dirty.
- `ignored`: the mechanism explicitly excludes the path (`.gitignore`, `.stignore`) so it is
  *not* captured, even though the surrounding tree is. This is unbacked user data and should be
  visible. Default output hides it behind `--ignored`/config because build directories would
  otherwise flood the report.

Note on semantics: "in sync" is not the same as "backed up". Syncthing replicates to peers, not
to an archive; a laptop and a desktop that both burn are still both gone. backupcrawl reports
mechanism state, it cannot judge whether the other side is a real backup. The doc for each plugin
should say this explicitly.

## 4. Target architecture

```
┌──────────────────────────── backupcrawl (host binary) ──────────────────────────────┐
│ backupcrawl-cli        clap CLI, config load, output selection                       │
│ backupcrawl-core       walker, ignore matching, result model, printers, progress     │
│ backupcrawl-host       wasmtime component runtime, manifests, capability policy,     │
│                        host imports (stat/exec/cache/http/log), plugin lifecycle        │
│ backupcrawl-wit        WIT contract (single source of truth for the plugin ABI)      │
├──────────────────────────────────────────────────────────────────────────────────────┤
│ Detector trait (core-facing façade) — one impl for wasm, one native for tests        │
├──────────────────────────────────────────────────────────────────────────────────────┤
│  wasmtime components, one instance per detector per worker:                          │
│    [git] [pacman] [chezmoi] [syncthing] [third-party ...]                            │
│  each: plugin.toml manifest (name, version, api, hooks, capabilities, config schema) │
└──────────────────────────────────────────────────────────────────────────────────────┘
```

Key decisions:

- **Runtime: wasmtime + component model (WIT).** Reasons: typed, versioned ABI; language-agnostic
  plugins; no ambient authority (no filesystem, no network, no clock unless the host grants it);
  fuel/epoch limits can stop runaway guest code. Pin the toolchain (`wit-bindgen` /
  `cargo-component`, target `wasm32-wasip2`, WIT package version `backupcrawl:detector@1`).
- **Alternative considered: Extism** (a wasmtime wrapper with a JSON-oriented ABI). Faster to
  bootstrap, but the ABI is less precise (bytes + serde in and out), capability control is
  host-function-only, and there is no component-level type checking. Wasmtime + WIT gives a
  better long-term contract. It remains the fallback if component tooling turns out to be a
  maintenance burden.
- **Alternative considered: native trait objects / dylib plugins.** Fastest, but ABI-fragile,
  unsafe boundary, no sandbox, and every new detector means a rebuild and trust decision in the
  core. Rejected.
- **Filesystem access (M2.0): read-only preopens for the crawl root plus `fs-read`.** The host
  resolves each manifest `fs-read` spec (plain paths, `~`, shell-style globs) and maps every
  existing directory read-only at its absolute guest path; the crawl root is always granted.
  System-wide detectors say so explicitly (`git` and `pacman` declare `fs-read = ["/"]`), while
  third-party plugins get the scope they asked for. Writes remain impossible; `exec`/`http` stay
  allowlisted (Appendix A).
- **Transport is swappable.** Core code talks to a `Detector` trait, not to wasmtime:

  ```rust
  trait Detector: Send {
      fn init(&mut self, info: &ScanInfo) -> Result<(), DetectorError>;
      fn check_dir(&mut self, path: &Path) -> Result<Option<Claim>, DetectorError>;
      fn check_file(&mut self, path: &Path) -> Result<Option<Claim>, DetectorError>;
      fn check_symlink(&mut self, link: &LinkInfo) -> Result<Option<Claim>, DetectorError>;
      fn finish(&mut self) -> Result<(), DetectorError>;
  }
  ```

  `WasmDetector` is the production implementation; a native `FixtureDetector` serves tests.
  If WASM ever proves wrong, an out-of-process JSON-RPC detector can implement the same trait
  with the same WIT records (serde to JSON), without touching the crawler.

  > Implementation note (M1.6): the records above (`ScanInfo`, `Claim`/`Attr`, `LinkInfo`,
  > `DetectorError`) are defined in `backupcrawl-core::detector` and the wasm host re-exports
  > them, so `WasmDetector` implements the trait with no parallel type set. The trait also
  > carries an optional `report()` accessor (host error/note accounting for the run report) and
  > the WIT `claim.attrs` list is folded into `Entry.attrs` for deterministic JSON.

### Why WASM is still right (and why `exec` is minimized)

WASM gives ABI isolation and a real sandbox for the guest code itself: a detector cannot open
files, read environment variables, open sockets or spawn processes by accident — every privileged
operation goes through a capability the host grants. Those capabilities are declared in the
manifest and auditable. The chosen design minimizes `exec`: git reads repository data with a pure
Rust implementation (`gix`; feasibility spike in M1) and pacman parses its local database
directly, so only chezmoi (`exec = ["chezmoi"]`) among the bundled detectors spawns a process —
and `exec` stays available and allowlisted for third-party plugins. WASM does **not** sandbox
child processes: a detector with `exec = ["git"]` gives the spawned `git` the user's normal rights.
This is documented in the trust model (section 10).

## 5. Plugin contract

### 5.1 WIT sketch (illustrative, to be finalized in `backupcrawl-wit`)

```wit
package backupcrawl:detector@1.0.0;

interface types {
  enum status { clean, dirty, ahead, unknown, ignored }  // recoverable: deferred, see 7.6

  record attr { key: string, value: string }

  /// A detector's verdict for one path. `None` (no claim) means "not managed by me".
  record claim {
    status: status,
    attrs: list<attr>,      // e.g. package = "bash"; shown in --all output
    prune: bool,            // do not descend into this directory (default false)
  }

  record scan-info {
    root: string,
    user: string,
    follow-symlinks: bool,
    config: list<attr>,     // this plugin's config table from the rcfile
  }

  record link-info { path: string, target: string, dangling: bool }
}

interface detector {
  use types.{claim, link-info, scan-info};

  /// Called once per instance before the scan. Expensive preparation (global listings)
  /// belongs here. Returning an error disables the plugin for this scan.
  init: func(info: scan-info) -> result<_, string>;

  check-dir:     func(path: string) -> result<option<claim>, string>;
  check-file:    func(path: string) -> result<option<claim>, string>;
  check-symlink: func(link: link-info) -> result<option<claim>, string>;
  check-files:   func(paths: list<string>) -> result<list<option<claim>>, string>;  // bulk, optional

  finish: func() -> result<_, string>;
}
```

Host imports available to a detector (each gated by capability):

```wit
interface host {
  enum file-kind { file, directory, symlink, other }

  /// Unix metadata that WASI does not expose (mode/uid/gid/nlink). Metadata only, no content.
  record file-info {
    kind: file-kind, size: u64, mtime-ms: u64,
    mode: u32, uid: u32, gid: u32, nlink: u64,
  }
  stat: func(path: string) -> result<file-info, string>;

  record exec-request {
    program: string, args: list<string>, cwd: option<string>,
    env: list<tuple<string, string>>, stdin: option<list<u8>>, timeout-ms: option<u32>,
  }
  record exec-result { code: s32, stdout: list<u8>, stderr: list<u8>, timed-out: bool }
  exec: func(req: exec-request) -> result<exec-result, string>;

  cache-get: func(key: string) -> option<list<u8>>;
  cache-set: func(key: string, value: list<u8>) -> result<_, string>;

  record http-request  { method: string, url: string, headers: list<tuple<string, string>>, body: option<list<u8>> }
  record http-response { status: u16, headers: list<tuple<string, string>>, body: list<u8> }
  http: func(req: http-request) -> result<http-response, string>;

  log: func(level: string, message: string);
}
```

> File **content** is read through standard WASI filesystem access. Since M2.0 the host grants
> read-only preopens for the crawl root plus every resolved `fs-read` spec, each mapped at its
> own absolute guest path; a plugin that needs system-wide access must say so (the git and
> pacman detectors declare `fs-read = ["/"]`). The custom `stat` import above only fills the
> metadata gap WASI 0.2 leaves (mode/uid/gid/nlink). `exec`, `cache`, `http` and `log` are
> custom imports; WASI has no process spawning, and we want allowlists for the rest.
> Full reasoning: Appendix A.

Design notes:

- **Filesystem model (decided; Appendix A).** *Content:* standard WASI filesystem access through
  read-only preopens of the crawl root plus the manifest's resolved `fs-read` roots (M2.0), each
  at its absolute guest path, so `std::fs` and crates such as `gix` work while undeclared paths
  stay invisible. *Metadata:* the custom `stat` import exposes mode/uid/gid/nlink, which WASI 0.2
  omits; it is unconditional and metadata-only. No custom file-content reads exist.
- **Grants are visible.** The manifest's resolved `fs-read` roots and `exec`/`http` lists are
  printed by `plugins info` and the trust prompt, together with the manifest hash. Unresolved
  specs are warnings; a plugin that declares `/` is shown as `read /`.
- **State lives in the plugin instance** (one wasm instance = one mutable state); `init` builds
  caches, `finish` may flush to the host cache.
- **Bulk calls are an optimization only.** `check-files` exists so the host can pass all files of
  a directory in one crossing; it must behave exactly like N singular calls. Batch-of-one is
  always valid, so a plugin can implement only the singular methods first.

### 5.2 Lifecycle

1. **Discovery** — `.wasm` + `plugin.toml` sidecar in, in order: explicit `--plugin-dir` paths,
   `$XDG_CONFIG_HOME/backupcrawl/plugins/`, `/usr/lib/backupcrawl/plugins/`, bundled plugins
   shipped next to the binary. Same-name precedence: user dir wins over system dir wins over
   bundled.
2. **Validation** — parse manifest, check `api = "1"` compatibility, collect requested
   capabilities. Unknown manifest keys are a warning, unknown required capabilities are a load
   error.
3. **Trust** — a plugin whose manifest hash is not in the config trust list requires an explicit
   `--trust-plugin=NAME` or an interactive confirmation showing its capabilities (section 10).
4. **Instantiate** — wasmtime component with read-only preopens for the crawl root and resolved
   `fs-read` roots (M2.0), per worker (section 9), memory + fuel/epoch limits set.
5. **`init(scan-info)`** — expensive global prep (pacman database read, chezmoi `managed`,
   syncthing config parse). Error → plugin disabled for this scan with a visible warning; the
   scan continues.
6. **Scan** — `check_dir` on every visited directory, `check_file` on every regular file,
   `check_symlink` on every symlink, in configured order; first `Some(claim)` wins.
7. **`finish()`** — partial results / diagnostics; instance dropped. Errors are summarized in the
   run report (`plugins: git ok, syncthing disabled (daemon unreachable), ...`).

### 5.3 Manifest (`plugin.toml`)

```toml
[plugin]
name = "git"
version = "0.1.0"
api = "1"
description = "Git repositories and their sync state"
license = "MIT"

[hooks]                 # which filesystem objects the plugin wants to see
dir = true
file = true             # answered from the per-repo cache (dirty/ignored files)
symlink = true
priority = 10           # lower = asked earlier; default 100

[capabilities]
exec = []                             # program allowlist; git reads repos via gix, no exec needed
fs-read = ["~/.config/git"]           # authoritative read roots (crawl root is always granted)
http = []                             # e.g. ["http://127.0.0.1:8384"]
cache = true                          # persistent per-plugin key/value cache

[config-schema]         # optional JSON schema for the plugin's rcfile table
```

### 5.4 Failure handling and ordering

- A call returning `Err` or trapping: log once, count it, treat as "no claim" (path stays
  unmanaged), and trip a circuit breaker after N consecutive failures (disable plugin, report at
  the end). Config `on-plugin-error = "warn" | "fail"` (`fail` = non-zero exit).
- Ordering is declared (`priority`, then config override `[plugins] order = [...]`). Cheap and
  authoritative detectors should come first: git before pacman before chezmoi before syncthing.
- First claim wins, matching Python behavior. A file is either git-clean or pacman-dirty, not
  both; if that ever becomes a problem, a claim can carry `also-check = true` (open question 14.5).
- Determinism: outputs are sorted; plugin order is stable.

### 5.5 Authoring experience

- `backupcrawl-plugin-sdk` (Rust) wraps the host imports and exports a `bind_detector!` macro so
  a plugin is `struct Git { ... }` + `impl Detector for Git` + `bind_detector!(Git)`. The macro
  emits the component export glue.
- Detector logic is written against a small `Host` trait so it can be unit-tested natively with
  a mock (no wasm toolchain needed for `cargo test`). The wasm glue stays a thin shell.
- `backupcrawl plugin new <name>` scaffolds a repo; `cargo component build --release` produces
  the `.wasm`; `backupcrawl plugins check` validates manifest + ABI offline.

## 6. Crawl integration and result model

Host-side walker rules (mostly preserving Python semantics):

- `read_dir` each directory; classify entries: directory, regular file, symlink, other.
- **Ignore patterns** (globs, `globset`) applied to every entry before any plugin sees it. Same
  semantics as today's `ignore_paths` fnmatch, plus an optional default set for pseudo
  filesystems (`/proc`, `/sys`, `/dev`, `/run`).
- **Directories:** ask `check_dir` in detector order. First claim wins:
  - `prune = true` → record claim, do not descend (git repos, syncthing folder roots).
  - `prune = false` (default) → record claim and descend normally. A directory claim does not
    itself answer for the files inside unless the plugin also hooks `file`/`symlink`; this is how
    git reports `.gitignore`d files (`prune = false` + file hook served from its per-repo cache)
    and how a claimed package tree can still expose modified files.
- **Files:** ask `check_file` in order; bulk `check-files` per directory when supported.
  No claim → loose path.
- **Symlinks:** see section 8.
- **Permission denied / unreadable** paths → `denied_paths`, unchanged.
- **Sockets, FIFOs, device nodes** → ignored, unchanged.
- **Aggregation:** per-plugin groups, loose paths, denied paths, symlink entries. The existing
  "collapse loose paths to the highest directory that contained nothing backed up" behavior is
  kept (it is what makes `/usr` output readable).

Result model in Rust:

```rust
struct Entry { path: PathBuf, status: Status, attrs: BTreeMap<String, String> }
struct PluginReport { plugin: PluginId, entries: Vec<Entry>, errors: u32, notes: Vec<String> }
struct CrawlResult {
    loose: Vec<PathBuf>,
    denied: Vec<PathBuf>,
    plugins: Vec<PluginReport>,
    symlinks: Vec<SymlinkEntry>,   // unmanaged symlinks (section 8)
}
```

## 7. Required detection methods per mechanism

Summary first — the "methods required" question at a glance:

| Mechanism | 1. Managed? | 2. Current? | 3. Propagated? | Transport |
| --- | --- | --- | --- | --- |
| **git** | `.git` dir, `.git` file (worktree/submodule), bare repo | status records (porcelain-v2 semantics) | ahead/behind vs upstream ref | `gix` in-plugin; `exec git` only as fallback |
| **pacman** | `/var/lib/pacman/local/*/files` (+ `desc`) | `mtree` compare (needs `host-stat` for mode/uid/gid) | n/a — package repos are the archive; modified file = needs own backup | native db parse; `exec pacman` only as fallback |
| **syncthing** | `.stfolder` marker, folder entry in `config.xml` | local vs global version (`needFiles` etc., or receive-only changed) | `GET /rest/db/status` `state`, `needFiles` | config file + localhost REST |
| **chezmoi** | `chezmoi managed` | `chezmoi status` (source/target columns) | source dir is usually a git repo → git plugin's job | `exec chezmoi` |
| **symlinks** | core object; plugins may claim (`check_symlink`) | target/dangling checks; target inside a claimed-clean tree | – | host |

### 7.1 git

Managed detection:

- `.git` **directory** in the path → normal repository.
- `.git` **file** containing `gitdir: <path>` → linked worktree or submodule; the real git dir is
  elsewhere. Must be supported; a plain `.git`-is-dir check misses worktrees.
- **Bare** repository (a directory with `HEAD`, `objects/`, `refs/`): contains the backup itself.
  Claim as `clean` with an attr `bare = true` and prune — its contents are backup data, not user
  data to back up.
- **Ancestor repositories:** if the crawl root (or any visited path) is *inside* a repo whose
  root is above the crawl root, the current walker never notices. Fix: at `init`, the plugin
  discovers the repository enclosing the crawl root (`gix::discover`; fallback
  `git -C <root> rev-parse --show-toplevel`); if it succeeds, the enclosing repo is checked once
  and, if clean and prune is acceptable, reported as an ambient claim covering the crawl root.
  (Configuration `ancestor-repos = true`; off if surprising.)

Current detection — reference semantics (shown here as porcelain v2; gix produces the same
information):

```console
$ git -C repo status --porcelain=v2 --branch --ignored=matching
# branch.oid 3f2a...
# branch.head main
# branch.upstream origin/main
# branch.ab +2 -0
1 .M N... 100644 100644 100644 <h> <h> src/app.rs
? new-file.txt
! target/
```

Implementation: the plugin uses **gix** (pure Rust git) to read the repository through its
read-only preopens — status (staged/unstaged/untracked/ignored) and ref comparison without
spawning a process. The output above is the reference semantics gix must reproduce. M1 starts
with a feasibility spike: gix on `wasm32-wasip2` with mmap/file-locking features disabled, plus
behavior and performance on large repositories. Fallback: an `exec = ["git"]` backend parsing
porcelain v2 exactly as shown; the ABI already supports it, only the manifest and internals change.

- dirty: any `1`, `2` or `u` record (modified/staged/renamed/conflicted), and (as today) any `?`
  untracked record. Empty output + zero branch.ab → current.
- propagated: `# branch.ab +A -B`; `A > 0` → `ahead`; `A = 0` → clean.
  `# branch.upstream` missing → configurable `no-upstream = "unknown" | "ahead" | "clean"`
  (default `unknown`, fixing the current false positive where local-only branches are "Unsynced").
  `B > 0` (behind) is not a backup problem; note it as an attr if `--all`.
- ignored: `!` records with `--ignored=matching` (or `git ls-files --others --ignored
  --exclude-standard`). Report as `ignored` status when `report-ignored = true`, otherwise drop.
  This closes today's blind spot where `.gitignore`d user data looks backed up.
- detached HEAD / unborn branch: no upstream → `unknown` (with attr reason); if also dirty → `dirty`
  wins (current state is at least locally captured if committed... a dirty detached HEAD has
  uncommitted content; report `dirty`).
- submodules: porcelain v2 emits `1` records for changed submodules; `git submodule status` is a
  cheaper additional check if needed. Recommendation: MVP treats a changed submodule as dirty;
  optional config `recurse-submodules = false` to skip.

Caching and cost:

- One status computation per repository per scan (gix walk, or the fallback `git status`), cached
  in the instance; the file hook (when enabled) answers from the cache. Never per-file work.
- `GIT_OPTIONAL_LOCKS=0` in the child environment to avoid touching locks during status
  (exec fallback only).
- Repo status is not persisted across scans (cheap to recompute, correctness matters);
  ignored-file collection only runs when `report-ignored = true` (it costs extra stat work).

Edge cases: repository unreadable or unrecognized → `unknown` attr; repository on a slow/network
filesystem → per-repo timeout → `unknown`; the exec fallback is additionally disabled when the
`git` binary is missing.

Capabilities: gix backend: `stat` plus `fs-read = ["/"]` (the plugin may need repository data
above the crawl root for ancestor discovery); the exec fallback adds `exec = ["git"]`.

Config: `backend = "gix" | "exec"` (default `gix`), `no-upstream`, `report-ignored`,
`ignored-mode = "matching" | "all"`, `ancestor-repos`, `recurse-submodules`, `timeout-ms`.

#### M1.7 spike results (target `wasm32-wasip2`, gix 0.89)

The M1.7 feasibility spike lives in `spikes/gix-feasibility/` (a standalone Cargo workspace excluded
from the main workspace). It builds a `cargo-component` 0.21.1 component for `wasm32-wasip2` that
inspects one repository with **gix 0.89.0** (`default-features = false`, features `basic`, `status`,
`sha1`) through a read-only `/` preopen, and runs it on wasmtime 49 with empty env. All 16 git-CLI
fixture tests pass and reproduce the porcelain-v2 semantics of this section (clean, dirty
modified+untracked, ahead, behind, no upstream, detached HEAD, unborn, linked worktree, bare,
ignored `--ignored=matching`, nested repository, ancestor discovery, staged rename, merge conflict,
packed objects/refs, non-repository). APIs used: `gix::discover_opts`, `repo.head`,
`repo.branch_remote_tracking_ref_name`, `repo.merge_base`, `repo.rev_walk(..).with_hidden(..)`,
`repo.index_or_empty`, `repo.status(..)` with `UntrackedFiles::Files` and
`emit_ignored(Matching)`, plus conflict stages read from the index.

**Required shims / adaptations** (all under `spikes/gix-feasibility/`, wired in via
`[patch.crates-io]`):

- **`memmap2` shim** (`vendor/memmap2-shim`): gix 0.89 uses `memmap2` unconditionally (index,
  loose objects, packs, packed refs) and `memmap2` has no wasm backend. Mapping becomes an eager
  read into a `Vec<u8>`, and it must **preserve the file cursor** (real `mmap` never moves it):
  `gix_index::File::at()` maps the index and then hashes it from the same handle. Caveat: memory
  cost ≈ pack/index size (M1's memory cap applies).
- **`filetime` shim** (`vendor/filetime`): upstream's wasm reader accessors are `unimplemented!()`;
  the shim implements them via `Metadata::{modified,accessed,created}`. Write paths stay
  unsupported (the plugin is read-only).
- **`core.fileMode = false`** on open: WASI exposes no Unix executable bit, so otherwise every
  tracked `0755` file is reported as modified. Behavior difference: exec-bit-only changes are not
  detected. The `pacman` plugin has the same gap and closes it with the `stat` host import for
  mode/uid/gid; the git plugin can adopt that later (M5) if it matters.
- gix `parallel`/`interrupt`/`command`/`credentials`/`progress`/`mailmap`/`worktree-stream`/
  `merge`/`blame` stay off; `sha1` is required for racily-clean hashing; zlib-rs enters
  unconditionally through `gix-zlib`.

**Performance** — synthetic 50 000-file / 100-dir repo (~4 MB index, packed objects); native
`git status --porcelain=v2` on the same repo is **31 ms**:

| component | host (runner) | small repo, `run` | 50k repo, `run` |
| --- | --- | --- | --- |
| debug | debug wasmtime | ~20 ms | ~17.0 s |
| release | debug wasmtime | ~12 ms | ~14.5 s |
| release | release wasmtime | ~3–6 ms | ~3.6 s |

Instantiation is < 1.5 ms (release) / ~52 ms (debug). The large-repo cost is dominated by
wasm↔host call overhead in the untracked/ignored dirwalk and per-file stat/hash, not by gix
algorithmics. A **release host** is therefore required, and the M1.4 host must cache the compiled
component (debug artifacts are ~85 MB and take ~90 s to JIT-compile; release is ~3.9 MB).

**Recommendation: `gix`-default for M1.8**, with `backend = "exec"` kept as fallback. gix is
workable on `wasm32-wasip2` and reproduces the reference semantics; every blocker is covered by a
small, reviewable shim. M1.8 should vendor/upstream the `memmap2` cursor fix and the `filetime`
reader shims (the spike's are the reference implementations), open with `core.fileMode=false` and
document the exec-bit limitation, and consider a size/time guard that falls back to `exec` (or
reports `unknown`) for very large repositories until the host call overhead is reduced. The
fallback also covers the exec-bit/symlink fidelity gap and remains selected by config.

### 7.2 pacman

Managed detection (ownership) — native:

- Parse `/var/lib/pacman/local/<pkg>-<ver>/files` (one path per line, directories end with `/`)
  plus `desc` (`%NAME%`, `%VERSION%`, `%BACKUP%`). One read of the database directory in `init`,
  cached in the host cache keyed by its mtime, so parallel workers don't repeat it.
- Paths come from the database verbatim (spaces included), unlike the Python version's
  `split(maxsplit=1)` parsing of `pacman -Ql` output, and no `pacman` binary is needed. The db
  format is stable but not a public API; unrecognized entries → `unknown` with a diagnostic
  instead of wrong results.

Current detection (integrity) — native:

- For every owned file, compare against its `mtree` entry (gzipped BSD mtree): existence, file
  type, size, mtime, mode, uid, gid, symlink target. Metadata that WASI does not expose
  (mode/uid/gid) comes from the `stat` host import. A mismatch → `dirty`, with the mismatch reasons
  in the entry attrs (mirroring pacman's strings: Modification time mismatch, Size mismatch,
  GID/UID/Permissions mismatch, Symlink path mismatch, File type mismatch, ...).
- Files listed in a package's `%BACKUP%` are checked like any other; a differing backup file means
  user/administrator edits → `dirty` is correct: it is not reproducible from the package and needs
  its own backup.
- The default level compares metadata; `check-level = "checksum"` additionally verifies md5/sha256
  (much slower, opt-in).

Fallback (exec, optional): run `pacman -Qkk` and parse lines starting with `warning:` or
`backup file:` of the form `<prefix>: <pkg>: <path> (<reason>)`; unknown reasons are collected and
logged. Selectable via `backend = "pacman"`, e.g. for exact parity on unusual databases.

Semantics: a `clean` package file is recoverable by reinstalling the package — it does not need to
be backed up. `dirty` package files are the interesting ones (edited `/etc` configs, user
modifications), exactly as today.

Caching: file list + comparison results in the host cache keyed by local db mtime; the native
comparison is a stat per owned file, but that still scales with package count, so results are
cached for the scan duration.

Edge cases: database unreadable → plugin disabled with warning; db lock present
(`/var/lib/pacman/db.lck`) → reads still work, and the mtime change when a transaction finishes
invalidates the cache naturally; foreign/AUR packages are in the db like any other; directories
and symlinks are owned too (claim symlinks as well, see 7.5).

Capabilities: `fs-read = ["/"]` (package files live system-wide; authoritative since M2.0) +
`stat`; the exec fallback adds `exec = ["pacman"]`.

Config: `db-path`, `check-level = "metadata" | "checksum"`, `backend = "native" | "pacman"`,
`timeout-ms`.

### 7.3 syncthing

Managed detection:

- **Folder marker:** a `.stfolder` directory in the folder root means "this folder is a syncthing
  folder" (recent versions place `syncthing-folder-<folder-id>.txt` inside it). Missing marker on
  a configured folder = syncthing refuses to sync it → `dirty`/error case, worth reporting.
- **Config:** parse `~/.local/state/syncthing/config.xml` (newer) or `~/.config/syncthing/config.xml`
  (older; also honor `STCONFDIR`): `folder id/path/label/type` entries and the GUI address + API
  key (`<gui><address>`, `<gui><apikey>`). Paths are absolute and usually per-user.
- Detection by marker alone is enough to claim *managed*; config supplies the folder id needed for
  the status API. If the path is not under any configured folder path, no claim.

Current + propagated detection (daemon API):

- `GET /rest/db/status?folder=<id>` with the API key (localhost only). Relevant fields:
  - `state` (`idle`, `scanning`, `syncing`, `error`),
  - `needFiles` / `needTotalItems` / `needBytes` (data needed *from* the cluster → remote changes
    not yet here),
  - `receiveOnlyChanged*` (local changes in a receive-only folder, i.e. diverged),
  - `pullErrors` (failed transfers),
  - `ignorePatterns` (whether `.stignore` is in effect).
- Mapping:
  - `state = idle` and `needFiles = 0` and `pullErrors = 0` → `clean` (local == cluster).
  - local changes pending to be sent (not directly shown by this endpoint: a send-only folder or
    any local modification shows as `state = syncing`/`scanning` or via `receiveOnlyChanged` when
    receive-only) → `ahead` for send-only semantics, `dirty` for receive-only divergence
    (configurable: `receive-only-changes = "dirty" | "ahead" | "clean"`).
  - `state = error` or `pullErrors > 0` or `invalid` folder (marker missing) → `dirty`.
- Daemon not reachable / no API key → `unknown` with attr `reason = "syncthing daemon
  unreachable"` *unless* `assume-marker-only = true`, in which case marker ⇒ `clean` with a
  visible note. Never claim `clean` silently when the daemon is down.
- The API docs explicitly warn that `/rest/db/status` is expensive (CPU/RAM on the device), and
  syncthing itself caches folder state. Therefore: one call per configured folder per scan, cached
  with a short TTL, and claims are made at **folder root level** (`prune = true`). Per-file checks
  via `/rest/db/file` exist but are far too chatty; if per-file granularity is ever wanted it must
  be opt-in.
- `.stignore`: patterns listed in folder settings / the `.stignore` file cause files to be skipped
  by syncthing → `ignored` status if `report-ignored = true`. Note that `ignorePatterns` in the
  status response tells whether ignores are active.

Semantics: syncthing is replication, not archival backup — repeat that in output
(`attr: "synced to cluster, not archived"`), so nobody mistakes a two-node mirror for a backup.

Capabilities: `fs-read = ["~/.local/state/syncthing", "~/.config/syncthing"]` (authoritative
since M2.0; the crawl root is always granted); `http = ["http://127.0.0.1:<port>"]` only.

Config: `address` (override), `api-key` (override; file otherwise), `require-daemon` (default
true), `assume-marker-only`, `receive-only-changes`, `timeout-ms`.

### 7.4 chezmoi

Managed detection:

- `chezmoi managed --format json --path-style absolute` (optionally
  `--include=files,dirs,symlinks`) → the managed entry list. `--format json` and `--path-style`
  are supported by the `managed` command. One exec in `init`, cached.
- Source and destination directories come from `~/.config/chezmoi/chezmoi.toml`
  (`sourceDir`, `destDir`), environment overrides, or defaults (`~/.local/share/chezmoi`,
  `$HOME`). `chezmoi data`/`dump-config` can be used as fallback; parsing the config file directly
  is cheaper.
- If the binary is missing, an optional degraded mode can build the managed list natively from
  the source tree by decoding chezmoi source-state filename attributes (`dot_`, `private_`,
  `executable_`, `symlink_`, `encrypted_`, `run_`, templates). Recommendation: phase 2; templates
  and externals make a fully faithful native implementation expensive.

Current detection:

- `chezmoi status --path-style absolute` → status codes per path (similar to git: first column
  source state, second column target state; `M`, `A`, `D`, `R`, etc.). Cache per scan.
- Mapping proposal:
  - both columns clean → `clean`;
  - target modified relative to source (destination column changed, e.g. ` M`, `MM`) → user data
    in the destination is not captured in the dotfiles repo → `dirty` (needs re-add/commit);
  - source modified, not yet applied (source column changed, e.g. `M `) → the dotfiles state is
    not on disk yet → `ahead` (state captured in source, not applied);
  - `chezmoi` execution failure / template error → `unknown` with attr.
- Propagation: chezmoi has no remote; whether the source directory is pushed is the git plugin's
  job (the source dir is usually a git repo). If both plugins run, the file under `$HOME` is
  claimed `clean` by chezmoi, and the source repo is reported in the git panel — document this
  composition so users read it correctly.

Scope and edge cases:

- chezmoi is inherently per-user. When crawling `/`, only the *current user's* chezmoi state can
  be checked; other users' dotfiles are invisible (config `users = [...]` would require running
  the tool as them — out of scope, note as limitation).
- `$HOME` itself: managed files under `$HOME` are fine; chezmoi also manages entries in `/etc` via
  `sudo` in some setups — not visible to a user-level scan.
- Scripts (`run_` entries) are stateful side effects with no target file; ignore them (no claim).
- `chezmoi unmanaged` lists unmanaged files inside the destination tree; optional
  `report-unmanaged-in-source` mode. MVP: skip.

chezmoi is the one bundled detector that keeps `exec`: a faithful native implementation would have
to reimplement source-state filename encoding and template evaluation, while `chezmoi managed`
and `chezmoi status` already answer the three questions. The grant is a single allowlisted program,
shown in the trust prompt. A native source-tree parser stays the M5 stretch goal.

Capabilities: `exec = ["chezmoi"]`, `fs-read = ["~"]` (authoritative since M2.0: chezmoi reads
the destination tree under `$HOME`, and the crawl root is always granted; source-dir/dest-dir
overrides outside `$HOME` need their own spec).

Config: `source-dir`, `dest-dir`, `include-types`, `report-unmanaged-in-source`, `timeout-ms`.

### 7.5 symlinks (core feature, not a plugin)

Why the core must own this:

- A symlink is not a file: a backup tool may store the link itself (git, tar, restic), follow it,
  or ignore it. "Is this symlink backed up?" and "what is at the other end?" are different
  questions; the current code answers neither (it silently drops links).
- Following links naively causes cycles and double-reporting (dotfile farms: `~/.config/foo` →
  `~/dotfiles/foo`, where both paths exist in any traversal).

Design:

- The walker never follows symlinks by default; a symlink is collected as
  `LinkInfo { path, target, dangling }`.
- Each plugin that hooks `symlink` may claim the link:
  - git: a link committed in a repo is part of the repo state; with prune-always semantics this is
    covered by the repo-level claim; if per-file claims are used, `check_symlink` returns the same
    status as the repo.
  - pacman: package-owned symlinks appear in `-Ql`; claim them like files (clean/dirty by `-Qkk`).
  - chezmoi: `symlink_` source entries → managed; claim according to `chezmoi status`.
  - stow/dotbot/rcm-style farms: a future plugin or `--follow-symlinks`.
- Unclaimed links become `SymlinkEntry` and are shown in their own console panel
  ("Symlinks") and JSON key (`symlinks`), never dropped:
  - field `target`: resolved target path (absolute, lexically normalized);
  - field `dangling = true` if the target does not exist (including broken chains);
  - field `via = Some(<target>)` when the **target-in-managed-tree rule** applies: if the resolved
    target lies inside a subtree that was claimed `clean` by a plugin **with `prune = true`** (or is
    itself a claimed-clean path), the link is reported clean via that target. This is the common,
    correct case for dotfile setups where the real files live in a synced/git-managed tree. A
    non-pruning directory claim is deliberately not extended to its subtree.
  - otherwise the link is loose/unmanaged (it is a pointer plus, potentially, unreferenced data
    elsewhere).
- `--follow-symlinks` (off by default): follow non-dangling links during traversal with a visited
  `(dev, inode)` set to prevent cycles and duplicates; entries reached via a link get an attr
  `via-link = <link>` and the link itself is reported as its own entry. Recommendation: implement
  in phase 3; the default mode already surfaces all links, which is the safety-relevant part.
- Hardlinks are not symlinks but share content: out of scope for now, note as a future attr
  (link count > 1) so users understand that deleting one name doesn't delete data.

### 7.6 Candidate future plugins (for context, not required now)

| Plugin | Managed detection | State detection |
| --- | --- | --- |
| restic / borg / kopia | configured repo list; file→repo mapping requires repo query | `restic snapshots`/`borg list`; expensive, needs config |
| Dropbox / Nextcloud / OneDrive | marker files (`.dropbox` in root, `~/.dropbox`), Nextcloud `.sync_*.db` | vendor CLI/API if available; otherwise managed-only claim |
| Steam / Flatpak / distro packages | manifest files (`appmanifest_*.acf`), `/var/lib/flatpak` | inherently recoverable by re-download |
| Nix store | path hash in `/nix/store/<hash>-...` | content-addressed → `recoverable` |

Steam/Flatpak/Nix motivate a future `recoverable` status: "not backed up, but reproducible
without a backup." It is distinct from `clean` (verified copy exists) and would keep huge
re-downloadable trees from drowning the report: a muted panel, never counted as unbacked.
**Decision:** not part of v1 — no bundled detector can emit it; it is added together with the
first plugin that needs it (Steam), which is an ABI change we can still make cheaply while no
third-party plugins exist. See open question 4.

## 8. Statuses, aggregation and output

- Statuses: `clean`, `dirty`, `ahead`, `unknown`, plus optional `ignored` (section 3;
  `recoverable` arrives with the first plugin that needs it). Console panels label `ahead` as
  "Unsynced".
- Panels: per-plugin, split into Dirty / Unsynced / Unknown / Clean (`Clean` only with `--all`),
  plus the "Not backed up" / "Permission denied" panels and the "Symlinks" panel. The symlink
  panel always shows unmanaged links as `[Unmanaged] path -> target` (with a `(dangling)` marker)
  and shows via-clean links as `[Clean]` only with `--all`.
- Console rendering: the `console` crate for styling/TTY detection and `indicatif` for progress
  (same ecosystem, closest analog to rich); panels are hand-rolled box drawing. No TUI
  (`ratatui`) for now — it is the right tool only for an interactive result browser.
- JSON: a versioned schema (`"schema": 1`), with per-plugin lowercase status keys and attrs. The
  `symlinks` array carries `path`, `target`, `dangling` and `via` (omitted unless set), sorted by
  path. No Python-compatibility mode (decision in section 14).
- Exit codes: v1 stays conventional — `0` on success, `2` for usage/config errors; scan results
  (unbacked data, plugin warnings) do not change the exit status. A `--fail-on unbacked|error`
  mode is deliberately deferred until scripts need it: defining "unbacked" (loose/dirty/ahead vs
  unknown/ignored) only becomes meaningful once the detectors exist.

## 9. Performance and parallelism

- Walker: one detector set **per worker** (`Vec<Vec<DetectorSlot>>`); directories
  are processed from a shared FIFO work queue by `std::thread` workers, so no new
  dependencies. Every directory is popped by exactly one worker, which keeps
  first-claim-wins per path unchanged. Per-directory results are collected in a
  map; after the workers join, the tree is rebuilt and loose-path collapsing is
  computed bottom-up exactly like the old recursion, and claims, denied paths and
  symlinks are merged and sorted deterministically. Scheduling never influences
  output: `--jobs 1` and `--jobs N` are byte-identical (M4a).
- Detector instances: one per worker thread (wasmtime `Store` is not `Sync`), each with its own
  mutable state. Expensive global preparations must go through the host cache so the second
  worker's `init` reads cache instead of re-running `pacman -Qkk`/`chezmoi managed`. The
  workers' `init` calls are serialized by a single mutex; the first warms the cache and the
  rest hit it (`--jobs` default 1, `0` = available parallelism). No cache-lock ABI is needed.
- Bulk `check-files` per directory reduces boundary crossings; wasmtime component calls are
  microseconds, so this is an optimization, not a necessity.
- Cache: per-plugin persistent KV under `$XDG_CACHE_HOME/backupcrawl/plugins/<name>/`, with
  invalidation keys driven by the plugin (e.g. pacman db mtime, chezmoi source mtime, syncthing
  config mtime). `backupcrawl cache clear` for manual control.
- Limits: wasmtime fuel/epoch interruption (cooperative cancel every ~50 ms), linear memory
  limit (e.g. 256 MiB), exec timeout + output caps (e.g. 64 MiB), HTTP timeout + size cap.
- Compiled components are cached in two layers: in-memory (keyed by the wasm bytes, one
  compilation per detector per process) and on disk via wasmtime's component cache
  (`$XDG_CACHE_HOME/backupcrawl/plugins/_components`). The bundled git component takes ~10 s
  to JIT-compile in a debug build, so the disk cache is what keeps repeat runs fast; a warm
  `backupcrawl` run drops from ~10 s to a fraction of a second, and `cargo test --workspace`
  from ~2.5 min to seconds.
- `--stats`: per-plugin wall time, exec count, cache hits, errors — makes it visible which
  detector is slow (pacman `-Qkk` will usually be the culprit).
- Progress display: port `PathTracker` semantics to `indicatif` + `console` (open/close counters,
  stragglers). Implemented in M4a as `--progress`: the CLI renders on **stderr**, throttled and
  disabled when stderr is not a TTY; core publishes a plain `ProgressSnapshot` through a
  `ProgressSink` trait (no terminal types in core), so JSON/console stdout is never touched.

**Per-worker contract (frozen in M2.0, implemented by M4a).** One detector instance per
worker. `init`/`finish` bracket that instance's lifetime, so a scan with N workers calls each
plugin's `init`/`finish` N times. Instances share no mutable state; global preparation belongs
in the host cache, and cache-write races between workers must be benign. The first worker's
`init` is serialized and warms the cache; the remaining workers' `init` calls then hit it (the
M4a single-flight guard). There are no cross-worker ordering guarantees: first-claim-wins is
still evaluated per path, and every path is handled by exactly one worker. Output stays
deterministic — per-worker claims are merged and sorted in the host after the walk, and
loose-path collapsing happens bottom-up after traversal — so `--jobs 1` and `--jobs N` produce
byte-identical output. Plugins must not assume one instance per process, process-global mutable
state, or hook order across unrelated paths.

Rollout: single-threaded crawler with per-plugin instances first (M1–M2, `--jobs 1`), then
parallelism (M4a) once the grant/permission model is settled and pacman gives a real workload.

## 10. Security and trust model

Threat model: the user may install a third-party detector from the internet; a detector may be
buggy or malicious. The host protects the machine.

- **No ambient authority.** Detectors get only what the host grants: read-only filesystem
  visibility scoped to the crawl root plus the manifest's resolved `fs-read` roots (M2.0),
  optionally `exec`/`http` on top; no writes, no sockets beyond the `http` allowlist.
- **Files are read-only** through WASI preopens enforced by wasmtime/`cap-std` (no writes; there
  is nothing to escape to). The resolved `fs-read` roots are printed in `plugins info` and the
  trust prompt, and they are the actual content scope; a plugin that declares `/` is shown as
  `read /`. The metadata `stat` import remains unrestricted and metadata-only.
- **exec is the escape hatch.** A spawned `chezmoi` (or any allowlisted program a third-party
  plugin declares) runs with the user's rights and is *not* sandboxed. This is why the bundled git
  and pacman detectors avoid exec and why the capability must be loud: `backupcrawl plugins info
  <name>` prints granted programs, and the trust prompt lists `exec` grants first.
- **http** is allowlisted by scheme/host/port/path-prefix; default localhost-only; responses are
  size-capped. No redirects to non-allowlisted hosts.
- **Trust on first use:** manifests are hashed (sha256). Unknown third-party plugin → interactive
  prompt (prints capabilities + hash) or `--trust-plugin=NAME` for non-interactive use; the hash
  is written to `~/.config/backupcrawl/config.toml` (`[plugins.trust]`, plugin name → hash). Hash
  change → prompt again. **Bundled plugins are auto-trusted**: they ship inside the binary and are
  covered by its integrity, so requiring a prompt would only train users to click through it; they
  are still hashed for display. The prompt is only shown on a terminal; piped input declines
  rather than blocking, so a run without `--trust-plugin` simply reports the plugin as untrusted.
  Optional minisign/sigstore verification later.
- **Resource limits** protect against accidental infinite loops and memory bombs (section 9).
- **Known residual risk:** a malicious detector can exfiltrate any file it is allowed to read
  through the `http` grant (e.g. syncthing's localhost API) or by writing it into an `exec` child's
  argv/env. Capability review is the defense; document it.

## 11. Config, CLI and distribution

Config (TOML, `~/.config/backupcrawl/config.toml`; TOML only, no legacy JSON rcfile):

```toml
[ignore]
paths = ["/proc", "/sys", "/dev", "/run", "**/node_modules"]

[plugins]
enabled = ["git", "pacman", "chezmoi", "syncthing"]  # empty = all discovered
order   = ["git", "pacman", "chezmoi", "syncthing"]

[plugins.trust]
git    = "sha256:..."
pacman = "sha256:..."

[plugins.git]
no-upstream   = "unknown"     # unknown | ahead | clean
report-ignored = false
ancestor-repos = true

[plugins.pacman]
db-path = "/var/lib/pacman"
check-level = "metadata"      # metadata | checksum

[plugins.chezmoi]
source-dir = ""               # auto-detect
```

CLI:

```
backupcrawl [PATH] [--format json|console] [--all] [--ignored]
            [--progress] [--jobs N] [--ignore GLOB]... [--plugin-dir DIR]
            [--plugins a,b,c] [--no-plugins] [--stats] [--config FILE]
backupcrawl plugins list|info|check|install|verify
backupcrawl plugin new <name>          # scaffolding
backupcrawl cache clear|path
```

Distribution: release archives contain the binary plus `plugins/*.wasm` + `plugin.toml`; the four
default detectors can additionally be embedded via `include_bytes!` so `cargo install backupcrawl`
works standalone (external same-name plugins override embedded ones). Detector crates live in the
same workspace (`plugins/git`, `plugins/pacman`, ...) and are built to `wasm32-wasip2` by CI. A
plugin tarball is a plain `.tar.zst` with manifest + wasm; `plugins install` copies it after the
trust prompt.

## 12. Milestones

Phase overview; each phase gets a task list with acceptance criteria when work on it starts
(M0/M1 are implemented and broken down below). The Python implementation is already preserved
by tag `v0.2.6` and was removed in M0.

**Ordering decision (2026-10-17).** The original M2→M3→M4→M5 order was revised: the pieces of
M5 that define the plugin contract are pulled forward before more detectors are written, while
distribution work and evidence-driven hardening stay later. The active sequence is:

1. **M2.0** — make `fs-read` authoritative and freeze the per-worker lifecycle/cache rules.
2. **M2** — pacman plugin + core symlink handling, written under the final grant model.
3. **M4a** — parallel crawl, per-worker instances, init single-flight, `--jobs` and `--progress`,
   validated against pacman.
4. **M3** — chezmoi + syncthing, written against the final concurrency and permission model.

M4b (distribution, `plugin new`, third-party example) and M5 (signing, backend hardening,
candidate plugins) are deliberately **deferred** until after this sequence; they are not
prerequisites for more first-party detectors.

| Phase | Theme | Deliverable |
| --- | --- | --- |
| M0 | Core without plugins | Rust workspace replaces the Python package; walker + result model + console/JSON printers; everything loose; CLI/config; fixture-based tests |
| M1 | Host + git | wasmtime host MVP: manifest, discovery, read-only `/` preopen, trust prompt, `init/check-dir/check-file/finish`, host imports stat/exec/cache/http/log, limits; **gix feasibility spike**, then the **git plugin** on gix (exec fallback behind `backend`), fixing the upstream bug; `plugins list/info` |
| M2.0 | Contract hardening | `fs-read` becomes authoritative (resolved read-only preopens; crawl root always granted); per-worker lifecycle and cache rules frozen and documented; `plugins info`/trust prompt show the real scope |
| M2 | pacman + symlinks | **pacman plugin** native (`files`/`desc`/`mtree` + `stat`, unknown-format handling); symlink basic handling (own panel/JSON, no following, target-in-managed-tree rule); golden-output tests |
| M4a | Parallel crawl + progress | Parallel crawl with per-worker detector instances and deterministic merges; serialized `init` as the single-flight cache guard; `--jobs N` (default 1) and `--progress`; validated against pacman |
| M3 | Dotfiles + sync | **chezmoi plugin**; **syncthing plugin**; `unknown`/`ignored` statuses wired through printers; `--ignored` |
| M4b | Distribution + SDK (deferred) | Trust/config polish; `plugins install`; `plugin new` + SDK docs; first third-party plugin example |
| M5 | Hardening (deferred) | Plugin signing; harden native backends (gix edge cases, checksum level, drop the exec fallback if unused); optional native chezmoi source-tree parser; candidate plugins (Steam/recoverable status, restic/borg, cloud markers); `--stats` |

### M0 task breakdown

Acceptance for every task: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and
`cargo test` green.

- **M0.1 Workspace + Python removal.** Cargo workspace at the repo root with
  `crates/backupcrawl-core` (library) and `crates/backupcrawl-cli` (binary `backupcrawl`); delete
  `backupcrawl/`, `pyproject.toml`, `poetry.lock`, `pylintrc` and the Python editor config;
  rewrite README for building/running the Rust binary; keep LICENSE. *Done when:* the binary
  builds and `backupcrawl --help` works.
- **M0.2 Result model + walker.** Status enum (`clean/dirty/ahead/unknown/ignored`), entry and
  result types from section 6, `globset`-based ignore patterns with `~` expansion, depth-first
  walker reproducing Python semantics: unreadable paths → `denied`, sockets/FIFOs/devices
  ignored, loose-path collapsing; the per-directory merge logic already models claims (always
  empty in M0), so detectors slot in without reshaping the walker; symlinks are skipped (M2
  reports them). *Done when:* unit tests cover traversal, ignore matching, denied paths and
  collapsing on fixture trees.
- **M0.3 Printers.** Console panels via `console` (loose/denied only for now; per-plugin panels
  appear in M1) and JSON with `"schema": 1`; deterministic sorting. *Done when:* golden tests
  cover both formats on a fixture tree.
- **M0.4 CLI + config.** `clap` CLI (`PATH` default `/`, `--format`, `--all`, `--ignore`,
  `--config`, `-v`), TOML config at `~/.config/backupcrawl/config.toml` (`[ignore] paths`), CLI
  patterns append to config; missing config is fine, unreadable/invalid config exits 2. *Done
  when:* `assert_cmd` integration tests cover defaults, config+flag merge and invalid config.
- **M0.5 Wrap-up.** README and `--help` text final; manual smoke run on a scratch tree; commits
  reviewable per task.

*Implemented on this branch (`5f118b1`): workspace, walker, printers, CLI/config, 16 tests;
`cargo fmt`, clippy `-D warnings` and `cargo test` green.*

### M1 task breakdown

M1 is the host and the first detector. wasmtime, `wit-bindgen`/`cargo-component` and the
`wasm32-wasip2` target are pinned in M1.1; the gix spike (M1.7) decides whether the git plugin
ships on gix by default in this milestone or only behind `backend = "exec"`.

- **M1.1 CI baseline + WIT contract.** GitHub Actions workflow running `cargo fmt --check`,
  `cargo clippy --all-targets -- -D warnings`, `cargo test`. Add `rust-toolchain.toml` (stable +
  `wasm32-wasip2` target) and a `crates/backupcrawl-wit` crate containing the finalized
  `backupcrawl:detector@1.0.0` package: types (`status` without `recoverable`, `attr`, `claim`,
  `scan-info`, `link-info`), the `detector` interface (`init`, `check-dir`, `check-file`,
  `check-symlink`, `check-files`, `finish`) and the `host` interface (`stat`, `exec`,
  `cache-get`/`cache-set`, `http`, `log`). Host bindings are generated with
  `wasmtime::component::bindgen!`. Decisions: `check-files` stays in `@1` and must behave exactly
  like singular calls (the SDK provides a default implementation that loops the singular calls);
  the host keeps using singular calls until the M4 bulk work. Test components are prebuilt
  `.wasm` files checked in under `tests/fixtures/` next to their source plus a rebuild script;
  CI fails if an artifact is stale.
  *Done when:* the WIT package builds, generated host bindings compile, and a fixture component
  instantiates in a test.

  *Implemented (M1.1, branch `feat/m1.1-foundation`):* `crates/backupcrawl-wit` is the single
  source of truth (`crates/backupcrawl-wit/wit/detector.wit`, world `plugin`); plugin crates
  symlink `wit` to it and a wit-parser round-trip test keeps the package honest. The host uses
  `wasmtime::component::bindgen!` from wasmtime 49 (wit-parser 0.258). `rust-toolchain.toml` pins
  stable + rustfmt/clippy and the `wasm32-wasip1`/`wasm32-wasip2` targets; cargo-component 0.21.1
  builds `wasm32-wasip1` and adapts it to a component with the preview1 adapter
  (`<target-dir>/wasm32-wasip1/{debug,release}/backupcrawl_test_components.wasm`). The fixture
  `crates/test-components` uses raw `wit_bindgen::generate!` 0.41 (cargo-component's generated
  `src/bindings.rs` is gitignored) and is rebuilt by `scripts/build-test-components.sh` into the
  checked-in `crates/backupcrawl-host/tests/fixtures/test-detector.wasm`; CI fails when that
  artifact is stale. `crates/backupcrawl-host/tests/fixture_smoke.rs` instantiates the fixture
  with wasmtime-wasi (empty env, no preopens) and asserts its documented behavior. Workspace
  `rust-version` is 1.96 (wasmtime 49 MSRV). Caveat: the committed artifact depends on the exact
  toolchain, so after a stable bump re-run the rebuild script and commit the refreshed artifact.
- **M1.2 Plugin SDK (`backupcrawl-plugin-sdk`).** Detector authoring crate: a native
  `Detector`-style trait plus a `Host` trait (stat/exec/cache/http/log) so detector logic is
  unit-testable with mocks under plain `cargo test` (no wasm toolchain needed); a
  `bind_detector!` macro emits the component exports, including the `check-files` default that
  loops the singular calls; `scan-info`/plugin-config plumbing.
  *Done when:* an example plugin's logic passes native unit tests with a mock host, the same crate
  builds with `cargo component build` to `wasm32-wasip2`, and the artifact loads in the host.
- **M1.3 Manifest, discovery and `plugins list|info`.** `plugin.toml` parsing (name, version,
  api, description, license, hooks + priority, capabilities, optional config schema); discovery
  in order `--plugin-dir`, `$XDG_CONFIG_HOME/backupcrawl/plugins/`,
  `/usr/lib/backupcrawl/plugins/`, bundled (embedded registry via `include_bytes!`, initially
  empty), with user > system > bundled precedence; api compatibility and required-capability
  validation (unknown manifest keys warn, unknown required capabilities are load errors);
  `plugins list` and `plugins info <name>` print resolved capabilities, including
  `fs: read /`.
  *Done when:* fixture-based tests cover precedence, api mismatch, unknown keys and capability
  errors; `plugins list|info` output is golden-tested.
- **M1.4 wasmtime host runtime + imports + limits.** Instantiation per detector with a read-only
  `/` preopen and an empty environment; memory cap (256 MiB default) and fuel/epoch interruption
  (cooperative cancel every ~50 ms); host imports: `stat` (mode/uid/gid/nlink), `exec`
  (program allowlist, timeout, output caps), `cache` (persistent per-plugin KV under
  `$XDG_CACHE_HOME/backupcrawl/plugins/<name>/`), `http` (scheme/host/port/path-prefix
  allowlist, size caps, no redirects to non-allowlisted hosts) and `log`; lifecycle sequencing
  init → checks → finish with per-plugin error accounting — call errors become "no claim", a
  circuit breaker after N consecutive failures disables the plugin for the scan, `finish`
  errors and disable reasons end up in the run report.
  *Done when:* host integration tests with fixture components cover successful
  init/check/finish, disabled-on-init-error, trap handling, fuel exhaustion and each host import
  (including exec timeout and cache round-trip).
- **M1.5 Trust flow.** sha256 manifest hashing; unknown hash → interactive prompt that prints the
  capabilities (exec grants first, `fs: read /` plainly) or `--trust-plugin=NAME` for
  non-interactive use; hash change re-prompts; the trust list persists in
  `~/.config/backupcrawl/config.toml` (`[plugins.trust]`). Decision to document: bundled plugins
  are auto-trusted (they ship with the binary and are covered by its integrity) while
  third-party plugins always prompt until trusted; §10 is updated accordingly.
  *Done when:* tests cover the unknown-hash path (non-interactive via flag), hash-change
  re-prompt and trust-list round-trip; bundled auto-trust is documented.
  *Implemented (M1.5, branch `feat/m1.5-trust-flow`):* `backupcrawl-host::trust` hashes the raw
  `plugin.toml` bytes (sha256, canonical `sha256:<hex>`), evaluates `Bundled`/`Trusted`/`Unknown`/
  `Changed` against `[plugins.trust]`, renders the prompt (exec first, `fs: read /`, http/cache,
  hash) and persists via `toml_edit` so unrelated tables and comments survive.
  `plugins list` shows the computed trust column, `plugins info` the state and hash, and
  `plugins verify` is an offline read-only check that exits 2 when a third-party plugin is
  untrusted. `--trust-plugin=NAME` is repeatable; crawl-level plumbing is M1.6.
- **M1.6 Detector façade + walker integration.** `Detector` trait in core (`init`, `check_dir`,
  `check_file`, `check_symlink`, `finish`) with `WasmDetector` in the host and a native
  `FixtureDetector` for core tests; the walker asks detectors in priority order, the first
  `Some(claim)` wins, `prune = true` stops the descent while `prune = false` keeps it, file
  hooks are served from detector state; per-plugin panels and JSON entries wired through the
  printers plus the `plugins: git ok, ...` run report line; CLI gains `--plugins a,b,c`,
  `--no-plugins` and repeatable `--plugin-dir`. The trait carries `check_symlink` already, but
  the walker keeps skipping symlinks until M2.
  *Done when:* core tests with fixture detectors cover first-claim-wins, prune, claim-then-descend
  and loose-path interaction; CLI golden tests show per-plugin panels and JSON entries.

  *Implemented (M1.6, branch `feat/m1.6-walker-integration`):* the shared records
  (`ScanInfo`, `Claim`/`Attr`, `LinkInfo`, `DetectorError`, `DetectorReport`, `Hooks`) live in
  `backupcrawl-core::detector` and the host re-exports them, so `WasmDetector` implements the core
  `Detector` trait without a parallel type set (the WIT `claim.attrs` list is folded into
  `Entry.attrs` for deterministic JSON). The walker asks detectors in caller order — manifest
  `hooks.priority`, overridden by `[plugins] order` — first `Some(claim)` wins, `prune = true`
  records the claim and stops the descent while `prune = false` records it and continues, and
  `init`/`finish` bracket the scan; loose collapsing is unchanged. The trait gains an optional
  `report()` accessor so the walker can build `PluginReport`s from the host's circuit-breaker
  accounting. The CLI builds detectors from discovery + trust + config + `Runtime`, adds
  `--plugins`/`--no-plugins`/`--plugin-dir`/`--trust-plugin`, renders per-plugin panels plus the
  `plugins: git ok, ...` run report line, and keeps `plugins list|info|verify`. Symlinks stay
  skipped until M2. CLI golden tests use the SDK example component as a committed fixture
  (`crates/backupcrawl-cli/tests/fixtures/detectors/dotbak`, rebuilt with
  `scripts/build-cli-fixture-plugin.sh`).
- **M1.7 gix feasibility spike (timeboxed, ~2 days).** Build a minimal component that uses `gix`
  on `wasm32-wasip2` with mmap/file-locking features disabled; run status and upstream comparison
  on real repositories, including a large one, and measure wall time; record which gix features
  and crates were needed and any blockers in §7.1 of this plan.
  *Done when:* the spike either proves gix workable with a documented feature set, or identifies
  concrete blockers that make `backend = "exec"` the M1 default (recommendation recorded in §7.1).
- **M1.8 git plugin on gix.** Managed detection (`.git` directory, `.git` file
  worktree/submodule, bare repo → `clean` with `bare = true` + prune, ancestor repos via
  `gix::discover` when `ancestor-repos`); status with porcelain-v2 semantics (dirty =
  modified/staged/renamed/conflicted/untracked); propagated via `branch.ab` with
  `no-upstream = unknown|ahead|clean` (default `unknown`) and behind noted as attr; ignored files
  when `report-ignored`; detached HEAD/unborn → `unknown`, dirty wins; unreadable repo → `unknown`
  with attr; one status computation per repository cached in the instance and served to the file
  hook; config keys of §7.1. The embedded registry from M1.3 is filled with this plugin.
  *Done when:* golden tests against fixture repos (created with the git CLI in tests) cover
  clean/dirty/ahead/no-upstream/ignored/worktree/bare/nested/ancestor cases and reproduce the
  documented porcelain-v2 reference semantics.

  *Implemented (M1.8, branch `feat/m1.8-git-plugin`):* `plugins/git` now implements the detector
  on the `gix` backend via the SDK (`bind_detector!` + the native `Detector`/`Host` traits).
  `backend.rs` is the spike's inspection code extended to collect the dirty/ignored *paths* (not
  just counts) so the file hook is served from a per-repository cache; `config.rs` validates
  `backend`/`no-upstream`/`report-ignored`/`ignored-mode`/`ancestor-repos`/`recurse-submodules`/
  `timeout-ms` (`backend = "exec"` returns a clear "not implemented yet (M1.9)" `init` error).
  Managed detection covers `.git` directories, `.git` files (linked worktree/submodule), bare
  repositories (`clean` + `bare = true` + prune) and ancestor repositories (`gix::discover`,
  lazily on the crawl root). Status is porcelain-v2: dirty wins, `ahead > 0` is `ahead`,
  otherwise `clean` with `behind` as an attr, no upstream follows `no-upstream` (default
  `unknown`), detached/unborn are `unknown` with a `reason`, and a changed submodule is `dirty`.
  The two M1.7 shims are vendored at the workspace root (`vendor/{memmap2-shim,filetime}` with
  `[patch.crates-io]`, `exclude = ["spikes", "vendor"]`); repositories open with
  `core.fileMode = false`. The component is built by `scripts/build-git-plugin.sh` into
  `plugins/git/dist/{git.wasm,plugin.toml}` and embedded in `bundled::REGISTRY`.
  **Decision recorded here:** a repository is claimed as one entry and pruned by default (the
  Python behavior and the cheap case); with `report-ignored = true` the claim does not prune,
  `.git` is claimed and pruned as metadata, and the file hook reports ignored paths and the
  per-file status from the cache. This reconciles §6's "prune = false + file hook served from
  its per-repo cache" with the default single-entry behavior. `timeout-ms` is parsed but not
  enforced by the gix backend (the `interrupt` feature is off; host fuel/epoch limits are the
  protection) and is reserved for the M1.9 exec backend. Golden tests: 23 cases in
  `crates/backupcrawl-host/tests/git_plugin.rs`, driving the built component through
  `Runtime`/`WasmDetector` against git-CLI fixtures (clean/dirty/ahead/behind/no-upstream/
  detached/unborn/worktree/bare/ignored matching+all/nested/ancestor/staged-rename/conflict/
  packed/submodule/non-repo), plus 12 native unit tests for config and decision logic.
  `cargo fmt --check`, clippy `-D warnings` and `cargo test --workspace` are green.
- **M1.9 git exec fallback (behind `backend = "exec"`).** Parse `git status --porcelain=v2
  --branch --ignored=matching` and `for-each-ref` exactly as specified in §7.1, run with
  `GIT_OPTIONAL_LOCKS=0`, disabled when the `git` binary is missing; outputs match the gix
  backend on the same fixtures.
  *Done when:* parity tests pass for both backends (skipped when `git` is unavailable).

  *Implemented (M1.9, branch `feat/m1.9-git-exec-fallback`):* the backend-neutral records
  and verdict logic moved to `plugins/git/src/model.rs`; the gix code stays in `backend.rs`
  and the new `exec.rs` parses porcelain v2 into the same `RepoInspection`, so the two
  backends share one decision path. The status command is
  `git -c core.quotePath=false status --porcelain=v2 --branch --untracked-files=all
  [--ignored=matching]` with an otherwise empty environment plus `GIT_OPTIONAL_LOCKS=0` and
  `GIT_CONFIG_NOSYSTEM=1`. Three choices make it match gix: `-uall` (gix's
  `UntrackedFiles::Files` lists individual files, the CLI default collapses an untracked
  directory), `--ignored=matching` (the same ignored set gix emits; `ignored-mode = all`
  still uses the host-side parent rule) and C-style path unquoting. Bare repositories read
  HEAD with `git symbolic-ref` + `git rev-parse --verify` (where `status` cannot run) so the
  attributes match gix exactly; `timeout-ms` is now honored by this backend. `init` probes
  `git --version` and disables the plugin for the scan when the program is missing. The
  manifest declares `exec = ["git"]` (bundled plugins are auto-trusted, and nothing runs
  unless `backend = "exec"` is selected). Parity tests live in
  `crates/backupcrawl-host/tests/git_plugin_exec.rs` (13 cases, each observing a shared
  fixture with both backends and asserting identical directory claims/attributes/prune and
  file statuses); the git fixture helpers moved to `tests/support/mod.rs`. CI rebuilds
  `plugins/git/dist/git.wasm` and fails on a stale artifact.

- **M1.10 Wrap-up.** README plugin section and a short plugin-authoring doc (build with
  `cargo component build`, manifest fields, capabilities, testing with the SDK mock); manual
  smoke run of `backupcrawl ~` on a real home directory; plan doc updated with the spike results
  and any ABI learnings.
  *Done when:* docs updated, `cargo fmt`/clippy/`cargo test` green, and the git panel shows up in
  a real scan.

  *Implemented (M1.10, branch `feat/m1.10-wrap-up`):* the stale README intro ("no detectors are
  wired yet") is replaced with the plugin architecture and the git detector, and it gains a
  Plugins section (discovery order, `plugins list|info|verify`, trust), the `[plugins]` config
  shape and the updated exit-code note. New `docs/plugin-authoring.md` covers the three-question
  model, crate setup (cdylib + `wit` symlink + `[package.metadata.component]`), the `Detector`
  trait and `Claim` builders, the `Host` services and the capability that gates each, native
  testing with `MockHost`, the full `plugin.toml` manifest with its validation rules, the build
  invocation, and the v1 limitations (`fs-read` informational, `check_symlink`/`check-files`
  not yet called by the walker). **ABI learning:** cargo-component 0.21.1 accepts both targets —
  the default is `wasm32-wasip1` (preview1 adapter), and `--target wasm32-wasip2` is also valid
  and is what the bundled git plugin uses; the SDK crate doc that said "do not pass
  `--target wasm32-wasip2`" was stale and is corrected. Smoke run: `backupcrawl ~` on the real
  home directory renders the `git` panel (2 `unknown`, 2 `dirty`, `plugins: git ok`), and
  `plugins list`/`plugins info git` show the bundled auto-trusted manifest.

### M2.0 task breakdown (contract hardening)

Acceptance for every task: `cargo fmt --check`,
`cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` green.

- **M2.0.1 Fine-grained filesystem grants.** `[capabilities].fs-read` stops being informational.
  At instantiation the host grants read-only WASI preopens for (a) the crawl root and (b) every
  resolved `fs-read` spec. Specs support `~` expansion and shell-style globs; a spec resolves to
  existing directories, and an unresolved spec is a warning (the plugin still loads). Nested
  roots are deduplicated to the broadest one, and each root is mapped at its own absolute guest
  path so `std::fs`/`gix` absolute paths keep working. `stat` remains unconditional (metadata
  only); content outside the granted roots is inaccessible. `plugins info` and the trust prompt
  print the resolved roots (or `read /` when a plugin declares `/`), not "documented intent".
  Bundled plugins may request broad roots honestly: git declares `fs-read = ["/"]` to keep
  ancestor-repository discovery working across the crawl-root boundary. *Done when:* tests prove
  a guest can read inside a granted root and cannot read outside it; `fs-read` resolution covers
  plain paths, `~` and globs; the CLI golden output shows the real scope; the git and CLI
  fixture detectors still pass their suites. **Deferred:** per-path read auditing and making
  `stat` path-scoped.
- **M2.0.2 Per-worker lifecycle and cache rules.** Document (and where cheap, test) the
  concurrency contract M4a will implement: one detector instance per worker, `init`/`finish`
  bracket that instance's lifetime, no shared mutable state between instances, shared
  preparation only through the host cache, benign cache-write races, no cross-worker ordering
  guarantees, and first-claim-wins still evaluated per path. Update
  `docs/plugin-authoring.md`, the core `Detector` docs and the SDK docs so plugin authors do not
  build single-instance assumptions. *Done when:* the contract is written down in the three
  places and a compile-time/unit test pins `Detector: Send` and the documented cache behavior.

### M2 task breakdown

- **M2.1 pacman plugin (`plugins/pacman`).** Native database parsing, no `pacman` process:
  `local/<pkg>/files` (verbatim paths, directories end in `/`, spaces preserved), `desc`
  (`%NAME%`, `%VERSION%`, `%BACKUP%`) and gzipped BSD `mtree` entries. `init` builds one
  path → package/mtree index, cached through the host cache keyed by the local db mtime, and
  disables the plugin with a visible note when the db is missing/unreadable. `check_dir`,
  `check_file` and `check_symlink` claim owned paths: matching metadata/symlink target → `clean`,
  any mismatch → `dirty` with pacman-style reasons (`Size mismatch`, `Modification time
  mismatch`, `UID/GID/Permissions mismatch`, `Symlink path mismatch`, `File type mismatch`, ...)
  and `package`/`version` attrs; unparseable mtree data → `unknown` with a diagnostic, never a
  wrong `clean`. `check-level = "metadata" | "checksum"` (checksum verifies md5/sha256, opt-in);
  config `db-path`, `backend = "native"` (the `pacman -Qkk` fallback stays deferred with a clear
  error), `timeout-ms`. Manifest: `fs-read = ["/"]` (package files live system-wide), `cache =
  true`, hooks dir/file/symlink, priority 20. Build with `scripts/build-pacman-plugin.sh` into
  `plugins/pacman/dist/{pacman.wasm,plugin.toml}`, embed in `bundled::REGISTRY`, and have CI fail
  on a stale artifact. *Done when:* native unit tests cover the parsers and comparison logic;
  host golden tests drive the built component against a fixture pacman db (clean, modified,
  deleted, type change, symlink, backup file, space in path, unknown format); the bundled plugin
  appears in `plugins list/info`; artifact rebuild is byte-identical.
- **M2.2 Core symlink handling.** The walker stops skipping symlinks. Each link becomes
  `LinkInfo { path, target, dangling }` (no traversal) and is offered to `check_symlink`; a claim
  is recorded like any other (pacman and later chezmoi claim owned links). Unclaimed links are
  collected into `CrawlResult.symlinks` and always reported: a dedicated console panel and a
  `symlinks` JSON array with `path`, `target`, `dangling` and `via`. Apply the
  target-in-managed-tree rule after the scan: if the resolved target is inside a subtree claimed
  `clean` with `prune = true` (or is itself a claimed-clean path), report the link clean with
  `via = <target>`; otherwise it stays loose/unmanaged. `--follow-symlinks` remains deferred. *Done
  when:* core tests cover a claimed link, an unclaimed loose link, a dangling link, the via-target
  rule and JSON/console golden output; git repositories with `prune = true` do not regress; the
  plugin authoring guide documents `check_symlink`.

### M4a task breakdown

- **M4a.1 Parallel walker with per-worker instances.** Rework `walker::scan` around a worker
  pool sized by `--jobs N` (default 1, `0` = available parallelism). Each worker builds its own
  detector instances from a factory, calls `init` once (serialized across workers so the first
  init warms the host cache and the rest hit it) and `finish` when its queue drains. Workers
  process directories from a shared queue (pruned directories are never enqueued), and each
  path is still handled by exactly one worker, so first-claim-wins is unchanged. Collect
  per-directory results and perform bottom-up loose-path collapsing and claim aggregation after
  the traversal, so output is byte-identical across `--jobs 1` and `--jobs 8`. Merge per-worker
  reports (errors summed, notes unioned in stable order, a disabled instance disables that
  plugin). *Done when:* a determinism test runs the same fixture tree at several `--jobs`
  values and compares console/JSON output; tests cover prune, denied paths, loose collapsing and
  symlinks under parallelism; each worker's `init`/`finish` runs exactly once; the host cache
  sees one preparation miss and N−1 hits for a fixture plugin.
- **M4a.2 Progress display.** Add `--progress` using `indicatif` + `console` on **stderr**, with
  open/closed directory counters, files seen, claims and denied paths, throttled and disabled
  automatically when stderr is not a TTY. Progress must never interleave with console/JSON
  output on stdout and must be off by default. *Done when:* a non-TTY (assert_cmd) run emits no
  progress bytes on either stream, a forced/TTY-style render is unit-tested, and `--progress`
  works with `--format json` without corrupting stdout.

  *Implemented (M4a, branch `feat/m4a-parallel-progress`):* `walker::crawl` takes
  `Vec<Vec<DetectorSlot>>` (one set per worker) and drives a shared FIFO work queue with
  `std::thread::scope`; `walker::scan` remains the single-worker convenience. A worker asks its
  own instances, collects a per-directory result, and the merge step rebuilds the tree for
  bottom-up loose collapsing exactly like the old recursion, then sorts claims/denied/symlinks and
  merges reports (summed errors, stable note union, disabled wins). `init` calls are serialized by
  one mutex (the single-flight cache guard), and `finish` runs once per worker instance. Core
  exposes `ProgressSnapshot`/`ProgressSink`/`NoProgress`; the CLI implements the sink with
  `indicatif` on stderr, throttled at 100 ms and disabled when stderr is not a TTY. `--jobs N`
  defaults to 1 (`0` = `available_parallelism`); worker sets are built up front from
  discovery/trust/`Runtime`, and a plugin that fails to instantiate is skipped once. Tests:
  `crates/backupcrawl-core/tests/parallel_walker.rs` (1/2/4-worker serialized-result equality
  over prune, non-prune claims, collapsing, denied paths and symlinks; per-worker init/finish
  counts; non-overlapping init; disabled-in-one-worker report merge) and the CLI suite
  (`--jobs 4` JSON equals `--jobs 1`, `--jobs 0`, non-TTY `--progress` is silent, JSON stays
  parseable). Denied paths are now sorted globally (the M4a ordering normalization).

### M3 task breakdown

- **M3.1 chezmoi plugin (`plugins/chezmoi`).** `exec = ["chezmoi"]` (the one bundled detector that
  keeps exec). `init` runs `chezmoi managed --format json --path-style absolute` and
  `chezmoi status --path-style absolute` once, resolves source/dest dirs from
  `~/.config/chezmoi/chezmoi.toml` or config overrides, and caches the result. Claims: managed
  and matching → `clean`; destination modified (` M`, `MM`, ...) → `dirty`; source modified but
  not applied (`M `, ...) → `ahead`; template/exec failure → `unknown` with a reason. Managed
  symlinks are claimed through `check_symlink`; `run_` scripts are ignored. Missing binary or
  failed exec disables the plugin with a visible note. Manifest declares `fs-read = ["~"]`,
  `exec = ["chezmoi"]`, `cache = true`; config `source-dir`, `dest-dir`, `include-types`,
  `chezmoi-path` (test hook, default `chezmoi`), `timeout-ms`; priority 30. Build
  `scripts/build-chezmoi-plugin.sh` into `plugins/chezmoi/dist/`, embed in the registry, CI
  staleness check. *Done when:* native model tests cover the status mapping; host golden tests
  use a fake `chezmoi` fixture binary (clean, dirty, ahead, unknown, symlink, missing binary);
  the bundled plugin is listed/inspected; artifact rebuild is byte-identical.
- **M3.2 syncthing plugin (`plugins/syncthing`).** Parse `config.xml` from `$STCONFDIR`,
  `~/.local/state/syncthing` or `~/.config/syncthing` (folders, GUI address/API key), detect
  `.stfolder` markers, and query `GET /rest/db/status?folder=<id>` once per folder per scan
  through the host HTTP import (localhost, plain HTTP, size-capped). Map idle/no-need/no-errors
  → `clean`; receive-only divergence → configurable `dirty`/`ahead`; `state = error`,
  `pullErrors > 0` or a missing marker on a configured folder → `dirty`; daemon unreachable
  without `assume-marker-only` → `unknown` with a reason (never a silent clean). Claims are made
  at folder-root level with `prune = true` and an attr noting "synced to cluster, not archived".
  Manifest: `fs-read = ["~/.local/state/syncthing", "~/.config/syncthing"]`, `http =
  ["http://127.0.0.1:8384"]`, `cache = true`, priority 40; config `address`, `api-key`,
  `require-daemon`, `assume-marker-only`, `receive-only-changes`, `timeout-ms`. Build
  `scripts/build-syncthing-plugin.sh` into `plugins/syncthing/dist/`, embed in the registry, CI
  staleness check. *Done when:* config parsing and status mapping have native tests; host golden
  tests use a local fake HTTP server (clean, syncing, error, unreachable, marker missing); the
  bundled plugin is listed/inspected; artifact rebuild is byte-identical.

## 13. Behavior inventory: carried over vs changed

The Python tool has no users, so nothing here is a compatibility requirement; this is a checklist
of what is worth keeping and what the rewrite intentionally does differently.

Carried over because it works well: positional path defaulting to `/`; per-plugin panels with
Dirty/Unsynced/Clean and hidden empty panels; ignored special files (sockets/FIFOs);
permission-denied collection; loose-path collapsing; first-claim-wins; dir claim prunes the
subtree; pacman backup-file semantics (`dirty` = edited); `--all`, `--ignore` and `--progress`
ideas; configurable ignore patterns.

Deliberately changed: CLI and config are redesigned (TOML config, subcommands) instead of being
flag-compatible; git no-upstream counts as `unknown` instead of `ahead`; symlinks are reported
instead of skipped; ignored files can be reported; pacman path-with-spaces parsing fixed; single
`git status` call per repo; ancestor repos can be detected; new statuses `unknown`/`ignored`
(`recoverable` comes with the Steam plugin); own JSON schema. Scan-based exit codes are deferred.

## 14. Open questions (with recommended defaults)

1. **Repo layout (resolved):** the Rust workspace becomes the repository root; the Python
   implementation is deleted from `main` and kept only in git history (tag `v0.2.6` already
   exists).
2. **Filesystem access for plugins (resolved; see Appendix A):** standard WASI filesystem
   access, read-only, scoped to the crawl root plus the resolved manifest `fs-read` roots
   (M2.0). Custom imports for `stat` (Unix metadata), `exec`, `http`, `cache` and `log`.
3. **JSON output (resolved):** free to design; no Python-compatible mode. Version the schema field.
4. **Status extensions (resolved):** `unknown`/`ignored` are wired from the start; `recoverable`
   is deferred and added together with the first plugin that emits it (Steam, section 7.6).
5. **Multiple simultaneous claims:** keep first-wins or allow a path to be reported by several
   mechanisms (e.g. git and pacman both own `/etc/foo`)? *Recommend: first-wins now; revisit if
   users hit it.*
6. **`--follow-symlinks`:** implement in M3 or defer? *Recommend: defer traversal, ship reporting.*
7. **Plugin distribution/trust:** sidecar `plugin.toml` next to the `.wasm` — simplest; embedding
   manifest in a wasm custom section — single file. *Recommend: sidecar now, custom section later.*
8. **Config format (resolved):** TOML only; no legacy JSON rcfile.
9. **Naming:** "detector" as the plugin kind name (vs "checker"/"provider")? *Recommend: detector.*
10. **Exit codes (resolved):** no `--fail-on` in v1; `0`/`2` only, scan results do not fail the
    run (section 8). Revisit once scripts need it.
11. **Console stack (resolved):** `console` + `indicatif`, panels hand-rolled; no TUI for now
    (section 8).

## Appendix A. Filesystem access for plugins: WASI preopens vs custom host imports

*Resolved 2026-10-11: standard WASI filesystem access, read-only. **Revised 2026-10-17 (M2.0):**
the preopen is scoped to the crawl root plus every resolved manifest `fs-read` root, each mapped
at its absolute guest path; plugins that need system-wide access declare `/` explicitly. Custom
`stat`/`exec`/`http`/`cache`/`log` imports. See A.6 for the decision and A.7 for the resulting
model; the analysis below documents why.*

### A.1 The problem

A WebAssembly component has no operating-system access by default. There are no syscalls, no
files, no network, no processes: a component can compute, and it can call functions the embedder
(backupcrawl) explicitly provides. For a detector to read `.git/HEAD`, syncthing's `config.xml` or
`/var/lib/pacman/local/...`, the embedder must hand it a way to do so. There are two established
ways, and they produce very different plugin code.

### A.2 Option A: custom host imports (what section 5.1 sketches)

The WIT contract would define `read-file`, `read-dir`, `metadata`, `read-link`; the guest calls
`backupcrawl:host/fs.read-file(path)`; host code checks the path against the plugin's grants, opens
the file, and returns the bytes. WASI is not linked at all, and `std::fs` cannot work.

- Policy is ours: canonicalize, enforce `{crawl-root}` + declared roots, deny symlink escapes, log
  every read, cap read sizes, and even deny-list sensitive paths while still crawling them.
- No standard-library file access: plugin code cannot use `std::fs`, `gix`, `notify`, or any crate
  that assumes a real filesystem without adapting it. Plugin authors use the SDK.
- Test-friendly: the host interface is a trait; a native mock makes unit tests trivial.

### A.3 Option B: WASI with read-only preopened directories

WASI is the standard "OS interface" for wasm components and is *capability-based*: at
instantiation the host builds a context listing directories to expose and the guest path each
appears at ("preopens"), with explicit permissions. A component can also import other WASI
interfaces (`clocks`, `random`, `environment`) or none.

Concrete example — instantiating the git detector for `backupcrawl /home/u`:

- preopen `/home/u` at guest path `/home/u`, read-only (crawl root);
- preopen `~/.config/git` at its absolute path, read-only (extra manifest grant);
- empty environment, no sockets, no process API (WASI has none);
- plugin code does `std::fs::read_to_string("/home/u/proj/.git/HEAD")` and it works.

The sandbox is enforced by wasmtime's `cap-std` layer: `..` escapes and symlinks that leave the
preopens are rejected; read-only directories make writes fail. When the crawl root is `/`, the
preopen is `/` — read-only whole-filesystem visibility for that plugin, which is exactly what a
scan of `/` implies (credential caveat in A.5).

### A.4 Comparison

| Dimension | A: custom fs imports | B: WASI preopens |
| --- | --- | --- |
| Plugin code | SDK calls only; no `std::fs` | idiomatic `std::fs`; ecosystem crates work (`gix`, `roxmltree`, ...) |
| Path policy | ours, path-level, can deny-list | wasmtime/`cap-std`, directory-level |
| Read auditing | every read logged/countable | not visible to the host |
| Writes | impossible (no API) | impossible only because preopens are read-only (configuration) |
| TOCTOU / symlink correctness | our responsibility | battle-tested (`cap-std`) |
| Host implementation effort | more code; we own the security-critical part | less code; wasmtime provides it |
| Testing plugins | trivial mocks | run under a WASI host (also easy) |
| WASI version dependency | none | pin wasmtime/toolchain to WASI 0.2/0.3 |
| Future native detectors (pacman mtree parsing) | easy (our reads) | easy, except Unix metadata (A.5) |
| Third-party authoring in other languages | requires a port of our SDK | standard WASI toolchains |

What the options mean for the planned detectors: in the original sketch git, pacman and chezmoi
all used `exec`; the chosen design moves git (`gix`) and pacman (database parsing) to native
implementations, keeps `exec` only for chezmoi, and syncthing only reads a config file and uses
HTTP. So the choice ended up being about both policy and dropping `exec` from the bundled set.

### A.5 Caveats that apply to both options

- **WASI has no process spawning.** `exec` is a custom host import in every variant; the allowlist,
  timeout and audit live in our host. A plugin granted `exec` can hand the child `git`/`pacman`
  arbitrary arguments, and those tools can read (and print) files the plugin itself cannot — so fs
  sandboxing guards against accidents and against plugins *without* exec, not against a malicious
  plugin that has exec.
- **Unix metadata.** WASI 0.2 file metadata exposes type, size, timestamps and link targets, but
  not Unix mode/uid/gid. If the native pacman parser needs those, a small custom `host-stat`
  import must stay regardless of the choice.
- **Whole-root scans.** Crawling `/` means the crawl-root grant is `/` for every plugin that needs
  to inspect the crawled tree. The trust prompt must show this plainly (`fs: read /`).

### A.6 Recommendation and decision

**Option B for file access, custom imports for everything else** (exec, http, cache, log) — the
hybrid. Reasons: plugin authors get `std::fs` and the Rust ecosystem (including `gix`, which the
git detector uses); sandbox path resolution is implemented by a battle-tested library instead of
by us; and the audit argument for A is weak once `exec` exists — the meaningful, enforceable
policies here are exec allowlists, HTTP allowlists and read-only-ness, all of which the hybrid
keeps.

**Decision (2026-10-11): the hybrid was chosen**, extended as follows: a custom `stat` import
covers the metadata WASI omits; and the bundled git and pacman detectors use native
implementations (gix, pacman database parsing) specifically to avoid `exec`, which remains a
capability for chezmoi and third-party plugins.

**Scope amendment (2026-10-11, first iteration; superseded by M2.0):** the preopen was `/` for
every plugin rather than the crawl tree plus declared extras. Since M2.0 the host grants the
crawl root plus resolved `fs-read` roots; plugins with genuinely system-wide needs (git ancestor
discovery, pacman package files) declare `/` in their manifest. See A.7.

### A.7 Chosen model in detail

1. **Tier 1 — content (standard WASI).** At instantiation, the host builds a read-only preopen
   for the crawl root plus every resolved `fs-read` root, mapped at its absolute guest path.
   Plugins use `std::fs` normally or crates such as `gix`; `cap-std` rejects every write and
   undeclared paths stay invisible. Nested roots are deduplicated to the broadest one.
2. **Tier 2 — metadata (`stat` import).** Returns type/size/mtime/mode/uid/gid/nlink for any path.
   Content never flows through this import, so per-path checking carries little risk and does not
   need preopen plumbing; it stays unconditional and metadata-only.
3. **Path specs (`fs-read`) are authoritative (M2.0).** Entries are specs (`~` expansion, globs
   such as `/var/lib/pacman` or `~/.local/state/syncthing*`) resolved to existing directories at
   instantiation. Unresolved specs are warnings (the plugin still loads), nested roots are
   deduplicated, and the crawl root is always granted. Globs are preferred over regexes for
   paths: the standard shell mental model, no regex escaping pitfalls (`/`, `.`, `+`). A regex
   mode can be added later if a real need appears.
4. **Published and pinned.** `plugins info` and the trust prompt print the manifest, the resolved
   read roots, exec programs, HTTP origins and the manifest hash. A plugin that declares `/` is
   shown as `fs: read /`; a scoped plugin shows each root.
5. **Consequences:**
   - A third-party plugin can read only the crawl root and the roots it declared. Bundled plugins
     may declare broad roots (git and pacman use `/`) because they ship with the binary and are
     auto-trusted; the scope is still visible.
   - No read auditing (preopen-level enforcement only). Revisit only if this becomes a hard
     requirement; switching a plugin's reads to custom imports would be a deliberate ABI change.
   - `exec` remains the strongest grant and is shown first; it is never implied by an fs grant.

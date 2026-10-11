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
- **Filesystem access (v1): read-only `/` for every plugin.** The planned detectors need ancestor
  repositories (7.1) and config-dependent paths (chezmoi source dir, syncthing `STCONFDIR`),
  which per-plugin grants only cover with fragile glob specs; v1 grants a blanket read-only
  preopen instead, `fs-read` stays documented intent, and scoping becomes the pre-third-party
  hardening step (M5). Writes remain impossible; `exec`/`http` stay allowlisted (Appendix A).
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

> File **content** is read through standard WASI filesystem access: in v1 the host instantiates
> each plugin with a read-only preopen of `/` (cheap and sufficient for every planned detector;
> capability scoping is revisited before third-party plugins ship). The custom `stat` import above
> only fills the metadata gap WASI 0.2 leaves (mode/uid/gid/nlink). `exec`, `cache`, `http` and
> `log` are custom imports; WASI has no process spawning, and we want allowlists for the rest.
> Full reasoning: Appendix A.

Design notes:

- **Filesystem model (decided; Appendix A).** *Content:* a read-only WASI preopen of `/` for
  every plugin in v1, so `std::fs` and crates such as `gix` work and wasmtime/`cap-std`
  guarantees nothing can be written. Fine-grained, manifest-scoped preopens are a later
  pre-third-party hardening step. *Metadata:* the custom `stat` import exposes mode/uid/gid/nlink,
  which WASI 0.2 omits. No custom file-content reads exist.
- **Grants are visible.** The manifest's `fs-read` specs and `exec`/`http` lists are printed by
  `plugins info` and the trust prompt, together with the manifest hash. In v1 `fs-read` is
  informational (the preopen is `/`); once scoping lands, it becomes the actual contract.
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
4. **Instantiate** — wasmtime component with a read-only `/` preopen (v1), per worker
   (section 9), memory + fuel/epoch limits set.
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
fs-read = ["~/.config/git"]            # documented intent; v1 preopens / read-only (Appendix A)
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

Capabilities: gix backend: `stat` only (`fs-read = []` documented; v1 preopens `/`); the exec
fallback adds `exec = ["git"]`.

Config: `backend = "gix" | "exec"` (default `gix`), `no-upstream`, `report-ignored`,
`ignored-mode = "matching" | "all"`, `ancestor-repos`, `recurse-submodules`, `timeout-ms`.

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

Capabilities: native `fs-read = ["/var/lib/pacman"]` (documented; v1 preopens `/`) + `stat`;
the exec fallback adds `exec = ["pacman"]`.

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

Capabilities: `fs-read = ["~/.local/state/syncthing", "~/.config/syncthing"]` (documented; v1
preopens `/`); `http = ["http://127.0.0.1:<port>"]` only.

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

Capabilities: `exec = ["chezmoi"]`,
`fs-read = ["~/.local/share/chezmoi", "~/.config/chezmoi"]` (documented; v1 preopens `/`).

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
- Unclaimed links become `SymlinkEntry` and are shown in their own console panel and JSON key
  (`symlinks`), never dropped:
  - attr `target`: resolved target path (absolute, normalized);
  - `dangling = true` if the target does not exist;
  - **target-in-managed-tree rule:** if the resolved target lies inside a subtree that was claimed
    `clean` by a plugin (or is itself a claimed-clean file), the link is reported `clean` with
    attr `via = <target>`. This is the common, correct case for dotfile setups where the real
    files live in a synced/git-managed tree.
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
- Panels: per-plugin, split into Dirty / Unsynced / Unknown / Clean (`Clean` only with `--all`).
  Loose / denied / symlink panels as today plus the new symlink panel.
- Console rendering: the `console` crate for styling/TTY detection and `indicatif` for progress
  (same ecosystem, closest analog to rich); panels are hand-rolled box drawing. No TUI
  (`ratatui`) for now — it is the right tool only for an interactive result browser.
- JSON: a versioned schema (`"schema": 1`), with per-plugin lowercase status keys and attrs. No
  Python-compatibility mode (decision in section 14).
- Exit codes: v1 stays conventional — `0` on success, `2` for usage/config errors; scan results
  (unbacked data, plugin warnings) do not change the exit status. A `--fail-on unbacked|error`
  mode is deliberately deferred until scripts need it: defining "unbacked" (loose/dirty/ahead vs
  unknown/ignored) only becomes meaningful once the detectors exist.

## 9. Performance and parallelism

- Walker: `jwalk`/`rayon` for parallel directory traversal; collect per-worker results, then merge
  and sort deterministically.
- Detector instances: one per worker thread (wasmtime `Store` is not `Sync`), each with its own
  mutable state. Expensive global preparations must go through the host cache so the second
  worker's `init` reads cache instead of re-running `pacman -Qkk`/`chezmoi managed`. A host
  single-flight guard for cache keys prevents a thundering herd of N identical preparation calls.
- Bulk `check-files` per directory reduces boundary crossings; wasmtime component calls are
  microseconds, so this is an optimization, not a necessity.
- Cache: per-plugin persistent KV under `$XDG_CACHE_HOME/backupcrawl/plugins/<name>/`, with
  invalidation keys driven by the plugin (e.g. pacman db mtime, chezmoi source mtime, syncthing
  config mtime). `backupcrawl cache clear` for manual control.
- Limits: wasmtime fuel/epoch interruption (cooperative cancel every ~50 ms), linear memory
  limit (e.g. 256 MiB), exec timeout + output caps (e.g. 64 MiB), HTTP timeout + size cap.
- `--stats`: per-plugin wall time, exec count, cache hits, errors — makes it visible which
  detector is slow (pacman `-Qkk` will usually be the culprit).
- Progress display: port `PathTracker` semantics to `indicatif` + `console` (open/close counters,
  stragglers).

Rough sequence: single-threaded crawler with per-plugin instances first (M1–M3), parallelism once
behavior is stable (M4). Parallel crawling changes result ordering and user-visible error
interleaving; keep it behind `--jobs` with a fixed default of 1 until proven.

## 10. Security and trust model

Threat model: the user may install a third-party detector from the internet; a detector may be
buggy or malicious. The host protects the machine.

- **No ambient authority.** Detectors get only what the host grants: read-only filesystem
  visibility (v1: all of `/`), optionally `exec`/`http` on top; no writes, no sockets beyond the
  `http` allowlist. Fine-grained fs grants replace the blanket preopen before third-party plugins
  ship.
- **Files are read-only** through a WASI preopen of `/` in v1, enforced by wasmtime/`cap-std`
  (no writes; there is nothing to escape to). Manifest `fs-read` specs are printed in
  `plugins info` and the trust prompt as documentation of intent, and become the real scope
  when fine-grained grants land before third-party plugins. The metadata `stat` import is
  likewise unrestricted in v1.
- **exec is the escape hatch.** A spawned `chezmoi` (or any allowlisted program a third-party
  plugin declares) runs with the user's rights and is *not* sandboxed. This is why the bundled git
  and pacman detectors avoid exec and why the capability must be loud: `backupcrawl plugins info
  <name>` prints granted programs, and the trust prompt lists `exec` grants first.
- **http** is allowlisted by scheme/host/port/path-prefix; default localhost-only; responses are
  size-capped. No redirects to non-allowlisted hosts.
- **Trust on first use:** manifests are hashed (sha256). Unknown plugin → interactive prompt
  (prints capabilities + hash) or `--trust-plugin`; the hash is written to `~/.config/backupcrawl/
  config.toml`. Hash change → prompt again. Optional minisign/sigstore verification later.
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
(M0 is broken down below). The Python implementation is already preserved by tag `v0.2.6`, so
no tagging ceremony is needed before the rewrite branch removes it.

| Phase | Theme | Deliverable |
| --- | --- | --- |
| M0 | Core without plugins | Rust workspace replaces the Python package; walker + result model + console/JSON printers; everything loose; CLI/config; fixture-based tests |
| M1 | Host + git | wasmtime host MVP: manifest, discovery, read-only `/` preopen, trust prompt, `init/check-dir/check-file/finish`, host imports stat/exec/cache/http/log, limits; **gix feasibility spike**, then the **git plugin** on gix (exec fallback behind `backend`), fixing the upstream bug; `plugins list/info` |
| M2 | pacman + symlinks | **pacman plugin** native (`files`/`desc`/`mtree` + `stat`, unknown-format handling); symlink basic handling (own panel/JSON, no following); golden-output tests |
| M3 | Dotfiles + sync | **chezmoi plugin**; **syncthing plugin**; `unknown`/`ignored` statuses wired through printers; `--ignored`, `--stats` |
| M4 | Parallel + polish | Parallel crawl + per-worker instances + single-flight cache; progress display (`indicatif` + `console`); trust/config polish; `plugin new` + SDK docs; first third-party plugin example |
| M5 | Hardening | Fine-grained plugin grants (`fs-read` becomes authoritative); harden native backends (gix edge cases, checksum level, drop the exec fallback if unused); optional native chezmoi source-tree parser; candidate plugins (Steam/recoverable status, restic/borg, cloud markers); plugin signing |

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
  ignored, loose-path collapsing; detector hook points exist but none are wired yet, so
  everything is loose; symlinks are skipped (M2 reports them). *Done when:* unit tests cover
  traversal, ignore matching, denied paths and collapsing on fixture trees.
- **M0.3 Printers.** Console panels via `console` (loose/denied only for now; per-plugin panels
  appear in M1) and JSON with `"schema": 1`; deterministic sorting. *Done when:* golden tests
  cover both formats on a fixture tree.
- **M0.4 CLI + config.** `clap` CLI (`PATH` default `/`, `--format`, `--all`, `--ignore`,
  `--config`, `-v`), TOML config at `~/.config/backupcrawl/config.toml` (`[ignore] paths`), CLI
  patterns append to config; missing config is fine, unreadable/invalid config exits 2. *Done
  when:* `assert_cmd` integration tests cover defaults, config+flag merge and invalid config.
- **M0.5 Wrap-up.** README and `--help` text final; manual smoke run on a scratch tree; commits
  reviewable per task.

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
   access, read-only; v1 preopens `/` for every plugin and keeps manifest `fs-read` specs as
   informational intent until fine-grained grants land (M5). Custom imports for `stat` (Unix
   metadata), `exec`, `http`, `cache` and `log`.
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

*Resolved 2026-10-11: standard WASI filesystem access, read-only; v1 grants a `/` preopen to every
plugin and keeps manifest `fs-read` specs as informational intent (fine-grained scoping lands
before third-party plugins). Custom `stat`/`exec`/`http`/`cache`/`log` imports. See A.6 for the
decision and A.7 for the resulting model; the analysis below documents why.*

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

**Scope amendment (2026-10-11, first iteration):** the read-only preopen is `/` for every plugin
rather than the crawl tree plus declared extras. The planned detectors need ancestor repositories
(7.1) and config-dependent roots (chezmoi source dir, syncthing `STCONFDIR`), which path-scoped
grants only cover with fragile glob specs; fine-grained preopens become a hardening step before
third-party plugins ship (M5).

### A.7 Chosen model in detail

1. **Tier 1 — content (standard WASI).** At instantiation, the host builds a read-only preopen of
   `/`. Plugins use `std::fs` normally or crates such as `gix`; `cap-std` rejects every write.
   Fine-grained, manifest-resolved preopens (crawl root + declared extras) are the later hardening
   step; the manifest field stays as documented intent until then.
2. **Tier 2 — metadata (`stat` import).** Returns type/size/mtime/mode/uid/gid/nlink for any path.
   Content never flows through this import, so per-path checking carries little risk and does not
   need preopen plumbing.
3. **Path specs (`fs-read`) are informational in v1.** Entries are specs (`~` expansion, globs
   such as `/var/lib/pacman` or `~/.local/state/syncthing*`) that document what the plugin
   intends to read; they are printed by `plugins info` and the trust prompt. When fine-grained
   preopens land, specs are resolved to existing directories at load time; unresolved specs are
   warnings by default, errors if the plugin marks them required. Globs are preferred over regexes
   for paths: the standard shell mental model, no regex escaping pitfalls (`/`, `.`, `+`). A regex
   mode can be added later if a real need appears.
4. **Published, not yet pinned.** `plugins info` and the trust prompt print the manifest, the
   documented read roots, exec programs, HTTP origins and the manifest hash. In v1 the read scope
   is always `/` regardless of what `fs-read` lists.
5. **Consequences accepted (v1):**
   - Every plugin can read every file readable by the user. The trust prompt says `fs: read /`
     plainly; fine-grained grants that make `fs-read` meaningful are tracked for M5.
   - No read auditing (preopen-level enforcement only). Revisit only if this becomes a hard
     requirement; switching a plugin's reads to custom imports would be a deliberate ABI change.
   - `exec` remains the strongest grant and is shown first; it is never implied by an fs grant.

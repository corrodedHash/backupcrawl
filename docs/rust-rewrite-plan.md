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
│                        host imports (exec/cache/http/log), plugin lifecycle          │
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

### Why WASM is still right even though detectors need to run external tools

WASM gives ABI isolation and a real sandbox for the guest code itself: a detector cannot open
files, read environment variables, open sockets or spawn processes by accident — every privileged
operation goes through a host import that we implement, allowlist and log. The four planned
detectors need `exec` (git, pacman, chezmoi), file reads (pacman db, syncthing config) and HTTP
(localhost syncthing). Those are exactly the operations we want declared in a manifest and
auditable. WASM does **not** sandbox child processes: a detector with `exec = ["git"]` gives the
spawned `git` the user's normal rights. This is documented in the trust model (section 10), and
native implementations (e.g. parsing the pacman database ourselves) are preferred where they
remove the need for `exec`.

## 5. Plugin contract

### 5.1 WIT sketch (illustrative, to be finalized in `backupcrawl-wit`)

```wit
package backupcrawl:detector@1.0.0;

interface types {
  enum status { clean, dirty, ahead, unknown, ignored, recoverable }

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
  record file-info { kind: file-kind, size: u64, mtime-ms: u64, mode: u32, uid: u32, gid: u32 }

  read-file: func(path: string) -> result<list<u8>, string>;
  read-dir:  func(path: string) -> result<list<string>, string>;
  metadata:  func(path: string) -> result<file-info, string>;
  read-link: func(path: string) -> result<string, string>;

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

Design notes:

- **No WASI.** Detectors are pure components importing only the host interface above. This keeps
  path handling uniform (host-style absolute paths), makes the policy layer trivial to audit and
  makes unit tests possible with a mock host. Consequence: detector code cannot use crates that
  call `std::fs`/`std::process` directly (e.g. `gix`, `reqwest`). Our four detectors don't need
  to; a future optional `wasi:filesystem` preopen capability can be added for third-party
  components that do (open question 14.2).
- **Host fs is read-only and policy-checked.** Every path is canonicalized and must fall under
  `{crawl-root}` (implicit for all detectors) or an explicit `fs-read` root from the manifest.
  Symlink escapes (a path that canonicalizes outside the grant) are denied. No write/edit API
  exists.
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
4. **Instantiate** — wasmtime component, per worker (section 9), memory + fuel/epoch limits set.
5. **`init(scan-info)`** — expensive global prep (pacman `-Ql`, chezmoi `managed`, syncthing
   config parse). Error → plugin disabled for this scan with a visible warning; the scan
   continues.
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
exec = ["git"]                        # program allowlist
fs-read = ["{crawl-root}", "~/.config/git"]
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
| **git** | `.git` dir, `.git` file (worktree/submodule), bare repo | `git status --porcelain=v2` records | `# branch.ab` / `# branch.upstream` | `exec git` |
| **pacman** | `pacman -Ql` (or `/var/lib/pacman/local/*/files`) | `pacman -Qkk` (or `mtree` compare) | n/a — package repos are the archive; modified file = needs own backup | `exec pacman`, later native db parse |
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
  root is above the crawl root, the current walker never notices. Fix: at `init`, plugin computes
  `git -C <root> rev-parse --show-toplevel`; if it succeeds, the enclosing repo is checked once
  and, if clean and prune is acceptable, reported as an ambient claim covering the crawl root.
  (Configuration `ancestor-repos = true`; off if surprising.)

Current detection — single invocation per repo, parse porcelain v2:

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

- One `git status` per repository per scan, cached in the instance; the file hook (when enabled)
  answers from the cache. Never per-file `git` calls.
- `GIT_OPTIONAL_LOCKS=0` in the child environment to avoid touching locks during status.
- Repo status result is not persisted across scans (cheap to recompute, correctness matters);
  `--ignored` only requested when `report-ignored = true` (it costs extra stat work).

Edge cases: git binary missing → `init` fails → plugin disabled with warning; `.git` unreadable →
`unknown` attr; repository on a slow/network filesystem → per-repo timeout (exec timeout,
default e.g. 30 s) → `unknown`.

Capabilities: `exec = ["git"]`, `fs-read = ["{crawl-root}"]`.

Config: `no-upstream`, `report-ignored`, `ignored-mode = "matching" | "all"`,
`ancestor-repos`, `recurse-submodules`, `timeout-ms`.

### 7.2 pacman

Managed detection (ownership):

- `pacman -Ql` → filename→package map, built once in `init`, cached in the host cache keyed by
  the local db mtime (`/var/lib/pacman/local`) so parallel workers don't repeat it.
- Native alternative (phase 2, removes the exec dependency): parse
  `/var/lib/pacman/local/<pkg>-<ver>/files` (one path per line, directories end with `/`) plus
  `desc` (`%NAME%`, `%VERSION%`, `%BACKUP%`) and `mtree` (gzipped BSD mtree). For ownership alone
  `files` is sufficient and immune to the current `split(maxsplit=1)` bug with spaces in paths.

Current detection (integrity):

- MVP: `pacman -Qkk` once in `init`; parse lines starting with `warning:` or `backup file:` of
  the form `<prefix>: <pkg>: <path> (<reason>)`. Known reasons (already in the Python code):
  Modification time mismatch, Size mismatch, GID/UID/Permissions mismatch, Permission denied,
  Symlink path mismatch, File type mismatch. Unknown reasons are collected and logged. Any file
  with a reason → `dirty`.
- Files listed in a package's `%BACKUP%` are reported by `-Qkk` like any other file; a differing
  backup file means it contains user/administrator edits → `dirty` is correct: it is not
  reproducible from the package and needs its own backup.
- Note the default `-Qkk` level compares metadata (existence, size, mtime, uid/gid, permissions,
  symlink target); checksum verification needs `--check` and is much slower. Keep metadata level
  as default, `check-level = "checksum"` as opt-in.
- Native alternative (phase 2): compare each owned file against its `mtree` entry (existence,
  type, size, mtime, mode, uid/gid; optional md5/sha256). Handles the same reasons without
  invoking pacman or requiring the pacman binary.

Semantics: a `clean` package file is recoverable by reinstalling the package — it does not need to
be backed up. `dirty` package files are the interesting ones (edited `/etc` configs, user
modifications), exactly as today.

Caching: file list + dirty set in the host cache keyed by local db mtime; `-Qkk` is the slow part
(minutes on large systems), so it runs at most once per scan and results are cached for the scan
duration. `--dbpath`/`--root`/`--config` from plugin config if non-standard.

Edge cases: pacman missing → plugin disabled; db lock present (`/var/lib/pacman/db.lck`) → read-only
commands still work, but db mtime will change when a transaction finishes → cache invalidates
naturally; foreign/AUR packages are in the db like any other; directories and symlinks are in
`-Ql` (only claim what makes sense; symlinks can be claimed too, see 7.5); files with newlines in
names — `-Qkk` output parsing cannot represent them, the native parser can (note as limitation).

Capabilities: MVP `exec = ["pacman"]`; native phase `fs-read = ["/var/lib/pacman"]`.

Config: `db-path`, `check-level`, `timeout-ms`.

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

Capabilities: `fs-read = ["~/.local/state/syncthing", "~/.config/syncthing"]` + `{crawl-root}`;
`http = ["http://127.0.0.1:<port>"]` only.

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

Capabilities: `exec = ["chezmoi"]`,
`fs-read = ["~/.local/share/chezmoi", "~/.config/chezmoi"]` + `{crawl-root}`.

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

Steam/Flatpak/Nix motivate the optional `recoverable` status: "not backed up, but reproducible
without a backup." This is distinct from `clean` (verified copy exists) and prevents huge
re-downloadable trees from drowning the report. If `recoverable` is added, the printer shows it
in a muted panel and it never counts as "unbacked data found".

## 8. Statuses, aggregation and output

- Statuses: `clean`, `dirty`, `ahead`, `unknown`, plus optional `ignored`, `recoverable`
  (section 3). Existing printers map `ahead` to "Unsynced" as today.
- Panels: per-plugin, split into Dirty / Unsynced / Unknown / Clean (`Clean` only with `--all`).
  Loose / denied / symlink panels as today plus the new symlink panel.
- JSON: recommend a versioned schema (`"schema": 2`), with per-plugin lowercase status keys and
  attrs. A `--format json-v1` compatibility mode can emit the Python key layout
  (`Dirty`/`Unsynced`/`Clean`) during migration. Open question 14.3.
- Exit codes (new): `--fail-on unbacked|error|never` (default `never`, preserving 0.2.6 behavior):
  `1` = unbacked data found, `2` = plugin/config error. A scriptable alternative to scraping
  output.

## 9. Performance and parallelism

- Walker: `jwalk`/`rayon` for parallel directory traversal; collect per-worker results, then merge
  and sort deterministically.
- Detector instances: one per worker thread (wasmtime `Store` is not `Sync`), each with its own
  mutable state. Expensive global preparations must go through the host cache so the second
  worker's `init` reads cache instead of re-running `pacman -Qkk`/`chezmoi managed`. A host
  single-flight guard for cache keys prevents a thundering herd of N identical exec calls.
- Bulk `check-files` per directory reduces boundary crossings; wasmtime component calls are
  microseconds, so this is an optimization, not a necessity.
- Cache: per-plugin persistent KV under `$XDG_CACHE_HOME/backupcrawl/plugins/<name>/`, with
  invalidation keys driven by the plugin (e.g. pacman db mtime, chezmoi source mtime, syncthing
  config mtime). `backupcrawl cache clear` for manual control.
- Limits: wasmtime fuel/epoch interruption (cooperative cancel every ~50 ms), linear memory
  limit (e.g. 256 MiB), exec timeout + output caps (e.g. 64 MiB), HTTP timeout + size cap.
- `--stats`: per-plugin wall time, exec count, cache hits, errors — makes it visible which
  detector is slow (pacman `-Qkk` will usually be the culprit).
- Progress display: port `PathTracker` semantics to `indicatif` (open/close counters, stragglers).

Rough sequence: single-threaded crawler with per-plugin instances first (M1–M3), parallelism once
behavior is stable (M4). Parallel crawling changes result ordering and user-visible error
interleaving; keep it behind `--jobs` with a fixed default of 1 until proven.

## 10. Security and trust model

Threat model: the user may install a third-party detector from the internet; a detector may be
buggy or malicious. The host protects the machine.

- **No ambient authority.** Detectors get exactly the host imports their manifest declares; no
  WASI, no clocks, no sockets, no writes.
- **fs is read-only** and every path is canonicalized and checked against grants
  (`{crawl-root}` + explicit roots). Reads outside grants are denied and logged.
- **exec is the escape hatch.** A spawned `git`/`pacman`/`chezmoi` runs with the user's rights and
  is *not* sandboxed. This is unavoidable for tool-backed detectors and must be loud: `backupcrawl
  plugins info <name>` prints granted programs; the trust prompt lists `exec` grants first. Prefer
  native implementations (pacman db parser) to shrink exec usage.
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

Config (TOML, `~/.config/backupcrawl/config.toml`; the legacy JSON rcfile is accepted with a
deprecation warning):

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
            [--plugins a,b,c] [--no-plugins] [--stats] [--fail-on ...]
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

| Phase | Deliverable |
| --- | --- |
| M0 | Workspace skeleton; core walker + result model + console/JSON printers; everything loose; legacy CLI/rcfile parsing; parity tests vs Python fixtures |
| M1 | wasmtime host MVP: manifest, discovery, trust prompt, `init/check-dir/check-file/finish`, host imports fs/exec/cache/log, limits; **git plugin** with porcelain-v2 parsing (fixes upstream bug); `plugins list/info` |
| M2 | **pacman plugin** parity (`-Ql`/`-Qkk`, unknown-reason reporting); symlink basic handling (own panel/JSON, no following); golden-output tests; legacy JSON schema mode |
| M3 | **chezmoi plugin**; **syncthing plugin**; `unknown`/`ignored` statuses wired through printers; `--ignored`, exit codes, `--stats` |
| M4 | Parallel crawl + per-worker instances + single-flight cache; progress display; trust/config polish; `plugin new` + SDK docs; first third-party plugin example |
| M5 | Native pacman db parser (drop exec); optional native chezmoi source-tree parser; candidate plugins (recoverable status, restic/borg, cloud markers); optional WASI preopen capability for third-party components |

## 13. Parity checklist (explicitly preserved vs deliberately changed)

Preserved: positional path defaulting to `/`; `-v/-a/-p/-i/-f`; JSON keys `not_backed_up`,
`permission_denied` (in compat mode); per-plugin panels with Dirty/Unsynced/Clean; empty panels
hidden; ignored special files (sockets/FIFOs); permission-denied collection; loose-path collapsing;
first-claim-wins; dir claim prunes subtree; pacman backup-file semantics (`dirty` = edited).

Deliberately changed: git no-upstream no longer counts as `ahead`; symlinks are reported instead of
skipped; ignored files can be reported; pacman path-with-spaces parsing fixed; single `git status`
call per repo; ancestor repos can be detected; new statuses `unknown`/`ignored` (and optionally
`recoverable`); JSON schema v2; exit codes; TOML config preferred.

## 14. Open questions (with recommended defaults)

1. **Repo layout:** keep `corrodedHash/backupcrawl` with a `rust/` subdirectory or
   move the Python version to a branch and make the workspace the repo root? *Recommend: new repo
   layout on `main`, tag the last Python version.*
2. **No-WASI vs WASI preopens:** pure host imports (uniform policy, plugins can't use `std::fs`
   crates like `gix`) vs `wasi:filesystem` preopens (library compatibility, coarser policy).
   *Recommend: host imports for v1; optional preopen capability later.*
3. **JSON output:** schema v2 with `--format json-v1` compat, or keep the Python schema exactly?
   *Recommend: v2 + compat flag.*
4. **Status extensions:** accept `unknown`/`ignored`; add `recoverable` now or later?
   *Recommend: `unknown`/`ignored` yes (wire from the start), `recoverable` later.*
5. **Multiple simultaneous claims:** keep first-wins or allow a path to be reported by several
   mechanisms (e.g. git and pacman both own `/etc/foo`)? *Recommend: first-wins now; revisit if
   users hit it.*
6. **`--follow-symlinks`:** implement in M3 or defer? *Recommend: defer traversal, ship reporting.*
7. **Plugin distribution/trust:** sidecar `plugin.toml` next to the `.wasm` — simplest; embedding
   manifest in a wasm custom section — single file. *Recommend: sidecar now, custom section later.*
8. **Config format:** TOML with legacy JSON acceptance. *Recommend: yes.*
9. **Naming:** "detector" as the plugin kind name (vs "checker"/"provider")? *Recommend: detector.*

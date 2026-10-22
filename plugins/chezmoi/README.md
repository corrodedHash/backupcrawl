# chezmoi detector

Bundled detector for [chezmoi](https://www.chezmoi.io) dotfiles. It is the one
bundled plugin that uses `exec`: `chezmoi managed` and `chezmoi status` already
answer the three questions of the plugin model, while a faithful native
implementation would have to reimplement the source-state filename encoding and
template evaluation (§7.4 of `docs/rust-rewrite-plan.md`).

## What it answers

| Question | How |
| --- | --- |
| **Managed?** | `chezmoi managed --format json --path-style absolute` |
| **Current?** | `chezmoi status --path-style absolute` |
| **Propagated?** | Not chezmoi's job — the **git** detector reports the source repository |

Both commands run once in `init`, through the host `exec` import. The result is
cached in the host cache keyed by the source/destination directory mtimes, the
`include-types` setting and the program, so a scan with several workers (or a
second scan of unchanged roots) skips both processes.

## Status mapping

`chezmoi managed` lists the managed destination paths. `chezmoi status` prints
two status characters followed by an absolute path. The plugin reads the columns
as documented in §7.4 — first column **source**, second column **target**:

| `chezmoi status` | Meaning | Detector status |
| --- | --- | --- |
| *(path absent)* | managed and no differences | `clean` |
| ` M`, `MM`, ` D`, ... | target column changed: destination content is not captured in the dotfiles source | `dirty` |
| `M `, `A `, ... | only the source column changed: the source state is not on disk yet | `ahead` |
| status failed / unparsable | state cannot be determined | `unknown` + `reason` attr |

`dirty` wins over `ahead` (`MM`). Claims carry a `source` attribute with the
resolved source directory; `dirty`/`ahead` carry `state =
"target-modified"`/`"source-modified"`, and `unknown` carries `reason`.

`run_` scripts (`run_`, `run_once_`, `run_onchange_`, ...) are stateful side
effects with no target file and are never claimed.

## Composition with git

The source directory (`~/.local/share/chezmoi`) is usually a git repository.
Chezmoi reports the **destination** files under `$HOME`; the git detector
reports the **source** repository. If the source is pushed, the dotfiles are
archived; if it is not, git reports the source repository as `ahead`. A
`clean` claim from chezmoi only means "chezmoi has captured this file" — it does
not mean the dotfiles repository has left the machine, and it is not an archive
(§3 of the plan).

## Configuration

```toml
[plugins.chezmoi]
# source-dir = "/home/u/dotfiles"   # default: chezmoi.toml, then ~/.local/share/chezmoi
# dest-dir   = "/home/u"            # default: chezmoi.toml, then $HOME
# include-types = "files,dirs,symlinks"  # default all
# chezmoi-path = "chezmoi"          # test/override hook
# timeout-ms = 5000
```

`source-dir`/`dest-dir` are resolved from the plugin config first, then
`~/.config/chezmoi/chezmoi.toml` (`sourceDir`/`destDir`), then the defaults.
The config file is parsed directly; template expressions are not evaluated.
Explicit overrides are forwarded to chezmoi as `--source`/`--destination`.

`include-types` restricts the managed set. It is forwarded as
`--include=files,dirs,symlinks` when not all types are selected.

## Capabilities

```toml
[capabilities]
exec = ["chezmoi"]
fs-read = ["~"]
cache = true
http = []
```

`exec` is the strongest grant in the trust model: the spawned `chezmoi` runs
with the user's rights and is **not** sandboxed. The single-program allowlist is
shown in `plugins info` and the trust prompt. `fs-read = ["~"]` grants read-only
access to the destination tree under `$HOME`; the crawl root is always granted
on top of it.

A missing or failing `chezmoi managed` is an `init` error: the host disables the
plugin with a visible note and nothing is claimed. A failing `chezmoi status`
is not fatal (every managed path becomes `unknown`), and such a snapshot is not
cached so a fixed template is retried by the next scan.

## Limitations

- Chezmoi is inherently per-user. When crawling `/`, only the current user's
  chezmoi state is visible; other users' dotfiles are out of scope.
- WASI starts with an empty environment, so the plugin infers `$HOME` from
  `/home/<user>`, `/Users/<user>`, `/root` or a home-shaped crawl root. A
  non-standard home needs `source-dir`/`dest-dir` overrides. The detected home
  is passed to the child as `HOME`.
- `chezmoi unmanaged`/`report-unmanaged-in-source` is deferred (MVP skips it).
- A native source-tree parser (for degraded mode without the binary) remains an
  M5 stretch goal.

## Build

```console
$ scripts/build-chezmoi-plugin.sh          # builds the wasm32-wasip2 release
```

The component is written to `dist/chezmoi.wasm` (gitignored; also built
automatically by `scripts/build-components.sh`) and is embedded in the binary via
`crates/backupcrawl-host/src/bundled.rs`.

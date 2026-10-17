# Backup Crawl

Detect files which are not backed up.

`backupcrawl` walks a directory tree and reports what no backup mechanism
(git, pacman, syncthing, chezmoi, ...) has captured.

This repository is the Rust rewrite. Each mechanism is a WebAssembly plugin
component (wasmtime + the component model) that the binary loads at runtime; the
core only walks the filesystem, queries the plugins and aggregates the result.
**git**, **pacman** and **chezmoi** are the detectors wired up so far — they
report repository/package/dotfile state (clean, dirty, unsynced) instead of
dumping every tracked/owned/managed file into "not backed up". The remaining
mechanisms and the roadmap live in
[`docs/rust-rewrite-plan.md`](docs/rust-rewrite-plan.md); plugin authors should
read [`docs/plugin-authoring.md`](docs/plugin-authoring.md).

## Build

    cargo build --release

The binary ends up at `target/release/backupcrawl`. The git, pacman and chezmoi
detectors are embedded in the binary, so no plugin directory is required. The
chezmoi detector runs the `chezmoi` binary (the one bundled detector that uses
`exec`); when it is missing, that plugin is disabled with a visible note and the
scan continues.

## Tests

    cargo test --workspace

The suite is wasmtime- and git-fixture-heavy, so
[`scripts/test-stats.py`](scripts/test-stats.py) records wall/CPU time, peak RSS,
block I/O, build cost and the cold wasmtime JIT cost per test binary, and checks
them against a committed baseline:

    scripts/test-stats.py --check          # fail on regressions
    scripts/test-stats.py --update-baseline

See [`docs/test-metrics.md`](docs/test-metrics.md) for the metric definitions and
the baseline policy.

## Usage

    backupcrawl [PATH]

    backupcrawl ~
    backupcrawl /home --format json
    backupcrawl / --ignore '**/node_modules' --ignore '/proc'
    backupcrawl --config ./backupcrawl.toml
    backupcrawl --no-plugins            # loose/denied walk only
    backupcrawl --plugins git,pacman,chezmoi  # run only the named detectors

Console output shows per-plugin panels (Dirty / Unsynced / Unknown, plus Clean
with `--all`), the "Not backed up" and "Permission denied" panels, and a
"Symlinks" panel for links no detector claimed (with a `(dangling)` marker;
links whose target is in a clean-managed tree show up as clean with `--all`).
JSON output is versioned (`"schema": 1`) and carries the same data in the
`not_backed_up`, `permission_denied`, `symlinks` (`path`, `target`, `dangling`,
optional `via`) and `plugins` arrays. A final `plugins: git ok, ...` line
summarizes each detector's fate.

## Plugins

Detectors are discovered from `--plugin-dir DIR` (repeatable), then
`$XDG_CONFIG_HOME/backupcrawl/plugins/`, then `/usr/lib/backupcrawl/plugins/`,
then the plugins bundled inside the binary. The first source to define a name
wins. Bundled plugins are auto-trusted; a third-party plugin prompts on first use
and can be trusted non-interactively with `--trust-plugin=NAME`.

    backupcrawl plugins list            # discovered plugins, sources and trust
    backupcrawl plugins info git        # resolved manifest and capabilities
    backupcrawl plugins verify          # offline trust check (exit 2 if untrusted)

Writing a detector is covered in
[`docs/plugin-authoring.md`](docs/plugin-authoring.md).

## Configuration

`~/.config/backupcrawl/config.toml`:

    [ignore]
    paths = ["/proc", "/sys", "/dev", "/run", "**/node_modules"]

    [plugins]
    enabled = ["git"]                    # empty = all discovered
    order   = ["git"]                    # first-claim-wins order

    [plugins.git]
    no-upstream    = "unknown"           # unknown | ahead | clean
    report-ignored = false
    ancestor-repos = true
    # backend = "exec"                   # git CLI fallback instead of gix

    [plugins.pacman]
    db-path     = "/var/lib/pacman"
    check-level = "metadata"             # metadata | checksum
    # backend = "native"                 # "pacman" (-Qkk) is not implemented yet

    [plugins.chezmoi]
    # source-dir = "/home/u/dotfiles"    # default: chezmoi.toml, then ~/.local/share/chezmoi
    # dest-dir   = "/home/u"             # default: chezmoi.toml, then $HOME
    # include-types = "files,dirs,symlinks"

`--ignore` globs are appended to the configured ones. A leading `~` in a pattern
is expanded to your home directory. Unknown top-level config tables are rejected
so a typo is not silently ignored.

## Exit codes

`0` on success, `2` for usage or config errors (including an untrusted plugin
under `plugins verify`). Scan results do not change the exit code yet; a
`--fail-on unbacked|error` mode is deferred until scripts need it.

## License

MIT

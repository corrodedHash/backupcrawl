# Backup Crawl

Detect files which are not backed up.

`backupcrawl` walks a directory tree and reports what no backup mechanism
(git, pacman, syncthing, chezmoi, ...) has captured.

This repository is the Rust rewrite: detectors are being ported as WebAssembly
plugin components. The current state is the core walker and CLI — no detectors
are wired yet, so every readable path is reported as "not backed up". The
design and roadmap live in [`docs/rust-rewrite-plan.md`](docs/rust-rewrite-plan.md).

## Build

    cargo build --release

The binary ends up at `target/release/backupcrawl`.

## Usage

    backupcrawl [PATH]

    backupcrawl ~
    backupcrawl /home --format json
    backupcrawl / --ignore '**/node_modules' --ignore '/proc'
    backupcrawl --config ./backupcrawl.toml

Console output shows "Not backed up" and "Permission denied" panels; JSON
output is versioned (`"schema": 1`).

## Configuration

`~/.config/backupcrawl/config.toml`:

    [ignore]
    paths = ["/proc", "/sys", "/dev", "/run", "**/node_modules"]

`--ignore` globs are appended to the configured ones. A leading `~` in a
pattern is expanded to your home directory.

## Exit codes

`0` on success, `2` for usage or config errors. Scan results do not change the
exit code yet (a `--fail-on` mode is planned once detectors exist).

## License

MIT

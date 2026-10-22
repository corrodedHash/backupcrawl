# Pacman index cost, memory and host limits

Investigation and redesign, 2026-10-18 · relates to `docs/rust-rewrite-plan.md`
§7.2 and §9

This document records why the pacman index was reshaped, how it is stored now,
and the measured memory/fuel cost. It supersedes the earlier revision that
scoped the index and raised the host memory cap.

## Original symptom

Scanning any tree disabled the bundled pacman detector after ~1,400
`path ... is claimed by both ...` warnings:

```
detector `pacman` disabled: init trapped: ... all fuel consumed by WebAssembly
```

The scan was correct apart from the missing pacman claims, but the plugin was
unusable on a normal Arch installation.

## The two problems

1. **Unbounded, heap-string-heavy representation.** `init` parsed the whole
   database into a `BTreeMap<String, OwnedPath>`. Every entry owned two heap
   `String`s (`package`, `version`) plus an inline mtree entry with up to four
   more (`link`, `md5` hex, `sha256` hex, `invalid`). For a full Arch system
   that is ~450 MiB live, and serializing it as JSON needed another ~123 MiB
   buffer — more than the (then 256 MiB) per-detector cap. The cap was raised to
   512 MiB as a stopgap.
2. **Unscoped, eager work.** `init` read every package's `files` and parsed
   every gzipped `mtree`, even for a crawl that could never ask about `/usr`.
   Fuel was a single 10-billion budget, but preparation legitimately needs tens
   of billions.

Scoping to the crawl root (kept from the earlier fix) removes the second
problem for ordinary scans but not the first: a full-system scan still visits
mostly-owned paths and needs per-path metadata.

## Host used for the numbers

Arch Linux, 1,695 packages in `/var/lib/pacman/local`, 520,735 `files` lines,
**479,485 unique owned paths** (27.3 MiB of path bytes). Largest single `mtree`
is 5 MiB uncompressed (`breeze-icons`). Bundled release component throughout.

## Design: a compact, partly-lazy index

Each path is keyed by a 64-bit FNV-1a hash of its absolute path. The index is
three cache entries (schema `INDEX_FORMAT = 3`):

| Entry | Contents | Size (measured) | Loaded |
| --- | --- | ---: | --- |
| `…:…:metadata` | header, interned package table, shared string arena (link targets, diagnostics), 64-bit-hash Bloom filter | 1.53 MiB | fully (`cache-get`) |
| `…:…:metadata:t` | open-addressed `hash → record index` table | 7.40 MiB | fully (`cache-get`) |
| `…:…:metadata:r` | one 52-byte record per owned path (plus, at `checksum` level, a 48-byte md5+sha256 arena) | 23.78 MiB | **one record at a time** (`cache-read`) |

Total on disk ≈ 32.7 MiB, down from 123 MiB of JSON.

A lookup hashes the path, rejects negatives with the Bloom filter, probes the
in-memory table, and reads the matching record on demand. The cache key
includes the database path, the `local/` mtime, the crawl root **and the
check level** (a `checksum` index carries the digest arena; a `metadata` index
does not). Different scopes, check levels or database snapshots never share an
entry.

`init` streams every `mtree` entry instead of building a map, so a package with a
multi-megabyte mtree costs one entry of guest memory at a time. `files` is read
first, so irrelevant packages never pay for `desc`/`mtree` parsing.

### New host import: `cache-read`

`cache-get` returns a whole entry, which would defeat the lazy records. The
`host` interface therefore gains:

```wit
cache-read: func(key: string, offset: u64, len: u32) -> result<option<list<u8>>, string>;
```

`none` means the entry is missing (a miss, like `cache-get`); an empty list
means the offset is at or past the end; a short list is a range that crosses the
end. The host caps `len` at 1 MiB and caches the open file handle so an
entry-by-entry scan is one `seek`+`read` per record. It is gated by the same
`cache = true` capability. If a ranged read fails mid-scan, the plugin reloads
the whole records entry once and continues from memory, so a host that cannot
serve ranges degrades gracefully instead of dropping claims.

## Measurements

Index build (cold, fresh cache) and steady state, on the host above:

| Scenario | Peak guest linear memory | `init` fuel | Result |
| --- | ---: | ---: | --- |
| Full index, cold (`root = /`) | ~44 MiB | ~51.1 billion | succeeds |
| Full index, warm (cache hit, records lazy) | ~12 MiB | ~0.4 billion | succeeds |
| `/usr/bin` or a project root (scoped) | tiny | tiny | succeeds |

The ~44 MiB cold peak is the transient build buffer (the 23.8 MiB record area
is reserved up front so it never reallocates, plus the 7.4 MiB table and the
meta/strings). Steady state is the ~9 MiB meta + table. Both are well under the
50 MiB goal.

Before the redesign a full system needed ~450 MiB and 64 MiB was not enough to
build; now 44 MiB builds and 12 MiB reloads.

Reproduce (Arch only; CI has no Arch host):

```sh
cargo build --release -p backupcrawl-cli
./target/release/backupcrawl ~ --plugins pacman
./target/release/backupcrawl /usr/bin --plugins pacman --format json
```

To re-measure the ceiling, instantiate the bundled pacman component with a
`Limits { max_memory: N, .. }` and call `init` against `/var/lib/pacman` with
`root = /`; the smallest `N` that succeeds is the build peak. `fuel_remaining()`
right after `init` gives the fuel figure.

## Host limits

`Limits::max_memory` is back to **256 MiB** (the pre-stopgap value). The pacman
index no longer needs the 512 MiB ceiling that the eager JSON representation
required. `Limits::init_fuel` stays at **200 billion** (≈4× the measured
full-system need) so a runaway `init` is still bounded; `fuel_per_call` stays at
10 billion for `check_*`/`finish`.

## Follow-ups (not done here)

- The cold build still holds the whole record area (~24 MiB) because
  `cache-set` writes one value. A host append/chunked-write import would let it
  stream.
- Per-plugin memory/fuel budgets (rather than host-wide values) would let each
  detector keep its own ceiling.
- A minimal-perfect-hash would shrink the 7.4 MiB table further, at the cost of
  a more complex builder.

## Reproduction

```sh
# unit/host suites
cargo test -p backupcrawl-plugin-pacman -p backupcrawl-host

# real-system smoke runs
cargo build --release -p backupcrawl-cli
./target/release/backupcrawl ~ --plugins pacman
./target/release/backupcrawl /usr/bin --plugins pacman --format json
```

`cargo test` must run somewhere with a real pacman database for the numbers to
be meaningful; CI has no Arch host, so these are not regression tests.

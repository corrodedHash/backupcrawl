//! The `gix` backend: repository discovery, HEAD/upstream inspection and
//! porcelain-v2-equivalent status.
//!
//! This module is the direct descendant of the M1.7 feasibility spike
//! (`spikes/gix-feasibility/component/src/lib.rs`); the gix call patterns are
//! copied from there, with two differences:
//!
//! * status iteration also collects the *paths* of dirty/ignored entries (the
//!   spike only counted them), so the file hook can answer from the cache;
//! * `ignored` collection is conditional on `report-ignored` (§7.1: "ignored
//!   collection only runs when `report-ignored = true` (it costs extra stat
//!   work)").
//!
//! Repository content is read through gix over the host's read-only `/`
//! preopen; `core.fileMode = false` compensates for WASI's missing executable
//! bit (otherwise every tracked `0755` file looks modified). The `memmap2` and
//! `filetime` shims are wired in via `[patch.crates-io]` in the root
//! `Cargo.toml`.
//!
//! The backend-neutral records and verdict logic live in [`crate::model`]; this
//! module only produces a [`RepoInspection`] from an opened
//! [`gix::Repository`]. The `exec` backend ([`crate::exec`]) mirrors it using
//! `git status --porcelain=v2` through the host `exec` import (M1.9).

use backupcrawl_plugin_sdk::Status;
use gix::bstr::BString;

use crate::model::{
    HeadInfo, HeadKind, InspectOptions, RepoInspection, RepoKind, Upstream, finalize,
    record_head_attrs, record_upstream_attrs, unknown,
};

/// Open options shared by every repository access: WASI has no executable bit.
pub fn open_options() -> gix::open::Options {
    gix::open::Options::default().config_overrides(["core.fileMode=false".to_owned()])
}

/// Opens exactly the repository at `path` (no upward discovery).
pub fn open_at(path: &std::path::Path) -> Result<gix::Repository, String> {
    gix::open_opts(path, open_options()).map_err(|err| reason("cannot open repository", &err))
}

/// Discovers the repository enclosing `path` by searching upwards.
pub fn discover_ancestor(path: &std::path::Path) -> Result<gix::Repository, String> {
    gix::discover_opts(path, Default::default(), open_options())
        .map_err(|err| reason("not inside a git repository", &err))
}

/// Formats a gix error as a one-line `reason` attribute.
///
/// gix attaches the full `Caused by` chain to some errors. The JSON output is
/// one row per entry, so the reason must not contain newlines; the `exec`
/// backend trims child stderr to its first line for the same reason.
fn reason(context: &str, err: &impl std::fmt::Display) -> String {
    let message = err.to_string();
    let first = message.lines().next().unwrap_or_default().trim();
    if first.is_empty() {
        context.to_owned()
    } else {
        format!("{context}: {first}")
    }
}

/// Inspects an opened repository. Never fails: an error is folded into an
/// `unknown` verdict with a `reason` attribute, as §7.1 requires.
pub fn inspect(repo: &gix::Repository, opts: &InspectOptions) -> RepoInspection {
    let mut inspection = RepoInspection {
        workdir: repo.workdir().map(std::path::Path::to_path_buf),
        git_dir: repo.git_dir().to_path_buf(),
        kind: repo_kind(repo),
        bare: repo.is_bare(),
        status: Status::Clean,
        attrs: Vec::new(),
        dirty: Default::default(),
        ignored: Default::default(),
    };

    inspection.attrs.push(backupcrawl_plugin_sdk::Attr::new(
        "kind",
        inspection.kind.as_str(),
    ));
    if inspection.bare {
        inspection
            .attrs
            .push(backupcrawl_plugin_sdk::Attr::new("bare", "true"));
    }

    let head = match read_head(repo) {
        Ok(head) => head,
        Err(reason) => return unknown(inspection, reason),
    };
    record_head_attrs(&mut inspection.attrs, &head);

    if inspection.bare {
        // A bare repository has no worktree to compare against; its contents
        // are the backup itself, so it is always `clean` (§7.1).
        inspection.status = Status::Clean;
        return inspection;
    }

    let upstream = compute_upstream(repo, &head);
    if let Some(upstream) = &upstream {
        record_upstream_attrs(&mut inspection.attrs, upstream);
    }

    if let Err(reason) = collect_status(repo, opts, &mut inspection) {
        return unknown(inspection, reason);
    }

    finalize(opts, &head, upstream.as_ref(), &mut inspection);
    inspection
}

fn repo_kind(repo: &gix::Repository) -> RepoKind {
    match repo.kind() {
        gix::repository::Kind::Common => RepoKind::Common,
        gix::repository::Kind::LinkedWorkTree => RepoKind::LinkedWorkTree,
        gix::repository::Kind::Submodule => RepoKind::Submodule,
    }
}

fn read_head(repo: &gix::Repository) -> Result<HeadInfo, String> {
    let head = repo
        .head()
        .map_err(|err| reason("failed to read HEAD", &err))?;
    let (kind, name) = if head.is_unborn() {
        (
            HeadKind::Unborn,
            head.referent_name().map(|name| name.shorten().to_string()),
        )
    } else if head.is_detached() {
        (HeadKind::Detached, None)
    } else {
        (
            HeadKind::Branch,
            head.referent_name().map(|name| name.shorten().to_string()),
        )
    };
    Ok(HeadInfo { kind, name })
}

/// Computes ahead/behind against the configured upstream, if any. Errors are
/// swallowed: a missing or broken upstream must not fail the repository status.
fn compute_upstream(repo: &gix::Repository, head: &HeadInfo) -> Option<Upstream> {
    if head.kind != HeadKind::Branch {
        return None;
    }
    let head_name = repo
        .head()
        .ok()
        .and_then(|head| head.referent_name().map(ToOwned::to_owned))?;
    let Some(Ok(tracking)) =
        repo.branch_remote_tracking_ref_name(head_name.as_ref(), gix::remote::Direction::Fetch)
    else {
        return None;
    };

    let head_id = repo.head_id().ok()?.detach();
    let upstream_id = repo
        .find_reference(&tracking)
        .and_then(|mut reference| reference.peel_to_id())
        .ok()?
        .detach();
    let base = repo.merge_base(head_id, upstream_id).ok()??.detach();

    Some(Upstream {
        name: tracking.shorten().to_string(),
        ahead: count_commits(repo, head_id, base),
        behind: count_commits(repo, upstream_id, base),
    })
}

/// `git rev-list --count <tip> ^<base>`.
fn count_commits(
    repo: &gix::Repository,
    tip: gix::hash::ObjectId,
    base: gix::hash::ObjectId,
) -> u64 {
    repo.rev_walk([tip])
        .with_hidden([base])
        .all()
        .map(|walk| walk.filter(|item| item.is_ok()).count() as u64)
        .unwrap_or(0)
}

/// Fills the dirty/ignored path sets from the porcelain-v2-equivalent status.
fn collect_status(
    repo: &gix::Repository,
    opts: &InspectOptions,
    inspection: &mut RepoInspection,
) -> Result<(), String> {
    // gix opens and checksum-verifies `.git/index` itself (mmap through the
    // vendored shim, timestamps through the vendored `filetime`) and falls back
    // to an empty index on an unborn branch.
    let index = repo
        .index_or_empty()
        .map_err(|err| reason("failed to open index", &err))?;

    // Conflicts are not reliably part of the status iterator; read them from
    // the index. Porcelain v2 emits one `u` record per conflicted path, while
    // the index holds up to three stage entries per file.
    for (path, ()) in index.entries_with_paths_by_filter_map(|_path, entry| {
        (entry.stage() != gix::index::entry::Stage::Unconflicted).then_some(())
    }) {
        inspection.dirty.insert(path.to_owned());
    }

    let emit_ignored = opts
        .report_ignored
        .then_some(gix::dir::walk::EmissionMode::Matching);
    let platform = repo
        .status(gix::progress::Discard)
        .map_err(|err| reason("failed to start status", &err))?
        .index(index.into())
        .untracked_files(gix::status::UntrackedFiles::Files)
        .dirwalk_options(|options| options.emit_ignored(emit_ignored));

    let items = platform
        .into_iter(Vec::<BString>::new())
        .map_err(|err| reason("failed to iterate status", &err))?;

    for item in items {
        let item = item.map_err(|err| reason("status iteration failed", &err))?;
        match item {
            gix::status::Item::TreeIndex(change) => {
                use gix::diff::index::Change;
                inspection.dirty.insert(change.location().to_owned());
                if let Change::Rewrite {
                    source_location, ..
                } = &change
                {
                    inspection
                        .dirty
                        .insert(BString::from(source_location.as_ref()));
                }
            }
            gix::status::Item::IndexWorktree(item) => match item {
                gix::status::index_worktree::Item::Modification { rela_path, .. } => {
                    inspection.dirty.insert(rela_path);
                }
                gix::status::index_worktree::Item::DirectoryContents { entry, .. } => {
                    match entry.status {
                        gix::dir::entry::Status::Untracked => {
                            inspection.dirty.insert(entry.rela_path);
                        }
                        gix::dir::entry::Status::Ignored(_) => {
                            inspection.ignored.insert(entry.rela_path);
                        }
                        _ => {}
                    }
                }
                gix::status::index_worktree::Item::Rewrite {
                    source,
                    dirwalk_entry,
                    ..
                } => {
                    inspection.dirty.insert(dirwalk_entry.rela_path);
                    inspection.dirty.insert(source.rela_path().to_owned());
                }
            },
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::reason;

    #[test]
    fn reason_keeps_only_the_first_line() {
        struct MultiLine;
        impl std::fmt::Display for MultiLine {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "outer\n\nCaused by:\n    0: inner")
            }
        }

        assert_eq!(
            reason("failed to iterate status", &MultiLine),
            "failed to iterate status: outer"
        );
    }
}

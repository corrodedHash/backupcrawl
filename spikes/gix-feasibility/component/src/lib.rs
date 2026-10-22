//! M1.7 feasibility spike: gix compiled to a wasm32-wasip2 component.
//!
//! The component inspects one git repository with `gix` and returns a small
//! JSON summary. It is deliberately close to what the M1.8 git plugin needs
//! (open/discover, HEAD state, upstream ahead/behind, porcelain-v2-like
//! status counts) so the plugin can copy these call patterns.
//!
//! JSON schema (stable for the spike):
//! ```json
//! {
//!   "root": "/abs/path/asked/for",
//!   "repo": "/abs/path/of/worktree",   // null for bare repositories
//!   "git_dir": "/abs/path/of/.git",
//!   "kind": "common" | "linked-worktree" | "submodule",
//!   "bare": false,
//!   "head": { "kind": "branch" | "detached" | "unborn", "name": "main", "oid": "…" },
//!   "upstream": { "name": "origin/main", "ahead": 1, "behind": 0, "base": "…" },
//!   "status": { "staged": 0, "unstaged": 1, "renamed": 0, "conflicted": 0,
//!               "untracked": 2, "ignored": 3 },
//!   "dirty": true,
//!   "error": null
//! }
//! ```
//! `upstream` fields are `null` when there is no upstream or no merge base.
//! `error` is set (and the rest of the summary stays best-effort) when the
//! path cannot be opened as a repository or status computation fails.

#[allow(warnings)]
mod bindings;

use bindings::Guest;
use serde::Serialize;

struct Component;

impl Guest for Component {
    fn run(root: String) -> Result<String, String> {
        let summary = inspect(&root);
        serde_json::to_string(&summary).map_err(|err| format!("failed to encode summary: {err}"))
    }
}

bindings::export!(Component with_types_in bindings);

#[derive(Serialize)]
struct Summary {
    root: String,
    repo: Option<String>,
    git_dir: Option<String>,
    kind: Option<String>,
    bare: bool,
    head: HeadInfo,
    upstream: UpstreamInfo,
    status: StatusCounts,
    dirty: bool,
    error: Option<String>,
}

#[derive(Default, Serialize)]
struct HeadInfo {
    /// `branch`, `detached` or `unborn`.
    kind: Option<String>,
    /// Short branch name, if HEAD is symbolic (also set for unborn branches).
    name: Option<String>,
    /// HEAD commit id as hex, if there is one.
    oid: Option<String>,
}

#[derive(Default, Serialize)]
struct UpstreamInfo {
    /// Short name of the remote-tracking reference, e.g. `origin/main`.
    name: Option<String>,
    /// Commits on the local branch not on the upstream; `null` if unknown.
    ahead: Option<u64>,
    /// Commits on the upstream not on the local branch; `null` if unknown.
    behind: Option<u64>,
    /// Merge base used for the counts; `null` if there is no merge base.
    base: Option<String>,
}

#[derive(Default, Serialize)]
struct StatusCounts {
    /// HEAD vs index: added/deleted/modified entries (`1` records in porcelain v2).
    staged: u64,
    /// Index vs worktree: modified entries (`1 .M` records).
    unstaged: u64,
    /// Renames/copies between HEAD and index or index and worktree (`2` records).
    renamed: u64,
    /// Unmerged index entries (`u` records).
    conflicted: u64,
    /// Files not in the index and not ignored (`?` records).
    untracked: u64,
    /// Files matched by ignore rules (`!` records, `--ignored=matching`).
    ignored: u64,
}

impl Summary {
    fn new(root: &str) -> Self {
        Summary {
            root: root.to_owned(),
            repo: None,
            git_dir: None,
            kind: None,
            bare: false,
            head: HeadInfo::default(),
            upstream: UpstreamInfo::default(),
            status: StatusCounts::default(),
            dirty: false,
            error: None,
        }
    }

    fn recompute_dirty(&mut self) {
        let s = &self.status;
        // Mirrors the porcelain v2 rule of §7.1: `1`, `2`, `u` and `?` records are dirty,
        // `!` (ignored) records are not.
        self.dirty = s.staged + s.unstaged + s.renamed + s.conflicted + s.untracked > 0;
    }
}

fn inspect(root: &str) -> Summary {
    let mut summary = Summary::new(root);
    if let Err(err) = inspect_inner(root, &mut summary) {
        summary.error = Some(err);
    }
    summary
}

fn inspect_inner(root: &str, summary: &mut Summary) -> Result<(), String> {
    // WASI exposes no Unix executable bit, so tell gix to ignore `core.fileMode`
    // (the same behavior git uses on filesystems without an exec bit).
    // Otherwise every tracked `100755` file is reported as modified.
    let open_options = gix::open::Options::default().config_overrides(["core.fileMode=false".to_owned()]);
    let repo = gix::discover_opts(root, Default::default(), open_options)
        .map_err(|err| format!("not a git repository: {err:#}"))?;

    summary.repo = repo.workdir().map(display_path);
    summary.git_dir = Some(display_path(repo.git_dir()));
    summary.kind = Some(
        match repo.kind() {
            gix::repository::Kind::Common => "common",
            gix::repository::Kind::LinkedWorkTree => "linked-worktree",
            gix::repository::Kind::Submodule => "submodule",
        }
        .to_owned(),
    );
    summary.bare = repo.is_bare();

    fill_head(&repo, summary)?;
    if summary.bare {
        // A bare repository has no worktree to compare against; its contents are the backup.
        return Ok(());
    }

    fill_upstream(&repo, summary);
    fill_status(&repo, summary)?;
    summary.recompute_dirty();
    Ok(())
}

fn fill_head(repo: &gix::Repository, summary: &mut Summary) -> Result<(), String> {
    let head = repo.head().map_err(|err| format!("failed to read HEAD: {err:#}"))?;
    if head.is_unborn() {
        summary.head.kind = Some("unborn".to_owned());
        summary.head.name = head.referent_name().map(|name| name.shorten().to_string());
    } else if head.is_detached() {
        summary.head.kind = Some("detached".to_owned());
        summary.head.oid = head.id().map(|id| id.detach().to_string());
    } else {
        summary.head.kind = Some("branch".to_owned());
        summary.head.name = head.referent_name().map(|name| name.shorten().to_string());
        summary.head.oid = head.id().map(|id| id.detach().to_string());
    }
    Ok(())
}

/// Fill `upstream` with the configured remote-tracking reference and the
/// ahead/behind counts relative to it. Errors are swallowed on purpose: a
/// missing or broken upstream must not fail the repository status.
fn fill_upstream(repo: &gix::Repository, summary: &mut Summary) {
    let Some(head_name) = repo.head().ok().and_then(|head| head.referent_name().map(ToOwned::to_owned))
    else {
        return;
    };
    let Some(Ok(tracking)) = repo.branch_remote_tracking_ref_name(head_name.as_ref(), gix::remote::Direction::Fetch)
    else {
        return;
    };
    summary.upstream.name = Some(tracking.shorten().to_string());

    let (Some(head_id), Ok(upstream_id)) = (
        summary.head.oid.as_deref().and_then(|hex| hex.parse::<gix::hash::ObjectId>().ok()),
        repo.find_reference(&tracking).and_then(|mut reference| reference.peel_to_id()).map(|id| id.detach()),
    ) else {
        return;
    };

    let Ok(Some(base)) = repo.merge_base(head_id, upstream_id) else {
        return;
    };
    let base = base.detach();
    summary.upstream.base = Some(base.to_string());

    // `rev_walk(tip).with_hidden(base)` yields exactly `git rev-list --count tip ^base`.
    if let Ok(walk) = repo.rev_walk([head_id]).with_hidden([base]).all() {
        summary.upstream.ahead = Some(walk.filter(|item| item.is_ok()).count() as u64);
    }
    if let Ok(walk) = repo.rev_walk([upstream_id]).with_hidden([base]).all() {
        summary.upstream.behind = Some(walk.filter(|item| item.is_ok()).count() as u64);
    }
}

/// Compute porcelain-v2-equivalent status counts:
/// HEAD↔index, index↔worktree, untracked files (not collapsed) and ignored
/// entries (`--ignored=matching`).
fn fill_status(repo: &gix::Repository, summary: &mut Summary) -> Result<(), String> {
    // gix opens and checksum-verifies `.git/index` itself (mmap through the
    // vendored shim, timestamps through the vendored `filetime`), and falls
    // back to an empty index on an unborn branch. The same snapshot is reused
    // by submodule detection inside `status()`, so the index is not read twice.
    let index = repo
        .index_or_empty()
        .map_err(|err| format!("failed to open index: {err:#}"))?;

    // Conflicts are not reliably part of the status iterator; read them from the
    // index, counting distinct paths (porcelain v2 emits one `u` record per path,
    // while the index holds up to three stage entries per conflicted file).
    let mut conflicted_paths = std::collections::BTreeSet::new();
    for (path, ()) in index.entries_with_paths_by_filter_map(|_path, entry| {
        (entry.stage() != gix::index::entry::Stage::Unconflicted).then_some(())
    }) {
        conflicted_paths.insert(path.to_owned());
    }
    summary.status.conflicted = conflicted_paths.len() as u64;

    let platform = repo
        .status(gix::progress::Discard)
        .map_err(|err| format!("failed to start status: {err:#}"))?
        .index(index.into())
        .untracked_files(gix::status::UntrackedFiles::Files)
        .dirwalk_options(|options| options.emit_ignored(Some(gix::dir::walk::EmissionMode::Matching)));

    let items = platform
        .into_iter(Vec::<gix::bstr::BString>::new())
        .map_err(|err| format!("failed to iterate status: {err:#}"))?;

    for item in items {
        let item = item.map_err(|err| format!("status iteration failed: {err:#}"))?;
        match item {
            gix::status::Item::TreeIndex(change) => match change {
                gix::diff::index::Change::Addition { .. }
                | gix::diff::index::Change::Deletion { .. }
                | gix::diff::index::Change::Modification { .. } => summary.status.staged += 1,
                gix::diff::index::Change::Rewrite { .. } => summary.status.renamed += 1,
            },
            gix::status::Item::IndexWorktree(item) => match item {
                gix::status::index_worktree::Item::Modification { .. } => summary.status.unstaged += 1,
                gix::status::index_worktree::Item::DirectoryContents { entry, .. } => match entry.status {
                    gix::dir::entry::Status::Untracked => summary.status.untracked += 1,
                    gix::dir::entry::Status::Ignored(_) => summary.status.ignored += 1,
                    _ => {}
                },
                gix::status::index_worktree::Item::Rewrite { .. } => summary.status.renamed += 1,
            },
        }
    }
    Ok(())
}

fn display_path(path: &std::path::Path) -> String {
    path.display().to_string()
}

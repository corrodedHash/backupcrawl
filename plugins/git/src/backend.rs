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

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use backupcrawl_plugin_sdk::{Attr, Status};
use gix::bstr::{BStr, BString};

use crate::config::{IgnoredMode, NoUpstream};

/// The kind of repository gix opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoKind {
    /// A normal repository with a `.git` directory.
    Common,
    /// A linked worktree (`.git` file pointing into the main git dir).
    LinkedWorkTree,
    /// A submodule worktree.
    Submodule,
}

/// Whether the opened repository is a submodule worktree.
pub fn is_submodule(repo: &gix::Repository) -> bool {
    repo.kind() == gix::repository::Kind::Submodule
}

/// A synthetic `unknown` inspection for a directory that looked like a
/// repository but could not be opened.
pub fn inspection_error(path: &Path, reason: impl Into<String>) -> RepoInspection {
    RepoInspection {
        workdir: Some(path.to_path_buf()),
        git_dir: path.join(".git"),
        kind: RepoKind::Common,
        bare: false,
        status: Status::Unknown,
        attrs: vec![Attr::new("reason", reason.into())],
        dirty: BTreeSet::new(),
        ignored: BTreeSet::new(),
    }
}

impl RepoKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Common => "common",
            Self::LinkedWorkTree => "linked-worktree",
            Self::Submodule => "submodule",
        }
    }
}

/// Everything the detector needs to know about one repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoInspection {
    /// Worktree root, or `None` for a bare repository.
    pub workdir: Option<PathBuf>,
    /// Git directory (`<root>/.git`, a linked worktree's git dir, ...).
    pub git_dir: PathBuf,
    /// Repository kind.
    pub kind: RepoKind,
    /// Whether the repository is bare (its contents are backup data).
    pub bare: bool,
    /// The porcelain-v2 verdict.
    pub status: Status,
    /// Attributes surfaced in `--all` output.
    pub attrs: Vec<Attr>,
    /// Worktree-relative paths that are modified/staged/renamed/conflicted or
    /// untracked.
    pub dirty: BTreeSet<BString>,
    /// Worktree-relative ignored paths (only collected when `report-ignored`).
    pub ignored: BTreeSet<BString>,
}

/// Options the backend needs from the detector config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InspectOptions {
    /// Collect ignored paths at all.
    pub report_ignored: bool,
    /// Granularity of ignored reporting.
    pub ignored_mode: IgnoredMode,
    /// Verdict for a branch without an upstream.
    pub no_upstream: NoUpstream,
}

impl InspectOptions {
    /// Whether `path` (worktree-relative) should be reported `ignored`.
    ///
    /// In [`IgnoredMode::All`] a file is ignored when it *or any of its parent
    /// directories* is in the collected set; in [`IgnoredMode::Matching`] only
    /// exact matches count (ignored directories are pruned before their
    /// contents are ever visited).
    pub fn is_ignored(&self, ignored: &BTreeSet<BString>, path: &BStr) -> bool {
        if !self.report_ignored {
            return false;
        }
        if ignored.contains(path) {
            return true;
        }
        if self.ignored_mode == IgnoredMode::All {
            return parent_paths(path).any(|parent| {
                let parent: &BStr = parent.as_ref();
                ignored.contains(parent)
            });
        }
        false
    }
}

/// Iterates the worktree-relative parent directories of `path`, shortest first.
fn parent_paths(path: &BStr) -> impl Iterator<Item = BString> + '_ {
    let bytes: &[u8] = path.as_ref();
    bytes
        .iter()
        .enumerate()
        .filter(|(_, byte)| **byte == b'/')
        .map(move |(index, _)| BString::from(&bytes[..index]))
}

/// Open options shared by every repository access: WASI has no executable bit.
pub fn open_options() -> gix::open::Options {
    gix::open::Options::default().config_overrides(["core.fileMode=false".to_owned()])
}

/// Opens exactly the repository at `path` (no upward discovery).
pub fn open_at(path: &Path) -> Result<gix::Repository, String> {
    gix::open_opts(path, open_options()).map_err(|err| format!("cannot open repository: {err:#}"))
}

/// Discovers the repository enclosing `path` by searching upwards.
pub fn discover_ancestor(path: &Path) -> Result<gix::Repository, String> {
    gix::discover_opts(path, Default::default(), open_options())
        .map_err(|err| format!("not inside a git repository: {err:#}"))
}

/// Inspects an opened repository. Never fails: an error is folded into a
/// `unknown` verdict with a `reason` attribute, as §7.1 requires.
pub fn inspect(repo: &gix::Repository, opts: &InspectOptions) -> RepoInspection {
    let mut inspection = RepoInspection {
        workdir: repo.workdir().map(Path::to_path_buf),
        git_dir: repo.git_dir().to_path_buf(),
        kind: repo_kind(repo),
        bare: repo.is_bare(),
        status: Status::Clean,
        attrs: Vec::new(),
        dirty: BTreeSet::new(),
        ignored: BTreeSet::new(),
    };

    inspection
        .attrs
        .push(Attr::new("kind", inspection.kind.as_str()));
    if inspection.bare {
        inspection.attrs.push(Attr::new("bare", "true"));
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
        inspection
            .attrs
            .push(Attr::new("upstream", upstream.name.clone()));
        inspection
            .attrs
            .push(Attr::new("ahead", upstream.ahead.to_string()));
        if upstream.behind > 0 {
            inspection
                .attrs
                .push(Attr::new("behind", upstream.behind.to_string()));
        }
    }

    if let Err(reason) = collect_status(repo, opts, &mut inspection) {
        return unknown(inspection, reason);
    }

    inspection.status = decide(opts, &head, upstream.as_ref(), !inspection.dirty.is_empty());
    if inspection.status == Status::Unknown
        && let Some(reason) = unknown_reason(opts, &head, upstream.as_ref())
    {
        inspection.attrs.push(Attr::new("reason", reason));
    }
    inspection
}

fn repo_kind(repo: &gix::Repository) -> RepoKind {
    match repo.kind() {
        gix::repository::Kind::Common => RepoKind::Common,
        gix::repository::Kind::LinkedWorkTree => RepoKind::LinkedWorkTree,
        gix::repository::Kind::Submodule => RepoKind::Submodule,
    }
}

/// Folds an inspection error into an `unknown` verdict.
fn unknown(mut inspection: RepoInspection, reason: String) -> RepoInspection {
    inspection.status = Status::Unknown;
    inspection.attrs.push(Attr::new("reason", reason));
    inspection
}

/// HEAD as far as the detector cares.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HeadInfo {
    kind: HeadKind,
    /// Short branch name, if HEAD is symbolic (also set for unborn branches).
    name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeadKind {
    Branch,
    Detached,
    Unborn,
}

fn read_head(repo: &gix::Repository) -> Result<HeadInfo, String> {
    let head = repo
        .head()
        .map_err(|err| format!("failed to read HEAD: {err:#}"))?;
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

fn record_head_attrs(attrs: &mut Vec<Attr>, head: &HeadInfo) {
    match head.kind {
        HeadKind::Branch => {
            if let Some(name) = &head.name {
                attrs.push(Attr::new("branch", name.clone()));
            }
        }
        HeadKind::Detached => attrs.push(Attr::new("head", "detached")),
        HeadKind::Unborn => attrs.push(Attr::new("head", "unborn")),
    }
}

/// The configured remote-tracking reference and the ahead/behind counts.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Upstream {
    name: String,
    ahead: u64,
    behind: u64,
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
        .map_err(|err| format!("failed to open index: {err:#}"))?;

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
        .map_err(|err| format!("failed to start status: {err:#}"))?
        .index(index.into())
        .untracked_files(gix::status::UntrackedFiles::Files)
        .dirwalk_options(|options| options.emit_ignored(emit_ignored));

    let items = platform
        .into_iter(Vec::<BString>::new())
        .map_err(|err| format!("failed to iterate status: {err:#}"))?;

    for item in items {
        let item = item.map_err(|err| format!("status iteration failed: {err:#}"))?;
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

/// The porcelain-v2 status decision (§7.1).
fn decide(
    opts: &InspectOptions,
    head: &HeadInfo,
    upstream: Option<&Upstream>,
    dirty: bool,
) -> Status {
    if dirty {
        // Dirty wins over everything, including a detached HEAD.
        return Status::Dirty;
    }
    if let Some(upstream) = upstream {
        return if upstream.ahead > 0 {
            Status::Ahead
        } else {
            Status::Clean
        };
    }
    match head.kind {
        // A detached HEAD or unborn branch has no upstream: report `unknown`
        // regardless of `no-upstream` (§7.1).
        HeadKind::Detached | HeadKind::Unborn => Status::Unknown,
        HeadKind::Branch => match opts.no_upstream {
            NoUpstream::Unknown => Status::Unknown,
            NoUpstream::Ahead => Status::Ahead,
            NoUpstream::Clean => Status::Clean,
        },
    }
}

/// The `reason` attribute for an `unknown` verdict, if any.
fn unknown_reason(
    _opts: &InspectOptions,
    head: &HeadInfo,
    upstream: Option<&Upstream>,
) -> Option<String> {
    if upstream.is_some() {
        return None;
    }
    match head.kind {
        HeadKind::Detached => Some("detached-head".to_owned()),
        HeadKind::Unborn => Some("unborn".to_owned()),
        HeadKind::Branch => Some("no-upstream".to_owned()),
    }
}

/// Converts a worktree-relative path to the byte form used by the sets.
pub fn relative_path(workdir: &Path, path: &Path) -> Option<BString> {
    let relative = path.strip_prefix(workdir).ok()?;
    gix::path::into_bstr(relative)
        .ok()
        .map(|bstr| bstr.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(report_ignored: bool, ignored_mode: IgnoredMode) -> InspectOptions {
        InspectOptions {
            report_ignored,
            ignored_mode,
            no_upstream: NoUpstream::Unknown,
        }
    }

    #[test]
    fn is_ignored_matches_exactly_or_by_parent() {
        let mut ignored = BTreeSet::new();
        ignored.insert(BString::from("build"));
        ignored.insert(BString::from("a.log"));

        assert!(opts(true, IgnoredMode::Matching).is_ignored(&ignored, BStr::new("a.log")));
        assert!(!opts(true, IgnoredMode::Matching).is_ignored(&ignored, BStr::new("build/out.o")));
        assert!(opts(true, IgnoredMode::All).is_ignored(&ignored, BStr::new("build/out.o")));
        assert!(opts(true, IgnoredMode::All).is_ignored(&ignored, BStr::new("build/deep/out.o")));
        assert!(!opts(false, IgnoredMode::All).is_ignored(&ignored, BStr::new("build/out.o")));
        assert!(!opts(true, IgnoredMode::All).is_ignored(&ignored, BStr::new("src/main.rs")));
    }

    #[test]
    fn parent_paths_yields_shortest_first() {
        let parents: Vec<String> = parent_paths(BStr::new("a/b/c.txt"))
            .map(|p| p.to_string())
            .collect();
        assert_eq!(parents, ["a", "a/b"]);
    }

    #[test]
    fn decide_prefers_dirty_then_upstream_then_head() {
        let branch = HeadInfo {
            kind: HeadKind::Branch,
            name: Some("main".to_owned()),
        };
        let detached = HeadInfo {
            kind: HeadKind::Detached,
            name: None,
        };
        let unborn = HeadInfo {
            kind: HeadKind::Unborn,
            name: Some("main".to_owned()),
        };
        let upstream = Upstream {
            name: "origin/main".to_owned(),
            ahead: 0,
            behind: 2,
        };
        let ahead = Upstream {
            name: "origin/main".to_owned(),
            ahead: 3,
            behind: 0,
        };

        assert_eq!(
            decide(&opts(true, IgnoredMode::Matching), &branch, None, true),
            Status::Dirty
        );
        assert_eq!(
            decide(
                &opts(true, IgnoredMode::Matching),
                &branch,
                Some(&upstream),
                false
            ),
            Status::Clean
        );
        assert_eq!(
            decide(
                &opts(true, IgnoredMode::Matching),
                &branch,
                Some(&ahead),
                false
            ),
            Status::Ahead
        );
        assert_eq!(
            decide(&opts(true, IgnoredMode::Matching), &detached, None, false),
            Status::Unknown
        );
        assert_eq!(
            decide(&opts(true, IgnoredMode::Matching), &unborn, None, false),
            Status::Unknown
        );
        assert_eq!(
            decide(&opts(true, IgnoredMode::Matching), &detached, None, true),
            Status::Dirty
        );

        let ahead_policy = InspectOptions {
            report_ignored: false,
            ignored_mode: IgnoredMode::Matching,
            no_upstream: NoUpstream::Ahead,
        };
        assert_eq!(decide(&ahead_policy, &branch, None, false), Status::Ahead);
        let clean_policy = InspectOptions {
            no_upstream: NoUpstream::Clean,
            ..ahead_policy
        };
        assert_eq!(decide(&clean_policy, &branch, None, false), Status::Clean);
    }

    #[test]
    fn unknown_reason_names_the_cause() {
        let branch = HeadInfo {
            kind: HeadKind::Branch,
            name: Some("main".to_owned()),
        };
        let detached = HeadInfo {
            kind: HeadKind::Detached,
            name: None,
        };
        let unborn = HeadInfo {
            kind: HeadKind::Unborn,
            name: Some("main".to_owned()),
        };
        let o = opts(true, IgnoredMode::Matching);
        assert_eq!(
            unknown_reason(&o, &branch, None).as_deref(),
            Some("no-upstream")
        );
        assert_eq!(
            unknown_reason(&o, &detached, None).as_deref(),
            Some("detached-head")
        );
        assert_eq!(unknown_reason(&o, &unborn, None).as_deref(), Some("unborn"));
        let upstream = Upstream {
            name: "origin/main".to_owned(),
            ahead: 0,
            behind: 0,
        };
        assert_eq!(unknown_reason(&o, &branch, Some(&upstream)), None);
    }
}

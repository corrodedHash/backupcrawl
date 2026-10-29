//! Backend-neutral inspection model shared by the `gix` and `exec` backends.
//!
//! Both backends answer the same three questions of §3 of
//! `docs/rust-rewrite-plan.md` and produce the same [`RepoInspection`]. The
//! verdict logic ([`decide`]) and the attribute/ignore helpers live here so the
//! two implementations cannot silently drift; only how a repository is read
//! (in-process `gix` vs. `git status --porcelain=v2` through the host `exec`
//! import) differs (M1.9).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use backupcrawl_plugin_sdk::{Attr, Status};
use gix::bstr::{BStr, BString};

use crate::config::{IgnoredMode, NoUpstream};
use crate::reason;

/// The kind of repository that was opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoKind {
    /// A normal repository with a `.git` directory.
    Common,
    /// A linked worktree (`.git` file pointing into the main git dir).
    LinkedWorkTree,
    /// A submodule worktree.
    Submodule,
}

impl RepoKind {
    pub(crate) fn as_str(self) -> &'static str {
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

/// A synthetic `unknown` inspection for a directory that looked like a
/// repository but could not be read.
pub fn inspection_error(path: &Path, reason: impl Into<String>) -> RepoInspection {
    RepoInspection {
        workdir: Some(path.to_path_buf()),
        git_dir: path.join(".git"),
        kind: RepoKind::Common,
        bare: false,
        status: Status::Unknown,
        attrs: vec![Attr::new("reason", reason::one_line(reason.into()))],
        dirty: BTreeSet::new(),
        ignored: BTreeSet::new(),
    }
}

/// Folds an inspection error into an `unknown` verdict.
pub fn unknown(mut inspection: RepoInspection, reason: String) -> RepoInspection {
    inspection.status = Status::Unknown;
    // The report contract is one line per `reason`; backends may hand over a
    // full cause chain (gix multi-line errors), so collapse it here as well.
    inspection
        .attrs
        .push(Attr::new("reason", reason::one_line(reason)));
    inspection
}

/// HEAD as far as the detector cares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadInfo {
    /// Whether HEAD is a branch, detached or unborn.
    pub kind: HeadKind,
    /// Short branch name, if HEAD is symbolic (also set for unborn branches).
    pub name: Option<String>,
}

/// The three HEAD shapes the detector distinguishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadKind {
    /// A symbolic HEAD pointing at a born branch.
    Branch,
    /// A detached HEAD (a raw commit id).
    Detached,
    /// A symbolic HEAD pointing at a branch with no commits yet.
    Unborn,
}

/// Records the HEAD/branch attributes shared by both backends.
pub fn record_head_attrs(attrs: &mut Vec<Attr>, head: &HeadInfo) {
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
pub struct Upstream {
    /// Short name of the remote-tracking reference, e.g. `origin/main`.
    pub name: String,
    /// Commits on HEAD that the upstream does not have.
    pub ahead: u64,
    /// Commits on the upstream that HEAD does not have.
    pub behind: u64,
}

/// Records the upstream/ahead/behind attributes shared by both backends.
pub fn record_upstream_attrs(attrs: &mut Vec<Attr>, upstream: &Upstream) {
    attrs.push(Attr::new("upstream", upstream.name.clone()));
    attrs.push(Attr::new("ahead", upstream.ahead.to_string()));
    if upstream.behind > 0 {
        attrs.push(Attr::new("behind", upstream.behind.to_string()));
    }
}

/// The porcelain-v2 status decision (§7.1).
pub fn decide(
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
pub fn unknown_reason(
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

/// Resolves the `gitdir:` target recorded in a `.git` file (a linked worktree
/// or submodule). Relative targets are resolved against the worktree root.
///
/// Returns `None` when the file has no non-empty `gitdir:` line.
pub fn gitdir_target(worktree: &Path, contents: &str) -> Option<PathBuf> {
    let target = contents
        .lines()
        .find_map(|line| line.strip_prefix("gitdir:"))
        .map(str::trim)
        .filter(|target| !target.is_empty())?;
    Some(if Path::new(target).is_absolute() {
        PathBuf::from(target)
    } else {
        worktree.join(target)
    })
}

/// Applies the shared verdict/attribute logic to a backend's raw findings.
pub fn finalize(
    opts: &InspectOptions,
    head: &HeadInfo,
    upstream: Option<&Upstream>,
    inspection: &mut RepoInspection,
) {
    inspection.status = decide(opts, head, upstream, !inspection.dirty.is_empty());
    if inspection.status == Status::Unknown
        && let Some(reason) = unknown_reason(opts, head, upstream)
    {
        inspection.attrs.push(Attr::new("reason", reason));
    }
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
    fn error_reasons_are_collapsed_to_one_line() {
        let inspection =
            inspection_error(Path::new("/tmp/x"), "failed: boom\nCaused by:\n 0: nope");
        assert_eq!(inspection.status, Status::Unknown);
        assert_eq!(inspection.attrs.len(), 1);
        assert_eq!(inspection.attrs[0].key, "reason");
        assert_eq!(inspection.attrs[0].value, "failed: boom");
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

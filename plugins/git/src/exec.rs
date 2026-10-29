//! The `exec` backend (M1.9): repository status via
//! `git status --porcelain=v2 --branch --untracked-files=all [--ignored=matching]`
//! through the host `exec` import.
//!
//! This is the fallback selected by `backend = "exec"` (§7.1). It exists because
//! the in-process `gix` backend cannot see the Unix executable bit through WASI
//! (`core.fileMode = false` hides exec-bit-only changes) and cannot be
//! interrupted mid-walk; the fallback is accurate and bounded, at the cost of a
//! process per repository and the `exec = ["git"]` grant. It is disabled when
//! the `git` program is missing (probed once in `init`).
//!
//! The parsed findings are converted into the same backend-neutral
//! [`RepoInspection`] as the `gix` backend and passed through the same verdict
//! logic ([`crate::model::finalize`]), so the two backends cannot drift.
//!
//! Reproducing gix's semantics with the CLI needs three deliberate flags:
//!
//! * `--untracked-files=all` — gix uses `UntrackedFiles::Files` (individual
//!   files); the CLI default collapses a fully-untracked directory to `dir/`.
//! * `--ignored=matching` — the same ignored set gix emits with
//!   `EmissionMode::Matching` (ignored directories are one entry, the
//!   `ignored-mode = all` parent rule is applied host-side as for gix).
//! * `-c core.quotePath=false` — keeps common UTF-8 paths literal; C-style
//!   quoting (control characters, `"`, `\`) is still decoded by [`unquote`].

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use backupcrawl_plugin_sdk::{Attr, ExecRequest, Host, Status};
use gix::bstr::BString;

use crate::model::{
    HeadInfo, HeadKind, InspectOptions, RepoInspection, RepoKind, Upstream, finalize,
    gitdir_target, record_head_attrs, record_upstream_attrs, unknown,
};

/// Environment handed to every `git` child.
///
/// The host clears the environment before spawning, so these are the *only*
/// variables git sees: no `GIT_DIR`, no `GIT_WORK_TREE`, no config overrides.
/// `GIT_OPTIONAL_LOCKS=0` stops `status` from taking the index lock (§7.1);
/// `GIT_CONFIG_NOSYSTEM=1` keeps the result independent of `/etc/gitconfig`,
/// matching the env-less wasm host the `gix` backend reads through.
const GIT_ENV: [(&str, &str); 2] = [("GIT_OPTIONAL_LOCKS", "0"), ("GIT_CONFIG_NOSYSTEM", "1")];

/// Probes that the `git` program can be run at all.
///
/// Called once from `init`; a failure disables the plugin for the scan with a
/// visible reason instead of producing a repository `unknown` per repo.
pub fn probe(host: &mut dyn Host) -> Result<(), String> {
    let request = ExecRequest::new("git").arg("--version");
    match host.exec(&request) {
        Ok(result) if result.code == 0 => Ok(()),
        Ok(result) => Err(format!(
            "`git --version` exited with code {}: {}",
            result.code,
            stderr_message(&result.stderr)
        )),
        Err(err) => Err(format!("cannot run `git`: {err}")),
    }
}

/// Discovers the worktree root enclosing `path`
/// (`git rev-parse --show-toplevel`), mirroring `gix::discover`.
pub fn discover_ancestor(path: &Path, host: &mut dyn Host) -> Result<PathBuf, String> {
    let request = with_git_env(
        ExecRequest::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .cwd(path),
    );
    let output = host.exec(&request).map_err(|err| err.to_string())?;
    if output.code != 0 {
        return Err(format!(
            "not inside a worktree: {}",
            stderr_message(&output.stderr)
        ));
    }
    let root = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if root.is_empty() {
        return Err("git rev-parse returned no worktree root".to_owned());
    }
    Ok(PathBuf::from(root))
}

/// Inspects the repository rooted at `path`. Never fails: every error is folded
/// into an `unknown` verdict with a one-line `reason` attribute, as §7.1
/// requires — except that a path which only *looks* like a repository (a stray
/// or incomplete `.git`) is declined, so the walker reports it as a loose path
/// exactly like the `gix` backend does.
///
/// Returns `None` when `path` is not a usable repository. `path` must already
/// look like a repository ([`crate::is_repo_root`]).
pub fn inspect(
    path: &Path,
    opts: &InspectOptions,
    host: &mut dyn Host,
    timeout_ms: Option<u32>,
) -> Option<RepoInspection> {
    let bare = path.join(".git").symlink_metadata().is_err();

    let (kind, git_dir) = if bare {
        (RepoKind::Common, path.to_path_buf())
    } else {
        match git_layout(path) {
            Ok(layout) => layout,
            Err(LayoutError::NotARepository) => return None,
            Err(LayoutError::Failed(reason)) => {
                let mut inspection = RepoInspection {
                    workdir: Some(path.to_path_buf()),
                    git_dir: path.join(".git"),
                    kind: RepoKind::Common,
                    bare: false,
                    status: Status::Clean,
                    attrs: Vec::new(),
                    dirty: BTreeSet::new(),
                    ignored: BTreeSet::new(),
                };
                inspection.attrs.push(Attr::new("kind", "common"));
                return Some(unknown(inspection, reason));
            }
        }
    };

    let mut inspection = RepoInspection {
        workdir: (!bare).then(|| path.to_path_buf()),
        git_dir,
        kind,
        bare,
        status: Status::Clean,
        attrs: Vec::new(),
        dirty: BTreeSet::new(),
        ignored: BTreeSet::new(),
    };
    inspection.attrs.push(Attr::new("kind", kind.as_str()));
    if bare {
        // A bare repository has no worktree to compare against; its contents
        // are the backup itself, so it is always `clean` (§7.1). HEAD is still
        // described so the two backends produce identical attributes.
        inspection.attrs.push(Attr::new("bare", "true"));
        match read_bare_head(path, host) {
            Ok(head) => record_head_attrs(&mut inspection.attrs, &head),
            Err(reason) => return Some(unknown(inspection, reason)),
        }
        return Some(inspection);
    }

    let output = match host.exec(&status_request(path, opts, timeout_ms)) {
        Ok(output) => output,
        Err(err) => {
            return Some(unknown(inspection, format!("git status failed: {err}")));
        }
    };
    if output.code != 0 {
        return Some(unknown(
            inspection,
            format!(
                "git status exited with code {}: {}",
                output.code,
                stderr_message(&output.stderr)
            ),
        ));
    }

    let report = match parse(&output.stdout) {
        Ok(report) => report,
        Err(reason) => return Some(unknown(inspection, reason)),
    };
    record_head_attrs(&mut inspection.attrs, &report.head);
    if let Some(upstream) = &report.upstream {
        record_upstream_attrs(&mut inspection.attrs, upstream);
    }
    inspection.dirty = report.dirty;
    inspection.ignored = report.ignored;
    finalize(
        opts,
        &report.head,
        report.upstream.as_ref(),
        &mut inspection,
    );
    Some(inspection)
}

/// Reads HEAD for a bare repository, where `git status` cannot run.
///
/// `git symbolic-ref -q HEAD` succeeds for a symbolic HEAD (born or unborn) and
/// fails for a detached one; `git rev-parse --verify -q HEAD` then tells a born
/// branch from an unborn one. Together they mirror `gix`'s `repo.head()`.
fn read_bare_head(path: &Path, host: &mut dyn Host) -> Result<HeadInfo, String> {
    let symbolic = with_git_env(
        ExecRequest::new("git")
            .args(["symbolic-ref", "-q", "HEAD"])
            .cwd(path),
    );
    let output = host.exec(&symbolic).map_err(|err| err.to_string())?;
    if output.code != 0 {
        return Ok(HeadInfo {
            kind: HeadKind::Detached,
            name: None,
        });
    }
    let reference = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let name = reference
        .strip_prefix("refs/heads/")
        .map(str::to_owned)
        .or_else(|| reference.rsplit('/').next().map(str::to_owned))
        .filter(|name| !name.is_empty());

    let verify = with_git_env(
        ExecRequest::new("git")
            .args(["rev-parse", "--verify", "-q", "HEAD"])
            .cwd(path),
    );
    let born = host.exec(&verify).is_ok_and(|result| result.code == 0);
    Ok(HeadInfo {
        kind: if born {
            HeadKind::Branch
        } else {
            HeadKind::Unborn
        },
        name,
    })
}

/// Why a path that looks like a repository cannot be inspected.
#[derive(Debug, PartialEq, Eq)]
enum LayoutError {
    /// Not a usable repository: a stray or incomplete `.git` (no `HEAD`) or a
    /// dangling `gitdir:` target. The detector declines the claim.
    NotARepository,
    /// The layout exists but cannot be read (permissions, I/O). The claim is
    /// kept and reported `unknown`.
    Failed(String),
}

/// The worktree/git-dir layout for a non-bare repository.
/// `.git` is a directory for a normal repository and a file for a linked
/// worktree or submodule. The file records `gitdir: <path>`; a git dir under a
/// `modules/` component identifies a submodule, under `worktrees/` a linked
/// worktree — the same distinction `gix::repository::Kind` makes.
fn git_layout(path: &Path) -> Result<(RepoKind, PathBuf), LayoutError> {
    let dotgit = path.join(".git");
    let meta = std::fs::symlink_metadata(&dotgit)
        .map_err(|err| LayoutError::Failed(format!("cannot stat `.git`: {err}")))?;
    if meta.is_dir() {
        ensure_head(&dotgit)?;
        return Ok((RepoKind::Common, dotgit));
    }
    let contents = std::fs::read_to_string(&dotgit)
        .map_err(|err| LayoutError::Failed(format!("cannot read `.git` file: {err}")))?;
    let Some(git_dir) = gitdir_target(path, &contents) else {
        return Err(LayoutError::NotARepository);
    };
    ensure_head(&git_dir)?;
    let kind = if git_dir
        .components()
        .any(|component| component.as_os_str() == "modules")
    {
        RepoKind::Submodule
    } else {
        RepoKind::LinkedWorkTree
    };
    Ok((kind, git_dir))
}

/// Rejects a git dir without `HEAD` (a stray `.git`, a dangling `gitdir:`) the
/// way `gix` reports `Missing HEAD`. A `HEAD` that only lacks permissions is
/// kept: `git status` reports the failure and the repository stays `unknown`.
fn ensure_head(git_dir: &Path) -> Result<(), LayoutError> {
    match std::fs::symlink_metadata(git_dir.join("HEAD")) {
        Ok(_) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => Ok(()),
        Err(_) => Err(LayoutError::NotARepository),
    }
}

/// Builds the `git status` request for one repository.
fn status_request(path: &Path, opts: &InspectOptions, timeout_ms: Option<u32>) -> ExecRequest {
    let mut args: Vec<String> = vec![
        "-c".to_owned(),
        "core.quotePath=false".to_owned(),
        "status".to_owned(),
        "--porcelain=v2".to_owned(),
        "--branch".to_owned(),
        "--untracked-files=all".to_owned(),
    ];
    if opts.report_ignored {
        args.push("--ignored=matching".to_owned());
    }
    let mut request = with_git_env(ExecRequest::new("git").args(args).cwd(path));
    if let Some(ms) = timeout_ms {
        request = request.timeout_ms(ms);
    }
    request
}

/// Adds the fixed `git` environment to a request.
fn with_git_env(mut request: ExecRequest) -> ExecRequest {
    for (key, value) in GIT_ENV {
        request = request.env(key, value);
    }
    request
}

/// Builds the `unknown` reason from a child's stderr, trimmed and single-line.
fn stderr_message(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let text = text.trim();
    if text.is_empty() {
        "(no stderr)".to_owned()
    } else {
        text.lines().next().unwrap_or(text).to_owned()
    }
}

/// What one `git status --porcelain=v2` run tells us.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StatusReport {
    /// HEAD shape and short branch name.
    head: HeadInfo,
    /// Upstream and ahead/behind, when the remote-tracking ref exists.
    upstream: Option<Upstream>,
    /// Worktree-relative modified/staged/renamed/conflicted/untracked paths.
    dirty: BTreeSet<BString>,
    /// Worktree-relative ignored paths (directories without trailing `/`).
    ignored: BTreeSet<BString>,
}

/// Raw branch state before it is shaped into a [`HeadInfo`]/[`Upstream`].
#[derive(Debug, Default)]
struct BranchState {
    unborn: bool,
    detached: bool,
    name: Option<String>,
    upstream: Option<String>,
    ahead: Option<u64>,
    behind: Option<u64>,
}

/// Parses porcelain-v2 output. Pure, so it is unit-tested without git.
fn parse(stdout: &[u8]) -> Result<StatusReport, String> {
    let text = String::from_utf8_lossy(stdout);
    let mut branch = BranchState::default();
    let mut dirty = BTreeSet::new();
    let mut ignored = BTreeSet::new();
    let mut saw_branch_line = false;

    for line in text.lines() {
        if let Some(value) = line.strip_prefix("# branch.oid ") {
            branch.unborn = value == "(initial)";
            saw_branch_line = true;
        } else if let Some(value) = line.strip_prefix("# branch.head ") {
            if value == "(detached)" {
                branch.detached = true;
            } else {
                branch.name = Some(value.to_owned());
            }
            saw_branch_line = true;
        } else if let Some(value) = line.strip_prefix("# branch.upstream ") {
            branch.upstream = Some(value.to_owned());
        } else if let Some(value) = line.strip_prefix("# branch.ab ") {
            let mut parts = value.split_whitespace();
            branch.ahead = parts
                .next()
                .and_then(|value| value.strip_prefix('+'))
                .and_then(|value| value.parse().ok());
            branch.behind = parts
                .next()
                .and_then(|value| value.strip_prefix('-'))
                .and_then(|value| value.parse().ok());
        } else if let Some(rest) = line.strip_prefix("1 ") {
            // 1 XY sub mH mI mW hH hI path
            if let Some(path) = nth_field(rest, 7) {
                dirty.insert(BString::from(unquote(path)));
            }
        } else if let Some(rest) = line.strip_prefix("2 ") {
            // 2 XY sub mH mI mW hH hI Xscore path \t origPath
            if let Some(tail) = nth_field(rest, 8) {
                let mut paths = tail.splitn(2, '\t');
                if let Some(path) = paths.next() {
                    dirty.insert(BString::from(unquote(path)));
                }
                if let Some(original) = paths.next() {
                    dirty.insert(BString::from(unquote(original)));
                }
            }
        } else if let Some(rest) = line.strip_prefix("u ") {
            // u XY sub m1 m2 m3 mW h1 h2 h3 path
            if let Some(path) = nth_field(rest, 9) {
                dirty.insert(BString::from(unquote(path)));
            }
        } else if let Some(rest) = line.strip_prefix("? ") {
            dirty.insert(BString::from(unquote(rest)));
        } else if let Some(rest) = line.strip_prefix("! ") {
            // Ignored directories carry a trailing `/`; the shared ignore
            // lookup compares against paths without it.
            let path = unquote(rest);
            ignored.insert(BString::from(path.strip_suffix('/').unwrap_or(&path)));
        }
    }

    if !saw_branch_line {
        return Err("git status produced no branch header".to_owned());
    }
    let head = if branch.detached {
        HeadInfo {
            kind: HeadKind::Detached,
            name: None,
        }
    } else if branch.unborn {
        HeadInfo {
            kind: HeadKind::Unborn,
            name: branch.name,
        }
    } else {
        HeadInfo {
            kind: HeadKind::Branch,
            name: branch.name,
        }
    };
    // Mirror gix: an upstream only counts when the tracking ref resolves, which
    // `branch.ab` proves. A configured-but-gone upstream (`branch.upstream` with
    // no `branch.ab`) is treated as no upstream.
    let upstream = match (branch.upstream, branch.ahead) {
        (Some(name), Some(ahead)) => Some(Upstream {
            name,
            ahead,
            behind: branch.behind.unwrap_or(0),
        }),
        _ => None,
    };

    Ok(StatusReport {
        head,
        upstream,
        dirty,
        ignored,
    })
}

/// The `index`th space-separated field, with the remainder (which may contain
/// spaces, e.g. a path) kept intact in the final field.
fn nth_field(rest: &str, index: usize) -> Option<&str> {
    rest.splitn(index + 1, ' ').nth(index)
}

/// Decodes git's C-style path quoting (`"a\tb"`). Unquoted paths pass through.
fn unquote(field: &str) -> String {
    let bytes = field.as_bytes();
    if bytes.len() < 2 || bytes[0] != b'"' || bytes[bytes.len() - 1] != b'"' {
        return field.to_owned();
    }
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 1;
    while index < bytes.len() - 1 {
        let byte = bytes[index];
        if byte != b'\\' || index + 1 >= bytes.len() - 1 {
            out.push(byte);
            index += 1;
            continue;
        }
        index += 1;
        match bytes[index] {
            b'a' => out.push(0x07),
            b'b' => out.push(0x08),
            b'f' => out.push(0x0c),
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'v' => out.push(0x0b),
            b'\\' => out.push(b'\\'),
            b'"' => out.push(b'"'),
            digit @ b'0'..=b'7' => {
                let mut value = u32::from(digit - b'0');
                let mut digits = 1;
                while digits < 3
                    && index + 1 < bytes.len() - 1
                    && (b'0'..=b'7').contains(&bytes[index + 1])
                {
                    index += 1;
                    digits += 1;
                    value = value * 8 + u32::from(bytes[index] - b'0');
                }
                out.push(value as u8);
            }
            other => {
                out.push(b'\\');
                out.push(other);
            }
        }
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(stdout: &str) -> StatusReport {
        parse(stdout.as_bytes()).expect("parse")
    }

    #[test]
    fn parses_branch_upstream_and_ahead_behind() {
        let report = run("\
# branch.oid 3aa10534bedcaee1848f55a66955341970608df2
# branch.head main
# branch.upstream origin/main
# branch.ab +2 -1
");
        assert_eq!(report.head.kind, HeadKind::Branch);
        assert_eq!(report.head.name.as_deref(), Some("main"));
        let upstream = report.upstream.expect("upstream");
        assert_eq!(upstream.name, "origin/main");
        assert_eq!(upstream.ahead, 2);
        assert_eq!(upstream.behind, 1);
    }

    #[test]
    fn parses_detached_unborn_and_no_upstream() {
        let detached = run("# branch.oid abc\n# branch.head (detached)\n");
        assert_eq!(detached.head.kind, HeadKind::Detached);
        assert_eq!(detached.upstream, None);

        let unborn = run("# branch.oid (initial)\n# branch.head main\n");
        assert_eq!(unborn.head.kind, HeadKind::Unborn);
        assert_eq!(unborn.head.name.as_deref(), Some("main"));

        let plain = run("# branch.oid abc\n# branch.head main\n");
        assert_eq!(plain.head.kind, HeadKind::Branch);
        assert_eq!(plain.upstream, None);
    }

    #[test]
    fn absent_branch_ab_means_no_upstream() {
        // `branch.upstream` without `branch.ab`: the tracking ref is gone.
        let report = run("# branch.oid abc\n# branch.head main\n# branch.upstream origin/main\n");
        assert_eq!(report.upstream, None);
    }

    #[test]
    fn collects_dirty_and_ignored_paths() {
        let report = run("\
# branch.oid abc
# branch.head main
1 .M N... 100644 100644 100644 a b tracked.txt
2 R. N... 100644 100644 100644 a b R100 new.txt\told.txt
u UU N... 100644 100644 100644 100644 a b c conflict.txt
? dir/keep.txt
! a.log
! build/
! dir/skip.log
");
        let dirty: Vec<String> = report.dirty.iter().map(|p| p.to_string()).collect();
        assert_eq!(
            dirty,
            [
                "conflict.txt",
                "dir/keep.txt",
                "new.txt",
                "old.txt",
                "tracked.txt"
            ]
        );
        let ignored: Vec<String> = report.ignored.iter().map(|p| p.to_string()).collect();
        assert_eq!(ignored, ["a.log", "build", "dir/skip.log"]);
    }

    #[test]
    fn keeps_paths_with_spaces_and_decodes_quoting() {
        let report = run("\
# branch.oid abc
# branch.head main
1 .M N... 100644 100644 100644 a b sp ace.txt
? \"tab\\tname.txt\"
? \"wei\\\"rd.txt\"
");
        let dirty: Vec<String> = report.dirty.iter().map(|p| p.to_string()).collect();
        assert_eq!(dirty, ["sp ace.txt", "tab\tname.txt", "wei\"rd.txt"]);
    }

    #[test]
    fn missing_branch_header_is_an_error() {
        assert!(parse(b"? untracked\n").is_err());
    }

    #[test]
    fn git_layout_distinguishes_the_git_file_kinds() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        std::fs::create_dir_all(root.join("common/.git")).unwrap();
        std::fs::write(root.join("common/.git/HEAD"), "ref: refs/heads/main\n").unwrap();
        assert_eq!(
            git_layout(&root.join("common")).unwrap(),
            (RepoKind::Common, root.join("common/.git"))
        );

        std::fs::create_dir_all(root.join("main/.git/worktrees/wt")).unwrap();
        std::fs::write(
            root.join("main/.git/worktrees/wt/HEAD"),
            "ref: refs/heads/main\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("worktree")).unwrap();
        std::fs::write(
            root.join("worktree/.git"),
            "gitdir: ../main/.git/worktrees/wt\n",
        )
        .unwrap();
        assert_eq!(
            git_layout(&root.join("worktree")).unwrap().0,
            RepoKind::LinkedWorkTree
        );

        std::fs::create_dir_all(root.join(".git/modules/sub")).unwrap();
        std::fs::write(root.join(".git/modules/sub/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::create_dir(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/.git"), "gitdir: ../.git/modules/sub\n").unwrap();
        assert_eq!(
            git_layout(&root.join("sub")).unwrap().0,
            RepoKind::Submodule
        );
    }

    #[test]
    fn git_layout_declines_a_stray_git_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // A `.git` without HEAD is not a repository: the detector declines it
        // instead of spawning `git status` for a guaranteed failure.
        std::fs::create_dir_all(root.join("stray/.git")).unwrap();
        assert_eq!(
            git_layout(&root.join("stray")),
            Err(LayoutError::NotARepository)
        );

        // Same for a `.git` file whose `gitdir:` target has no HEAD.
        std::fs::create_dir_all(root.join("dangling")).unwrap();
        std::fs::write(root.join("dangling/.git"), "gitdir: ../../nowhere\n").unwrap();
        assert_eq!(
            git_layout(&root.join("dangling")),
            Err(LayoutError::NotARepository)
        );

        // A git dir that has a `HEAD` is not declined; `git status` decides
        // whether it is `unknown`.
        std::fs::create_dir_all(root.join("broken/.git")).unwrap();
        std::fs::write(root.join("broken/.git/HEAD"), "ref: refs/heads/main\n").unwrap();
        assert!(git_layout(&root.join("broken")).is_ok());
    }

    #[test]
    fn inspect_declines_a_stray_git_directory() {
        use crate::config::{IgnoredMode, NoUpstream};
        use backupcrawl_plugin_sdk::MockHost;

        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
        let opts = InspectOptions {
            report_ignored: false,
            ignored_mode: IgnoredMode::Matching,
            no_upstream: NoUpstream::Unknown,
        };
        let mut host = MockHost::new();
        assert!(inspect(tmp.path(), &opts, &mut host, None).is_none());
        assert!(
            host.exec_requests().is_empty(),
            "a stray .git must be declined without spawning git"
        );
    }

    #[test]
    fn unquote_round_trips_octal_and_simple_escapes() {
        assert_eq!(unquote("\"a\\tb\""), "a\tb");
        assert_eq!(unquote("\"caf\\303\\251\""), "café");
        assert_eq!(unquote("plain"), "plain");
    }
}

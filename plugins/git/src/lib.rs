//! Bundled git detector plugin.
//!
//! Answers the three questions of §3 of `docs/rust-rewrite-plan.md` for git
//! repositories. The default backend is pure-Rust `gix` (§7.1, M1.7 spike) so
//! no `git` process is spawned and the plugin needs no `exec` capability; the
//! `backend = "exec"` fallback (M1.9) runs `git status --porcelain=v2` through
//! the host `exec` import for exact exec-bit fidelity.
//!
//! * **Managed?** A directory containing a `.git` directory or `.git` file
//!   (linked worktree/submodule), or a bare repository (`HEAD`, `objects/`,
//!   `refs/`). The repository enclosing the crawl root is discovered (`gix` or
//!   `git rev-parse --show-toplevel`) when `ancestor-repos` is on.
//! * **Current?** Porcelain-v2 semantics: any staged/unstaged/renamed/
//!   conflicted/untracked record is `dirty`; a changed submodule counts as
//!   `dirty` too.
//! * **Propagated?** `branch.ab`: `ahead > 0` is `ahead`, otherwise `clean`;
//!   behind is noted as an attribute. A missing upstream follows `no-upstream`
//!   (default `unknown`); a detached HEAD or unborn branch is always `unknown`.
//!
//! One status computation per repository is cached in the instance and served
//! to the file hook, so the walker never triggers per-file work.
//!
//! # Pruning and the file hook
//!
//! By default a repository is claimed as a single entry and **pruned** (the
//! Python behavior, and the cheap case). With `report-ignored = true` the claim
//! does not prune: the walker descends, `.git` is claimed and pruned as
//! metadata, and the file hook reports ignored paths (`ignored`) and the
//! dirty/clean status of every other file from the per-repository cache. This
//! is how the `.gitignore` blind spot of the Python tool is closed (§6/§7.1)
//! without paying for a per-file walk in the common case.
//!
//! # Config
//!
//! `backend` (`"gix"`, the default, or `"exec"`), `no-upstream`,
//! `report-ignored`, `ignored-mode`, `ancestor-repos`, `recurse-submodules`,
//! `timeout-ms`. `timeout-ms` is honored by the `exec` backend (a per-repository
//! process budget); the gix backend cannot be interrupted mid-walk (the
//! `interrupt` feature is off), so there the host's fuel/epoch limits are the
//! real protection.
//!
//! # Backends
//!
//! * **`gix`** (default): in-process, no `exec` grant, WASI reads through a
//!   read-only `/` preopen. Ignores the Unix executable bit
//!   (`core.fileMode = false`), so exec-bit-only changes are invisible.
//! * **`exec`** (M1.9): `git status --porcelain=v2` through the host `exec`
//!   import, disabled when the `git` program is missing. Accurate but spawns a
//!   process per repository; the manifest grants `exec = ["git"]`.

mod backend;
mod config;
mod exec;
mod model;
mod reason;

use std::path::{Path, PathBuf};

use backupcrawl_plugin_sdk::{
    Claim, Detector, DetectorError, Host, LogLevel, ScanInfo, bind_detector,
};

use backend::OpenErrorKind;
use config::{Backend, GitConfig, IgnoredMode};
use model::{InspectOptions, RepoInspection, RepoKind};

/// The git detector's mutable state (one instance per wasm store).
#[derive(Debug, Default)]
pub struct Git {
    /// Validated `[plugins.git]` config.
    config: GitConfig,
    /// Crawl root, used for the ancestor-repository claim.
    root: PathBuf,
    /// Repositories inspected this scan, keyed by worktree root (insertion
    /// order; lookups pick the most specific prefix).
    repos: Vec<RepoInspection>,
    /// Repository enclosing the crawl root, if any (lazy).
    ancestor: Option<RepoInspection>,
    /// Whether ancestor discovery has already run.
    ancestor_checked: bool,
}

impl Git {
    fn inspect_options(&self) -> InspectOptions {
        InspectOptions {
            report_ignored: self.config.report_ignored,
            ignored_mode: self.config.ignored_mode,
            no_upstream: self.config.no_upstream,
        }
    }

    /// The most specific inspected repository whose worktree contains `path`.
    fn repo_for(&self, path: &Path) -> Option<&RepoInspection> {
        self.repos
            .iter()
            .filter(|repo| {
                repo.workdir
                    .as_deref()
                    .is_some_and(|dir| path.starts_with(dir))
            })
            .max_by_key(|repo| {
                repo.workdir
                    .as_ref()
                    .map(|dir| dir.components().count())
                    .unwrap_or(0)
            })
    }

    /// The per-repository `exec` timeout, clamped to the WIT `u32`.
    fn exec_timeout(&self) -> Option<u32> {
        self.config
            .timeout_ms
            .map(|ms| ms.min(u64::from(u32::MAX)) as u32)
    }

    /// Opens (or reuses) the repository rooted at `path` with the configured
    /// backend.
    ///
    /// Returns `None` when the repository should not be claimed: a submodule
    /// with `recurse-submodules = false`, or a path whose `.git` is not a
    /// usable repository (a stray/incomplete `.git` such as `Missing HEAD`).
    /// The latter falls through to the walker, which reports it as a loose
    /// path, instead of producing a bogus `unknown` repository row.
    fn inspect_at(&mut self, path: &Path, host: &mut dyn Host) -> Option<RepoInspection> {
        if let Some(existing) = self
            .repos
            .iter()
            .find(|repo| repo.workdir.as_deref() == Some(path))
        {
            return Some(existing.clone());
        }

        let options = self.inspect_options();
        let inspection = match self.config.backend {
            Backend::Gix => match backend::open_at(path) {
                Ok(repo) => backend::inspect(&repo, &options, host),
                Err(err) => {
                    self.log_open_error(path, &err, host);
                    match err.kind() {
                        OpenErrorKind::NotARepository => return None,
                        OpenErrorKind::Unreadable => model::inspection_error(path, err.reason()),
                    }
                }
            },
            Backend::Exec => exec::inspect(path, &options, host, self.exec_timeout())?,
        };
        if !self.config.recurse_submodules && inspection.kind == RepoKind::Submodule {
            return None;
        }
        self.repos.push(inspection.clone());
        Some(inspection)
    }

    /// Logs a failed open with the full gix cause chain at debug level (`-vv`).
    /// The `reason` attribute only keeps one line, so this is where the detail
    /// stays available.
    fn log_open_error(&self, path: &Path, err: &backend::OpenError, host: &mut dyn Host) {
        let verdict = match err.kind() {
            OpenErrorKind::NotARepository => "declining",
            OpenErrorKind::Unreadable => "cannot open",
        };
        host.log(
            LogLevel::Debug,
            &format!("git: {verdict} {}: {}", path.display(), err.chain()),
        );
    }

    /// Discovers the repository enclosing `path` (used for the ambient claim on
    /// the crawl root).
    fn discover_ancestor(&self, path: &Path, host: &mut dyn Host) -> Option<RepoInspection> {
        let options = self.inspect_options();
        match self.config.backend {
            Backend::Gix => backend::discover_ancestor(path)
                .ok()
                .map(|repo| backend::inspect(&repo, &options, host)),
            Backend::Exec => exec::discover_ancestor(path, host)
                .ok()
                .and_then(|root| exec::inspect(&root, &options, host, self.exec_timeout())),
        }
    }

    /// Turns an inspection into a directory claim. Bare repositories always
    /// prune; otherwise the walker descends only when ignored files are
    /// reported.
    fn claim_for(&self, inspection: &RepoInspection, prune: bool) -> Claim {
        Claim {
            status: inspection.status,
            attrs: inspection.attrs.clone(),
            prune: prune || inspection.bare,
        }
    }
}

impl Detector for Git {
    fn init(&mut self, info: &ScanInfo, host: &mut dyn Host) -> Result<(), DetectorError> {
        self.config = GitConfig::from_config(&info.config)?;
        if self.config.backend == Backend::Exec {
            // The exec fallback is disabled when the `git` program is missing
            // (§7.1): probe once so the scan reports the plugin as disabled
            // instead of turning every repository into `unknown`.
            exec::probe(host).map_err(|reason| {
                DetectorError::new(format!("git backend \"exec\" is unavailable: {reason}"))
            })?;
        }
        self.root = info.root.clone();
        self.repos.clear();
        self.ancestor = None;
        self.ancestor_checked = false;
        host.log(
            LogLevel::Debug,
            &format!(
                "git: backend={} report-ignored={} ignored-mode={:?} ancestor-repos={}",
                self.config.backend.as_str(),
                self.config.report_ignored,
                self.config.ignored_mode,
                self.config.ancestor_repos
            ),
        );
        Ok(())
    }

    fn check_dir(
        &mut self,
        path: &Path,
        host: &mut dyn Host,
    ) -> Result<Option<Claim>, DetectorError> {
        // Git metadata directory: only reached when a repository claim did not
        // prune (report-ignored); it is the mechanism's own data, not user data.
        if is_git_dir(path) {
            return Ok(Some(Claim::clean().attr("kind", "git-dir").prune()));
        }

        if is_repo_root(path) {
            let Some(inspection) = self.inspect_at(path, host) else {
                return Ok(None);
            };
            return Ok(Some(
                self.claim_for(&inspection, !self.config.report_ignored),
            ));
        }

        // Ancestor repository: the crawl root is strictly inside a repo whose
        // root is above it, so the repo root is never visited. Checked lazily
        // (and once) the first time we see the crawl root.
        if self.config.ancestor_repos && path == self.root {
            if !self.ancestor_checked {
                self.ancestor_checked = true;
                let discovered = self.discover_ancestor(path, host);
                self.ancestor = discovered;
            }
            if let Some(ancestor) = &self.ancestor
                && ancestor.workdir.as_deref() != Some(path)
            {
                return Ok(Some(self.claim_for(ancestor, true)));
            }
        }

        // Ignored directories: with `ignored-mode = matching` the directory is
        // reported as one `ignored` entry and pruned; with `all` the walker
        // descends and the file hook reports the contents.
        if self.config.report_ignored && self.config.ignored_mode == IgnoredMode::Matching {
            let options = self.inspect_options();
            if let Some(repo) = self.repo_for(path)
                && let Some(workdir) = repo.workdir.clone()
                && let Some(relative) = model::relative_path(&workdir, path)
                && options.is_ignored(&repo.ignored, relative.as_ref())
            {
                return Ok(Some(Claim::ignored().prune()));
            }
        }

        Ok(None)
    }

    fn check_file(
        &mut self,
        path: &Path,
        _host: &mut dyn Host,
    ) -> Result<Option<Claim>, DetectorError> {
        let options = self.inspect_options();
        let Some(repo) = self.repo_for(path) else {
            return Ok(None);
        };
        let Some(workdir) = repo.workdir.clone() else {
            return Ok(None);
        };
        let Some(relative) = model::relative_path(&workdir, path) else {
            return Ok(None);
        };

        if options.is_ignored(&repo.ignored, relative.as_ref()) {
            return Ok(Some(Claim::ignored()));
        }
        if repo.dirty.contains(&relative) {
            return Ok(Some(Claim::dirty()));
        }
        Ok(Some(Claim::new(repo.status)))
    }
}

// Emits the component export glue for the canonical `plugin` world.
bind_detector!(Git);

/// A git directory: the `.git` directory of a normal repository (or a bare
/// repository's own layout, which `check_dir` checks separately).
fn is_git_dir(path: &Path) -> bool {
    path.file_name().and_then(|name| name.to_str()) == Some(".git") && path.join("HEAD").is_file()
}

/// Whether `path` is a repository root: a `.git` directory or `.git` file, or a
/// bare repository (`HEAD` + `objects/` + `refs/`).
///
/// The checks are deliberately cheap: `check_dir` runs for every directory of
/// the crawl, so a plain subdirectory must not open a repository.
fn is_repo_root(path: &Path) -> bool {
    if path.join(".git").symlink_metadata().is_ok() {
        return true;
    }
    is_bare_layout(path)
}

/// A bare repository's on-disk layout: `HEAD` + `objects/` + `refs/`.
pub(crate) fn is_bare_layout(path: &Path) -> bool {
    path.join("HEAD").is_file() && path.join("objects").is_dir() && path.join("refs").is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;
    use backupcrawl_plugin_sdk::{Attr, Config, ExecResult, MockHost, Status};
    use std::path::PathBuf;

    fn scan_info(attrs: &[(&str, &str)]) -> ScanInfo {
        ScanInfo {
            root: PathBuf::from("/data"),
            user: "tester".to_owned(),
            follow_symlinks: false,
            config: Config::new(
                attrs
                    .iter()
                    .map(|(key, value)| Attr::new(*key, *value))
                    .collect(),
            ),
        }
    }

    fn git_version() -> ExecResult {
        ExecResult {
            code: 0,
            stdout: b"git version 2.56.0\n".to_vec(),
            stderr: Vec::new(),
            timed_out: false,
        }
    }

    #[test]
    fn init_reads_config_and_logs() {
        let mut detector = Git::default();
        let mut host = MockHost::new();

        detector
            .init(
                &scan_info(&[("report-ignored", "true"), ("ancestor-repos", "false")]),
                &mut host,
            )
            .unwrap();

        assert!(detector.config.report_ignored);
        assert!(!detector.config.ancestor_repos);
        assert_eq!(detector.root, PathBuf::from("/data"));
        assert_eq!(host.logs().len(), 1);
    }

    #[test]
    fn init_exec_probes_the_git_program() {
        let mut detector = Git::default();
        let mut host = MockHost::new().with_exec_result(git_version());

        detector
            .init(&scan_info(&[("backend", "exec")]), &mut host)
            .unwrap();

        assert_eq!(detector.config.backend, Backend::Exec);
        let requests = host.exec_requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].program, "git");
        assert_eq!(requests[0].args, ["--version"]);
    }

    #[test]
    fn init_exec_without_git_disables_the_plugin() {
        let mut detector = Git::default();
        let mut host = MockHost::new().with_exec_error("exec: cannot find program `git`");

        let err = detector
            .init(&scan_info(&[("backend", "exec")]), &mut host)
            .unwrap_err();

        assert!(err.message().contains("exec"), "got: {err}");
        assert!(err.message().contains("git"), "got: {err}");
    }

    #[test]
    fn unclaimed_paths_return_none() {
        let mut detector = Git::default();
        let mut host = MockHost::new();
        detector.init(&scan_info(&[]), &mut host).unwrap();

        assert_eq!(
            detector
                .check_file(Path::new("/data/notes.txt"), &mut host)
                .unwrap(),
            None
        );
        assert_eq!(
            detector
                .check_dir(Path::new("/data/plain-dir"), &mut host)
                .unwrap(),
            None
        );
    }

    fn attr<'a>(claim: &'a Claim, key: &str) -> Option<&'a str> {
        claim
            .attrs
            .iter()
            .find(|attr| attr.key == key)
            .map(|attr| attr.value.as_str())
    }

    #[test]
    fn stray_git_directory_is_declined() {
        let tmp = tempfile::tempdir().unwrap();
        // A `.git` directory without `HEAD` is what a half-initialised checkout
        // or a vendored tool leaves behind. It is not a repository, so the
        // detector must not claim it: the walker reports the directory as a
        // loose path instead of a bogus `unknown` repository row.
        std::fs::create_dir_all(tmp.path().join(".git")).unwrap();

        let mut detector = Git::default();
        let mut host = MockHost::new();
        detector.init(&scan_info(&[]), &mut host).unwrap();

        assert_eq!(detector.check_dir(tmp.path(), &mut host).unwrap(), None);
        assert!(
            detector.repos.is_empty(),
            "a declined path must not be cached: {:?}",
            detector.repos
        );
        // The full gix cause chain stays available at debug level.
        assert!(
            host.logs()
                .iter()
                .any(|(level, message)| *level == LogLevel::Debug
                    && message.contains("declining")
                    && message.contains("HEAD")),
            "expected a debug log with the cause chain: {:?}",
            host.logs()
        );
    }

    #[test]
    fn dangling_git_file_is_declined() {
        let tmp = tempfile::tempdir().unwrap();
        // A linked worktree / submodule whose `gitdir:` target is gone.
        std::fs::write(tmp.path().join(".git"), "gitdir: /does/not/exist\n").unwrap();

        let mut detector = Git::default();
        let mut host = MockHost::new();
        detector.init(&scan_info(&[]), &mut host).unwrap();

        assert_eq!(detector.check_dir(tmp.path(), &mut host).unwrap(), None);
        assert!(detector.repos.is_empty());
    }

    #[test]
    fn real_repository_is_still_claimed() {
        let tmp = tempfile::tempdir().unwrap();
        // An empty repository has an unborn branch: it is a real repository, so
        // it stays claimed as `unknown` with the one-line `unborn` reason.
        gix::init(tmp.path()).unwrap();

        let mut detector = Git::default();
        let mut host = MockHost::new();
        detector.init(&scan_info(&[]), &mut host).unwrap();

        let claim = detector
            .check_dir(tmp.path(), &mut host)
            .unwrap()
            .expect("a real repository is claimed");
        assert_eq!(claim.status, Status::Unknown);
        assert_eq!(attr(&claim, "head"), Some("unborn"));
        assert_eq!(attr(&claim, "reason"), Some("unborn"));
        assert_eq!(detector.repos.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_repository_stays_unknown() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        gix::init(tmp.path()).unwrap();
        let git_dir = tmp.path().join(".git");
        std::fs::set_permissions(&git_dir, std::fs::Permissions::from_mode(0o000)).unwrap();

        // Root ignores the mode bits; skip rather than assert a false negative.
        if std::fs::symlink_metadata(git_dir.join("HEAD")).is_ok() {
            std::fs::set_permissions(&git_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
            return;
        }

        let mut detector = Git::default();
        let mut host = MockHost::new();
        detector.init(&scan_info(&[]), &mut host).unwrap();
        let claim = detector.check_dir(tmp.path(), &mut host).unwrap();
        let _ = std::fs::set_permissions(&git_dir, std::fs::Permissions::from_mode(0o755));

        let claim = claim.expect("an unreadable repository is still claimed");
        assert_eq!(claim.status, Status::Unknown);
        let reason = attr(&claim, "reason").expect("unknown carries a reason");
        assert!(
            reason.starts_with("cannot open repository: "),
            "got: {reason}"
        );
        assert!(!reason.contains('\n'), "reason must be one line: {reason}");
    }

    #[test]
    fn detects_repository_roots_cheaply() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // Plain directory.
        std::fs::create_dir(root.join("plain")).unwrap();
        assert!(!is_repo_root(&root.join("plain")));

        // `.git` directory.
        std::fs::create_dir_all(root.join("repo/.git")).unwrap();
        std::fs::write(root.join("repo/.git/HEAD"), "ref: refs/heads/main\n").unwrap();
        assert!(is_repo_root(&root.join("repo")));
        assert!(is_git_dir(&root.join("repo/.git")));

        // `.git` file (linked worktree/submodule).
        std::fs::create_dir(root.join("worktree")).unwrap();
        std::fs::write(root.join("worktree/.git"), "gitdir: /somewhere\n").unwrap();
        assert!(is_repo_root(&root.join("worktree")));

        // Bare repository layout.
        std::fs::create_dir_all(root.join("bare/objects")).unwrap();
        std::fs::create_dir_all(root.join("bare/refs")).unwrap();
        std::fs::write(root.join("bare/HEAD"), "ref: refs/heads/main\n").unwrap();
        assert!(is_repo_root(&root.join("bare")));
    }
}

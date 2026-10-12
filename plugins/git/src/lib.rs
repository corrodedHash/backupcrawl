//! Bundled git detector plugin.
//!
//! Answers the three questions of §3 of `docs/rust-rewrite-plan.md` for git
//! repositories, using the pure-Rust `gix` backend (§7.1, M1.7 spike) so no
//! `git` process is spawned and the plugin needs no `exec` capability:
//!
//! * **Managed?** A directory containing a `.git` directory or `.git` file
//!   (linked worktree/submodule), or a bare repository (`HEAD`, `objects/`,
//!   `refs/`). The repository enclosing the crawl root is discovered with
//!   `gix::discover` when `ancestor-repos` is on.
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
//! `backend` (default `"gix"`; `"exec"` is M1.9 and returns a clear
//! "not implemented yet" error for now), `no-upstream`, `report-ignored`,
//! `ignored-mode`, `ancestor-repos`, `recurse-submodules`, `timeout-ms`.
//! `timeout-ms` is parsed and reserved: the gix backend cannot be interrupted
//! mid-walk (the `interrupt` feature is off), so the host's fuel/epoch limits
//! are the real protection and the M1.9 exec backend will honor the budget.

mod backend;
mod config;

use std::path::{Path, PathBuf};

use backupcrawl_plugin_sdk::{
    Claim, Detector, DetectorError, Host, LogLevel, ScanInfo, bind_detector,
};

use backend::{InspectOptions, RepoInspection};
use config::{Backend, GitConfig, IgnoredMode};

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

    /// Opens (or reuses) the repository rooted at `path`.
    ///
    /// Returns `None` when the repository should not be claimed (a submodule
    /// with `recurse-submodules = false`).
    fn inspect_at(&mut self, path: &Path) -> Option<RepoInspection> {
        if let Some(existing) = self
            .repos
            .iter()
            .find(|repo| repo.workdir.as_deref() == Some(path))
        {
            return Some(existing.clone());
        }

        let options = self.inspect_options();
        let inspection = match backend::open_at(path) {
            Ok(repo) => {
                if !self.config.recurse_submodules && backend::is_submodule(&repo) {
                    return None;
                }
                backend::inspect(&repo, &options)
            }
            Err(reason) => backend::inspection_error(path, reason),
        };
        self.repos.push(inspection.clone());
        Some(inspection)
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
            return Err(DetectorError::new(
                "git backend \"exec\" is not implemented yet (M1.9); use backend = \"gix\"",
            ));
        }
        self.root = info.root.clone();
        self.repos.clear();
        self.ancestor = None;
        self.ancestor_checked = false;
        host.log(
            LogLevel::Debug,
            &format!(
                "git: backend=gix report-ignored={} ignored-mode={:?} ancestor-repos={}",
                self.config.report_ignored, self.config.ignored_mode, self.config.ancestor_repos
            ),
        );
        Ok(())
    }

    fn check_dir(
        &mut self,
        path: &Path,
        _host: &mut dyn Host,
    ) -> Result<Option<Claim>, DetectorError> {
        // Git metadata directory: only reached when a repository claim did not
        // prune (report-ignored); it is the mechanism's own data, not user data.
        if is_git_dir(path) {
            return Ok(Some(Claim::clean().attr("kind", "git-dir").prune()));
        }

        if is_repo_root(path) {
            let Some(inspection) = self.inspect_at(path) else {
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
                self.ancestor = backend::discover_ancestor(path)
                    .ok()
                    .map(|repo| backend::inspect(&repo, &self.inspect_options()));
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
                && let Some(relative) = backend::relative_path(&workdir, path)
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
        let Some(relative) = backend::relative_path(&workdir, path) else {
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
    path.join("HEAD").is_file() && path.join("objects").is_dir() && path.join("refs").is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;
    use backupcrawl_plugin_sdk::{Attr, Config, MockHost};
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
    fn init_rejects_exec_backend_with_a_clear_error() {
        let mut detector = Git::default();
        let mut host = MockHost::new();

        let err = detector
            .init(&scan_info(&[("backend", "exec")]), &mut host)
            .unwrap_err();

        assert!(err.message().contains("exec"), "got: {err}");
        assert!(err.message().contains("not implemented"), "got: {err}");
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

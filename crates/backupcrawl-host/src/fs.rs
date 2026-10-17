//! Read-only filesystem grants (`[capabilities].fs-read`) — M2.0.
//!
//! Until M2.0 the host preopened `/` read-only for every detector and treated
//! `fs-read` as documentation. Since M2.0 the manifest is authoritative: the
//! crawl root is always granted, and every resolved `fs-read` spec adds a
//! read-only preopen **at its own absolute guest path**, so `std::fs` and
//! crates such as `gix` keep working with absolute paths.
//!
//! # Specs
//!
//! A spec is a path with a leading `~` (expanded to the user's home) and
//! shell-style globs (`*`, `?`, `[...]`). Every match must be an existing
//! **directory**; a spec that resolves to nothing is a warning, not a load
//! error (the plugin still loads). Nested roots are deduplicated to the
//! broadest one, so a plugin that declares `/` (git, pacman) yields a single
//! `read /` grant.
//!
//! Two types model this:
//!
//! * [`FsReadScope`] is the resolution of the *declared* specs. It is what
//!   `plugins info` and the trust prompt render, and it carries the unresolved
//!   specs so they can be shown rather than hidden.
//! * [`FsGrants`] is the complete preopen set for one detector instance: the
//!   crawl root plus an [`FsReadScope`], deduplicated to [`FsRoot`] host/guest
//!   pairs. [`Runtime`](crate::runtime::Runtime) builds it at instantiation.

use std::path::{Path, PathBuf};

/// A read-only preopen: an absolute host directory mapped at the same absolute
/// guest path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsRoot {
    /// Absolute host directory made visible to the guest.
    pub host: PathBuf,
    /// Absolute guest path the directory is mounted at (equal to `host`).
    pub guest: PathBuf,
}

impl FsRoot {
    /// A root mapping `path` to itself in the guest.
    fn at(path: PathBuf) -> Self {
        Self {
            guest: path.clone(),
            host: path,
        }
    }
}

/// The resolution of `[capabilities].fs-read` specs.
///
/// `roots` are existing directories, deduplicated to the broadest roots;
/// `unresolved` holds the specs that matched no directory (warnings).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FsReadScope {
    /// Resolved read roots, broadest first (see [`dedup_broadest`]).
    pub roots: Vec<PathBuf>,
    /// Specs that did not resolve to an existing directory.
    pub unresolved: Vec<String>,
}

impl FsReadScope {
    /// Resolves every spec against the filesystem.
    ///
    /// `~` is expanded against the user's home; globs are expanded with the
    /// standard shell rules. Non-directory matches and specs that match nothing
    /// land in [`FsReadScope::unresolved`].
    pub fn resolve(specs: &[String]) -> Self {
        let mut roots = Vec::new();
        let mut unresolved = Vec::new();
        for spec in specs {
            let mut matched_dir = false;
            if let Some(matches) = expand_spec(spec) {
                for path in matches {
                    if path.is_dir() {
                        roots.push(path);
                        matched_dir = true;
                    }
                }
            }
            if !matched_dir {
                unresolved.push(spec.clone());
            }
        }
        Self {
            roots: dedup_broadest(roots),
            unresolved,
        }
    }

    /// Whether the whole filesystem is readable (`/` was resolved).
    pub fn is_whole_filesystem(&self) -> bool {
        self.roots.iter().any(|root| root == Path::new("/"))
    }

    /// A human-readable scope for `plugins info` and the trust prompt.
    ///
    /// A plugin that declares `/` renders as `read /`; a scoped plugin lists
    /// each root; unresolved specs are appended and visibly marked. With no
    /// roots and no specs the result is `(none)`.
    pub fn display(&self) -> String {
        let mut out = if self.roots.is_empty() {
            String::new()
        } else {
            self.roots
                .iter()
                .map(|root| root.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        };
        if !self.unresolved.is_empty() {
            if !out.is_empty() {
                out.push_str("; ");
            }
            out.push_str("unresolved: ");
            out.push_str(&self.unresolved.join(", "));
        }
        if out.is_empty() {
            "(none)".to_owned()
        } else {
            out
        }
    }
}

/// The complete read-only preopen set for one detector instance.
///
/// Built by the runtime from the crawl root plus a resolved [`FsReadScope`].
/// Each root is mapped at its own absolute guest path so guest-side absolute
/// paths resolve; writes are rejected by `FsPerms::ReadOnly`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FsGrants {
    roots: Vec<FsRoot>,
}

impl FsGrants {
    /// No filesystem content at all.
    ///
    /// Used by tests that do not read files; the CLI always grants at least the
    /// crawl root through [`FsGrants::for_scan`].
    pub fn none() -> Self {
        Self::default()
    }

    /// The declared `fs-read` roots alone (no crawl root).
    ///
    /// This is what [`Runtime::instantiate`](crate::runtime::Runtime::instantiate)
    /// grants, keeping the test entry point usable without a crawl context.
    pub fn for_scope(scope: &FsReadScope) -> Self {
        Self::from_paths(scope.roots.iter().cloned())
    }

    /// The crawl root plus the declared `fs-read` roots, deduplicated to the
    /// broadest ones.
    ///
    /// The crawl root is made absolute without resolving symlinks, so guest
    /// paths match the paths the walker hands to detectors.
    pub fn for_scan(crawl_root: &Path, scope: &FsReadScope) -> Self {
        let mut paths: Vec<PathBuf> = Vec::new();
        if let Ok(root) = std::path::absolute(crawl_root) {
            paths.push(root);
        }
        paths.extend(scope.roots.iter().cloned());
        Self::from_paths(paths)
    }

    /// Grants exactly `paths`, deduplicated to the broadest roots.
    ///
    /// Paths are used as given and must already be absolute; this is the
    /// explicit constructor tests use to grant a temp directory.
    pub fn from_roots<I, P>(paths: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        Self::from_paths(paths.into_iter().map(Into::into))
    }

    fn from_paths(paths: impl IntoIterator<Item = PathBuf>) -> Self {
        Self {
            roots: dedup_broadest(paths.into_iter().collect())
                .into_iter()
                .map(FsRoot::at)
                .collect(),
        }
    }

    /// The preopens, in deterministic (broadest-first) order.
    pub fn roots(&self) -> &[FsRoot] {
        &self.roots
    }

    /// Whether no filesystem content is granted.
    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }
}

/// Expands one `fs-read` spec (`~` + globs) into existing path candidates.
///
/// Returns `None` when the spec cannot be expanded at all (no home directory,
/// invalid glob); an empty `Some` means the glob matched nothing.
fn expand_spec(spec: &str) -> Option<Vec<PathBuf>> {
    let expanded = expand_tilde(spec)?;
    // Make the pattern absolute (without resolving symlinks) so the resulting
    // host and guest roots are absolute even for a relative spec.
    let absolute = std::path::absolute(&expanded).ok()?;
    let pattern = absolute.to_str()?;
    match glob::glob(pattern) {
        Ok(paths) => Some(paths.filter_map(Result::ok).collect()),
        Err(_) => None,
    }
}

/// Expands a leading `~` / `~/` to the user's home directory.
///
/// Only the home directory is supported (`~user` is deliberately not); a spec
/// without a leading `~` is returned unchanged.
fn expand_tilde(spec: &str) -> Option<PathBuf> {
    if spec == "~" {
        return dirs::home_dir();
    }
    if let Some(rest) = spec.strip_prefix("~/") {
        return Some(dirs::home_dir()?.join(rest));
    }
    Some(PathBuf::from(spec))
}

/// Deduplicates a root set, dropping every root nested under a broader one.
///
/// Sorting first puts ancestors before their descendants (`/a` < `/a/b`), so a
/// single pass with [`Path::starts_with`] keeps exactly the broadest roots. `/`
/// therefore collapses the set to just `/`.
fn dedup_broadest(mut roots: Vec<PathBuf>) -> Vec<PathBuf> {
    roots.sort();
    roots.dedup();
    let mut kept: Vec<PathBuf> = Vec::new();
    for root in roots {
        if kept.iter().any(|broader| root.starts_with(broader)) {
            continue;
        }
        kept.push(root);
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `$HOME` is process-global; the `~` tests must not race. Setting it in a
    /// test would be unsafe in edition 2024, so the glob/dedup tests use real
    /// temp directories and only `expand_tilde` is exercised indirectly via a
    /// home-relative spec that resolves against the ambient `$HOME`.
    fn dirs(names: &[&str]) -> (tempfile::TempDir, Vec<String>) {
        let tmp = tempfile::tempdir().unwrap();
        let mut paths = Vec::new();
        for name in names {
            let path = tmp.path().join(name);
            std::fs::create_dir_all(&path).unwrap();
            paths.push(path.display().to_string());
        }
        (tmp, paths)
    }

    #[test]
    fn plain_path_resolves_to_an_existing_directory() {
        let (_tmp, paths) = dirs(&["plain"]);
        let scope = FsReadScope::resolve(&paths);

        assert_eq!(scope.roots.len(), 1);
        assert!(scope.roots[0].ends_with("plain"));
        assert!(scope.unresolved.is_empty());
    }

    #[test]
    fn tilde_expansion_resolves_against_home() {
        let home = dirs::home_dir().expect("home directory");
        let name = "backupcrawl-fs-read-test";
        let dir = home.join(name);
        std::fs::create_dir_all(&dir).unwrap();

        let scope = FsReadScope::resolve(&[format!("~/{name}")]);
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(scope.roots, vec![dir], "scope: {scope:?}");
        assert!(scope.unresolved.is_empty());
    }

    #[test]
    fn globs_expand_to_every_matching_directory() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("globs/sync-a")).unwrap();
        std::fs::create_dir_all(tmp.path().join("globs/sync-b")).unwrap();
        std::fs::write(tmp.path().join("globs/file"), b"x").unwrap();

        let scope = FsReadScope::resolve(&[format!("{}/globs/sync*", tmp.path().display())]);

        assert_eq!(scope.roots.len(), 2, "scope: {scope:?}");
        assert!(
            scope
                .roots
                .iter()
                .all(|root| root.ends_with("sync-a") || root.ends_with("sync-b"))
        );
        assert!(scope.roots.iter().all(|root| !root.ends_with("file")));
        assert!(scope.unresolved.is_empty());
    }

    #[test]
    fn nested_roots_dedup_to_the_broadest() {
        let (_tmp, paths) = dirs(&["outer", "outer/inner", "other"]);

        let scope = FsReadScope::resolve(&paths);

        assert_eq!(scope.roots.len(), 2, "scope: {scope:?}");
        assert!(scope.roots[0].ends_with("other") || scope.roots[0].ends_with("outer"));
        assert!(scope.roots.iter().any(|root| root.ends_with("outer")));
        assert!(!scope.roots.iter().any(|root| root.ends_with("inner")));
    }

    #[test]
    fn root_spec_collapses_everything_to_slash() {
        let (_tmp, paths) = dirs(&["anything"]);
        let mut specs = paths;
        specs.push("/".to_owned());

        let scope = FsReadScope::resolve(&specs);

        assert_eq!(scope.roots, vec![PathBuf::from("/")]);
        assert!(scope.is_whole_filesystem());
    }

    #[test]
    fn unresolved_specs_are_warnings_not_failures() {
        let (_tmp, paths) = dirs(&["present"]);
        let mut specs = paths;
        specs.push("/definitely/not/here".to_owned());
        specs.push("/definitely/not/here/*".to_owned());

        let scope = FsReadScope::resolve(&specs);

        assert_eq!(scope.roots.len(), 1);
        assert_eq!(
            scope.unresolved,
            ["/definitely/not/here", "/definitely/not/here/*"]
        );
    }

    #[test]
    fn display_shows_the_real_scope_and_marks_unresolved() {
        let whole = FsReadScope::resolve(&["/".to_owned()]);
        assert_eq!(whole.display(), "/");

        let empty = FsReadScope::resolve(&[]);
        assert_eq!(empty.display(), "(none)");

        let mixed = FsReadScope {
            roots: vec![PathBuf::from("/var/lib/pacman")],
            unresolved: vec!["~/.config/missing".to_owned()],
        };
        assert_eq!(
            mixed.display(),
            "/var/lib/pacman; unresolved: ~/.config/missing"
        );
    }

    #[test]
    fn grants_include_the_crawl_root_and_dedup() {
        let (_tmp, paths) = dirs(&["root", "extra", "extra/nested"]);
        let scope = FsReadScope::resolve(&paths[1..]);
        let grants = FsGrants::for_scan(Path::new(&paths[0]), &scope);

        assert_eq!(grants.roots().len(), 2, "grants: {grants:?}");
        assert!(grants.roots().iter().any(|r| r.host.ends_with("root")));
        assert!(grants.roots().iter().any(|r| r.host.ends_with("extra")));
        // Every root is mounted at its own absolute guest path.
        assert!(grants.roots().iter().all(|r| r.host == r.guest));
    }
}

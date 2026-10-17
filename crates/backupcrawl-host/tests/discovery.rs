//! M1.3 acceptance: fixture-based discovery tests covering precedence, API
//! mismatches, unknown manifest keys and capability errors.

use std::fs;
use std::path::{Path, PathBuf};

use backupcrawl_host::discovery::{PluginSource, SearchPaths, discover};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plugins")
}

/// The committed fixture tree: explicit, user and system sources, plus bad
/// plugins in the explicit source.
fn fixture_paths() -> SearchPaths {
    let root = fixtures();
    SearchPaths {
        explicit: vec![root.join("explicit")],
        user: Some(root.join("user")),
        system: Some(root.join("system")),
        bundled: true,
    }
}

fn write(dir: &Path, manifest: &str) {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join("plugin.toml"), manifest).unwrap();
}

fn write_wasm(path: &Path) {
    fs::write(path, b"\0asm\x01\0\0\0").unwrap();
}

const GIT: &str = r#"
[plugin]
name = "git"
version = "1.0.0"
api = "1"
"#;

#[test]
fn discovers_sources_in_precedence_order() {
    let outcome = discover(&fixture_paths());

    let summary: Vec<(&str, &str, PluginSource)> = outcome
        .plugins
        .iter()
        .map(|plugin| (plugin.name.as_str(), plugin.version(), plugin.source))
        .collect();

    assert_eq!(
        summary,
        vec![
            ("git", "1.0.0", PluginSource::Explicit),
            ("unknown-keys", "0.2.0", PluginSource::Explicit),
            ("pacman", "1.0.0", PluginSource::User),
            ("chezmoi", "0.1.0", PluginSource::System),
            ("syncthing", "0.1.0", PluginSource::Bundled),
        ],
        "explicit/user > system precedence must be stable"
    );

    assert_eq!(
        outcome.errors.len(),
        4,
        "expected the four bad fixtures: {:?}",
        outcome.errors
    );
    let bad_dirs: Vec<&str> = outcome
        .errors
        .iter()
        .map(|error| {
            error
                .dir
                .as_deref()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .unwrap_or_default()
        })
        .collect();
    for expected in ["bad-api", "bad-cap", "broken", "no-wasm"] {
        assert!(
            bad_dirs.contains(&expected),
            "missing {expected}: {bad_dirs:?}"
        );
    }
}

#[test]
fn same_name_later_sources_are_shadowed() {
    let outcome = discover(&fixture_paths());

    // user/git loses to explicit/git; system/pacman loses to user/pacman.
    assert!(outcome.warnings.iter().any(|warning| {
        warning.contains("plugin `git`") && warning.contains("shadowed by explicit")
    }));
    assert!(outcome.warnings.iter().any(|warning| {
        warning.contains("plugin `pacman`") && warning.contains("shadowed by user")
    }));
}

#[test]
fn api_mismatch_is_a_load_error() {
    let outcome = discover(&fixture_paths());

    let error = outcome
        .errors
        .iter()
        .find(|error| {
            error
                .dir
                .as_deref()
                .is_some_and(|dir| dir.ends_with("bad-api"))
        })
        .expect("bad-api must be reported");
    assert_eq!(error.source, PluginSource::Explicit);
    assert!(
        error.message.contains("requires API 2"),
        "got: {}",
        error.message
    );
}

#[test]
fn broken_manifest_is_a_load_error() {
    let outcome = discover(&fixture_paths());

    let error = outcome
        .errors
        .iter()
        .find(|error| {
            error
                .dir
                .as_deref()
                .is_some_and(|dir| dir.ends_with("broken"))
        })
        .expect("broken must be reported");
    assert!(
        error.message.contains("invalid plugin.toml"),
        "got: {}",
        error.message
    );
}

#[test]
fn missing_wasm_sidecar_is_a_load_error() {
    let outcome = discover(&fixture_paths());

    let error = outcome
        .errors
        .iter()
        .find(|error| {
            error
                .dir
                .as_deref()
                .is_some_and(|dir| dir.ends_with("no-wasm"))
        })
        .expect("no-wasm must be reported");
    assert!(
        error.message.contains("no wasm sidecar"),
        "got: {}",
        error.message
    );
}

#[test]
fn unknown_capability_is_a_load_error() {
    let outcome = discover(&fixture_paths());

    let error = outcome
        .errors
        .iter()
        .find(|error| {
            error
                .dir
                .as_deref()
                .is_some_and(|dir| dir.ends_with("bad-cap"))
        })
        .expect("bad-cap must be reported");
    assert!(
        error
            .message
            .contains("unknown required capability for plugin `bad-cap`: sudo"),
        "got: {}",
        error.message
    );
}

#[test]
fn unknown_manifest_keys_surface_as_warnings() {
    let outcome = discover(&fixture_paths());

    let plugin = outcome.get("unknown-keys").expect("unknown-keys found");
    assert!(
        plugin
            .warnings
            .contains(&"unknown manifest key `plugin.homepage`".to_string()),
        "got: {:?}",
        plugin.warnings
    );
    assert!(
        plugin
            .warnings
            .contains(&"unknown manifest key `hooks.files`".to_string()),
        "got: {:?}",
        plugin.warnings
    );
    assert!(outcome.get("git").unwrap().warnings.is_empty());
}

#[test]
fn missing_roots_are_skipped_silently() {
    let outcome = discover(&SearchPaths::explicit_only(vec![
        fixtures().join("does-not-exist"),
    ]));

    assert!(outcome.plugins.is_empty());
    assert!(outcome.errors.is_empty());
    assert!(outcome.warnings.is_empty());
}

#[test]
fn earlier_explicit_directory_wins() {
    let tmp = tempfile::tempdir().unwrap();
    let first = tmp.path().join("first");
    let second = tmp.path().join("second");
    write(&first.join("git"), GIT);
    write_wasm(&first.join("git/git.wasm"));
    write(&second.join("git"), &GIT.replace("1.0.0", "2.0.0"));
    write_wasm(&second.join("git/git.wasm"));

    let outcome = discover(&SearchPaths::explicit_only(vec![first, second]));

    assert_eq!(outcome.plugins.len(), 1);
    assert_eq!(outcome.plugins[0].version(), "1.0.0");
}

#[test]
fn conventional_plugin_wasm_is_resolved() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("git");
    write(&dir, GIT);
    write_wasm(&dir.join("plugin.wasm"));

    let outcome = discover(&SearchPaths::explicit_only(vec![tmp.path().to_path_buf()]));

    assert_eq!(outcome.plugins.len(), 1);
    assert_eq!(
        outcome.plugins[0].wasm_path.as_deref(),
        Some(dir.join("plugin.wasm").as_path())
    );
}

#[test]
fn ambiguous_wasm_sidecars_error() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("git");
    write(&dir, GIT);
    write_wasm(&dir.join("a.wasm"));
    write_wasm(&dir.join("b.wasm"));

    let outcome = discover(&SearchPaths::explicit_only(vec![tmp.path().to_path_buf()]));

    assert!(outcome.plugins.is_empty());
    assert_eq!(outcome.errors.len(), 1);
    assert!(
        outcome.errors[0].message.contains("ambiguous wasm sidecar"),
        "got: {}",
        outcome.errors[0].message
    );
}

#[test]
fn unreadable_plugin_dir_warns_and_continues() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let good = tmp.path().join("good");
    write(&good, GIT);
    write_wasm(&good.join("git.wasm"));

    let locked = tmp.path().join("locked");
    fs::create_dir_all(&locked).unwrap();
    let mut perms = fs::metadata(&locked).unwrap().permissions();
    perms.set_mode(0o000);
    fs::set_permissions(&locked, perms).unwrap();

    let outcome = discover(&SearchPaths::explicit_only(vec![
        locked.clone(),
        tmp.path().to_path_buf(),
    ]));

    // Restore so the tempdir can be cleaned up.
    let mut perms = fs::metadata(&locked).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&locked, perms).unwrap();

    assert!(
        outcome
            .warnings
            .iter()
            .any(|warning| warning.contains("cannot read plugin dir")),
        "got: {:?}",
        outcome.warnings
    );
    // The readable directory still contributes its plugin (git is picked from
    // `good`, the `locked` root only produced a warning).
    assert!(outcome.get("git").is_some());
}

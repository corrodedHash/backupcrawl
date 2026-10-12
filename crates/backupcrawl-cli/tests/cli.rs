//! End-to-end CLI behavior.

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use serde_json::Value;

fn touch(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, b"x").unwrap();
}

fn run_json(root: &Path, extra: &[&std::ffi::OsStr]) -> Value {
    let output = Command::cargo_bin("backupcrawl")
        .unwrap()
        .arg(root)
        .arg("--format")
        .arg("json")
        .args(extra)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn loose_paths(json: &Value) -> Vec<String> {
    json["not_backed_up"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn reports_loose_paths_as_versioned_json() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    touch(&root.join("a.txt"));
    touch(&root.join("sub/b.txt"));

    let json = run_json(root, &[]);

    assert_eq!(json["schema"], 1);
    assert_eq!(json["root"], root.to_str().unwrap());
    let loose = loose_paths(&json);
    assert!(loose.contains(&root.join("a.txt").display().to_string()));
    assert!(loose.contains(&root.join("sub").display().to_string()));
    assert!(json["permission_denied"].as_array().unwrap().is_empty());
    assert!(json["plugins"].as_array().unwrap().is_empty());
}

#[test]
fn console_output_has_panels_without_ansi_when_piped() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    touch(&root.join("a.txt"));

    let output = Command::cargo_bin("backupcrawl")
        .unwrap()
        .arg(root)
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Not backed up"));
    assert!(stdout.contains(&root.join("a.txt").display().to_string()));
    assert!(
        !stdout.contains('\u{1b}'),
        "unexpected ANSI escapes: {stdout:?}"
    );
}

#[test]
fn cli_ignore_flags_build_on_config() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    touch(&root.join("keep.txt"));
    touch(&root.join("skip/a.txt"));
    touch(&root.join("also-skip/b.txt"));

    let config_dir = tempfile::tempdir().unwrap();
    let config = config_dir.path().join("config.toml");
    fs::write(&config, "[ignore]\npaths = [\"*/skip\"]\n").unwrap();

    let json = run_json(
        root,
        &[
            std::ffi::OsStr::new("--config"),
            config.as_os_str(),
            std::ffi::OsStr::new("--ignore"),
            std::ffi::OsStr::new("*/also-skip"),
        ],
    );

    assert_eq!(
        loose_paths(&json),
        vec![root.join("keep.txt").display().to_string()]
    );
}

#[test]
fn invalid_config_exits_2() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    touch(&root.join("a.txt"));

    let config = tmp.path().join("config.toml");
    fs::write(&config, "not valid = [ toml").unwrap();

    Command::cargo_bin("backupcrawl")
        .unwrap()
        .arg(root)
        .arg("--config")
        .arg(&config)
        .assert()
        .code(2)
        .stderr(predicates::str::contains("invalid config file"));
}

#[test]
fn missing_explicit_config_exits_2() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    Command::cargo_bin("backupcrawl")
        .unwrap()
        .arg(root)
        .arg("--config")
        .arg(root.join("does-not-exist.toml"))
        .assert()
        .code(2)
        .stderr(predicates::str::contains("cannot read config file"));
}

#[test]
fn invalid_ignore_pattern_exits_2() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    Command::cargo_bin("backupcrawl")
        .unwrap()
        .arg(root)
        .arg("--ignore")
        .arg("[")
        .assert()
        .code(2)
        .stderr(predicates::str::contains("invalid ignore patterns"));
}

// --- plugins list / info ---------------------------------------------------

/// Fixture plugin directory shipped with the crate.
fn plugins_fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plugins")
}

/// A `backupcrawl` command with an isolated, empty XDG config home so the
/// developer's real `~/.config/backupcrawl/plugins` cannot leak into the test.
fn plugins_command(xdg: &Path) -> Command {
    let mut cmd = Command::cargo_bin("backupcrawl").unwrap();
    cmd.env("XDG_CONFIG_HOME", xdg).env("NO_COLOR", "1");
    cmd
}

/// Extracts the table rows (lines framed by `│`) as whitespace-split cells.
fn table_rows(stdout: &str) -> Vec<Vec<String>> {
    stdout
        .lines()
        .filter(|line| line.starts_with('│'))
        .map(|line| {
            line.trim_matches(|ch| ch == '│' || ch == ' ')
                .split_whitespace()
                .map(str::to_owned)
                .collect()
        })
        .collect()
}

/// Extracts `key: value` fields from an info panel.
fn info_fields(stdout: &str) -> Vec<(String, String)> {
    stdout
        .lines()
        .filter(|line| line.starts_with('│'))
        .filter_map(|line| {
            line.trim_matches(|ch| ch == '│' || ch == ' ')
                .split_once(':')
                .map(|(key, value)| (key.trim().to_owned(), value.trim().to_owned()))
        })
        .collect()
}

#[test]
fn plugins_list_shows_sorted_table_and_load_errors() {
    let xdg = tempfile::tempdir().unwrap();
    let output = plugins_command(xdg.path())
        .args(["plugins", "list", "--plugin-dir"])
        .arg(plugins_fixture_dir())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();

    assert_eq!(
        table_rows(&stdout),
        vec![
            vec!["NAME", "VERSION", "SOURCE", "API", "ENABLED", "TRUST"],
            vec!["alpha", "2.0.0", "explicit", "ok", "yes", "new"],
            vec!["demo", "0.1.0", "explicit", "ok", "yes", "new"],
        ]
    );

    // The bad fixture (api = "3") is reported as a load error below the table.
    assert!(
        stdout.contains("plugin `bad` requires API 3"),
        "got: {stdout}"
    );
    assert!(!stdout.contains('\u{1b}'), "unexpected ANSI escapes");
}

#[test]
fn plugins_list_reflects_enabled_and_trust_config() {
    let xdg = tempfile::tempdir().unwrap();
    let config_dir = xdg.path().join("backupcrawl");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join("config.toml"),
        "[plugins]\nenabled = [\"demo\"]\n\n[plugins.trust]\ndemo = \"sha256:abc\"\n",
    )
    .unwrap();

    let output = plugins_command(xdg.path())
        .args(["plugins", "list", "--plugin-dir"])
        .arg(plugins_fixture_dir())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    assert_eq!(
        table_rows(&stdout),
        vec![
            vec!["NAME", "VERSION", "SOURCE", "API", "ENABLED", "TRUST"],
            vec!["alpha", "2.0.0", "explicit", "ok", "no", "new"],
            vec!["demo", "0.1.0", "explicit", "ok", "yes", "changed"],
        ]
    );
}

#[test]
fn plugins_info_prints_manifest_and_capabilities() {
    let xdg = tempfile::tempdir().unwrap();
    let output = plugins_command(xdg.path())
        .args(["plugins", "info", "--plugin-dir"])
        .arg(plugins_fixture_dir())
        .arg("demo")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Convenience: the whole panel title is the plugin name.
    assert!(stdout.contains("╭─ demo "), "got: {stdout}");

    let fields = info_fields(&stdout);
    let get = |key: &str| {
        fields
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    };

    assert_eq!(get("version"), Some("0.1.0"));
    assert_eq!(get("api"), Some("1 (backupcrawl:detector@1.0.0)"));
    assert_eq!(
        get("description"),
        Some("Demo detector for CLI golden tests")
    );
    assert_eq!(get("license"), Some("MIT"));
    assert_eq!(get("hooks"), Some("dir, file"));
    assert_eq!(get("priority"), Some("5"));
    assert_eq!(get("exec"), Some("demo-tool"));
    assert_eq!(get("fs"), Some("read / (v1 read-only preopen)"));
    assert_eq!(get("fs-read"), Some("~/.config/demo (documented intent)"));
    assert_eq!(get("http"), Some("http://127.0.0.1:9000"));
    assert_eq!(get("cache"), Some("yes"));

    // `source` carries the resolved directory.
    let source = get("source").expect("source field");
    assert!(source.starts_with("explicit ("), "got: {source}");
    assert!(source.ends_with("/demo)"), "got: {source}");
}

#[test]
fn plugins_info_unknown_name_exits_2() {
    let xdg = tempfile::tempdir().unwrap();
    plugins_command(xdg.path())
        .args(["plugins", "info", "--plugin-dir"])
        .arg(plugins_fixture_dir())
        .arg("nope")
        .assert()
        .code(2)
        .stderr(predicates::str::contains("not found"));
}

// --- trust flow ------------------------------------------------------------

/// The `TRUST` cell of the `plugins list` row for `name`.
fn trust_of(stdout: &str, name: &str) -> String {
    table_rows(stdout)
        .into_iter()
        .find(|cells| cells.first().map(String::as_str) == Some(name))
        .map(|cells| cells[5].clone())
        .unwrap_or_else(|| panic!("no row for `{name}` in:\n{stdout}"))
}

fn config_text(xdg: &Path) -> String {
    fs::read_to_string(xdg.join("backupcrawl/config.toml")).unwrap()
}

#[test]
fn plugins_trust_unknown_hash_via_flag_persists_and_round_trips() {
    let xdg = tempfile::tempdir().unwrap();
    let dir = plugins_fixture_dir();

    // Trust on first use: the demo plugin is unknown, so it is not trusted.
    let stdout = plugins_command(xdg.path())
        .args(["plugins", "list", "--plugin-dir"])
        .arg(&dir)
        .output()
        .unwrap();
    let stdout = String::from_utf8(stdout.stdout).unwrap();
    assert_eq!(trust_of(&stdout, "demo"), "new");
    assert_eq!(trust_of(&stdout, "alpha"), "new");
    assert!(
        !xdg.path().join("backupcrawl/config.toml").exists(),
        "an unanswered plugin must not write a config"
    );

    // Non-interactive confirmation via --trust-plugin.
    let out = plugins_command(xdg.path())
        .args(["plugins", "list", "--plugin-dir"])
        .arg(&dir)
        .args(["--trust-plugin", "demo"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(trust_of(&stdout, "demo"), "trusted");
    assert_eq!(trust_of(&stdout, "alpha"), "new");

    // The hash is persisted to the isolated XDG config.
    let config = config_text(xdg.path());
    assert!(config.contains("[plugins.trust]"), "{config}");
    assert!(config.contains("demo = \"sha256:"), "{config}");

    // Round-trip: a later run without the flag still trusts demo.
    let out = plugins_command(xdg.path())
        .args(["plugins", "list", "--plugin-dir"])
        .arg(&dir)
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(trust_of(&stdout, "demo"), "trusted");
}

#[test]
fn plugins_trust_hash_change_reprompts_until_confirmed() {
    let xdg = tempfile::tempdir().unwrap();
    let dir = plugins_fixture_dir();
    let config_dir = xdg.path().join("backupcrawl");
    fs::create_dir_all(&config_dir).unwrap();
    let stale = "sha256:0000000000000000000000000000000000000000000000000000000000000000";
    fs::write(
        config_dir.join("config.toml"),
        format!("[plugins.trust]\ndemo = \"{stale}\"\n"),
    )
    .unwrap();

    // A trusted-but-changed plugin is reported as `changed`; non-interactively
    // nothing is persisted and the stale hash survives.
    let out = plugins_command(xdg.path())
        .args(["plugins", "list", "--plugin-dir"])
        .arg(&dir)
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(trust_of(&stdout, "demo"), "changed");
    assert!(config_text(xdg.path()).contains(stale));

    // Confirming with the flag re-persists the current hash.
    let out = plugins_command(xdg.path())
        .args(["plugins", "list", "--plugin-dir"])
        .arg(&dir)
        .args(["--trust-plugin", "demo"])
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(trust_of(&stdout, "demo"), "trusted");
    let config = config_text(xdg.path());
    assert!(!config.contains(stale), "{config}");
    assert!(config.contains("demo = \"sha256:"), "{config}");

    // Round-trip.
    let out = plugins_command(xdg.path())
        .args(["plugins", "list", "--plugin-dir"])
        .arg(&dir)
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(trust_of(&stdout, "demo"), "trusted");
}

#[test]
fn plugins_info_reports_trust_state_and_hash() {
    let xdg = tempfile::tempdir().unwrap();
    let out = plugins_command(xdg.path())
        .args(["plugins", "info", "--plugin-dir"])
        .arg(plugins_fixture_dir())
        .args(["--trust-plugin", "demo"])
        .arg("demo")
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();

    let fields = info_fields(&stdout);
    let get = |key: &str| {
        fields
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    };
    assert_eq!(get("trust"), Some("trusted"));
    let hash = get("hash").expect("hash field");
    assert!(hash.starts_with("sha256:"), "got: {hash}");

    // And it was persisted for the round-trip.
    assert!(config_text(xdg.path()).contains(&format!("demo = \"{hash}\"")));
}

#[test]
fn plugins_verify_reports_state_and_fails_on_untrusted() {
    let xdg = tempfile::tempdir().unwrap();
    let out = plugins_command(xdg.path())
        .args(["plugins", "verify", "--plugin-dir"])
        .arg(plugins_fixture_dir())
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(2));
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("Plugin trust"), "{stdout}");
    assert!(stdout.contains("sha256:"), "{stdout}");
    // alpha and demo are untrusted; the command must not have written a config.
    assert!(String::from_utf8_lossy(&out.stderr).contains("not trusted"));
    assert!(!xdg.path().join("backupcrawl/config.toml").exists());
}

#[test]
fn plugins_verify_succeeds_when_all_plugins_are_trusted() {
    let xdg = tempfile::tempdir().unwrap();
    let dir = plugins_fixture_dir();

    // Persist both fixtures via the non-interactive flag, then verify offline.
    let out = plugins_command(xdg.path())
        .args(["plugins", "list", "--plugin-dir"])
        .arg(&dir)
        .args(["--trust-plugin", "alpha", "--trust-plugin", "demo"])
        .output()
        .unwrap();
    assert!(out.status.success());

    let out = plugins_command(xdg.path())
        .args(["plugins", "verify", "--plugin-dir"])
        .arg(&dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

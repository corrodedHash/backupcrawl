//! End-to-end CLI behavior.

use std::fs;
use std::path::Path;

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

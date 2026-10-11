//! Walker semantics on fixture trees.

use std::fs;
use std::path::Path;

use backupcrawl_core::ignore::IgnoreMatcher;
use backupcrawl_core::walker;

fn matcher(patterns: &[&str]) -> IgnoreMatcher {
    IgnoreMatcher::new(patterns.iter().copied()).unwrap()
}

fn touch(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, b"x").unwrap();
}

#[test]
fn root_files_are_loose_and_loose_subtrees_collapse() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    touch(&root.join("a.txt"));
    touch(&root.join("sub/b.txt"));
    touch(&root.join("sub/deeper/c.txt"));

    let result = walker::scan(root, &matcher(&[]));

    assert_eq!(
        result.loose_paths,
        vec![root.join("a.txt"), root.join("sub")]
    );
    assert!(result.denied_paths.is_empty());
    assert!(result.plugins.is_empty());
}

#[test]
fn empty_directories_are_invisible() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    touch(&root.join("a.txt"));
    fs::create_dir_all(root.join("empty")).unwrap();

    let result = walker::scan(root, &matcher(&[]));

    assert_eq!(result.loose_paths, vec![root.join("a.txt")]);
}

#[test]
fn ignore_patterns_apply_to_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    touch(&root.join("keep.txt"));
    touch(&root.join("skip/a.txt"));

    let result = walker::scan(root, &matcher(&["*/skip"]));

    assert_eq!(result.loose_paths, vec![root.join("keep.txt")]);
}

#[test]
fn patterns_match_the_full_path_like_fnmatch() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    touch(&root.join("keep.txt"));
    touch(&root.join("nested/skip/a.txt"));

    let result = walker::scan(root, &matcher(&["*nested/skip*"]));

    assert_eq!(result.loose_paths, vec![root.join("keep.txt")]);
}

#[cfg(unix)]
#[test]
fn symlinks_are_skipped_for_now() {
    use std::os::unix::fs::symlink;

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    touch(&root.join("real.txt"));
    symlink(root.join("real.txt"), root.join("link.txt")).unwrap();

    let result = walker::scan(root, &matcher(&[]));

    assert_eq!(result.loose_paths, vec![root.join("real.txt")]);
}

#[cfg(unix)]
#[test]
fn unreadable_paths_are_denied() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    touch(&root.join("fine.txt"));
    touch(&root.join("secret.txt"));
    touch(&root.join("locked/inner.txt"));

    // Root ignores file permissions; detect that and skip.
    let probe = root.join("probe");
    touch(&probe);
    fs::set_permissions(&probe, fs::Permissions::from_mode(0o000)).unwrap();
    let may_bypass = fs::File::open(&probe).is_ok();
    fs::set_permissions(&probe, fs::Permissions::from_mode(0o644)).unwrap();
    if may_bypass {
        return;
    }

    fs::set_permissions(root.join("secret.txt"), fs::Permissions::from_mode(0o000)).unwrap();
    fs::set_permissions(root.join("locked"), fs::Permissions::from_mode(0o000)).unwrap();

    let result = walker::scan(root, &matcher(&[]));

    // Restore permissions before asserting so cleanup can remove the tree.
    fs::set_permissions(root.join("secret.txt"), fs::Permissions::from_mode(0o644)).unwrap();
    fs::set_permissions(root.join("locked"), fs::Permissions::from_mode(0o755)).unwrap();

    let mut loose = result.loose_paths.clone();
    loose.sort();
    let mut expected_loose = vec![root.join("fine.txt"), root.join("probe")];
    expected_loose.sort();
    assert_eq!(loose, expected_loose);

    let mut denied = result.denied_paths.clone();
    denied.sort();
    let mut expected_denied = vec![root.join("locked"), root.join("secret.txt")];
    expected_denied.sort();
    assert_eq!(denied, expected_denied);
}

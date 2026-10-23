//! The filesystem boundary, tested as an attacker would use it.
//!
//! Every case here is a path a model can actually produce. The point of the suite is not that the
//! happy path works (the tool tests cover that) but that the escapes fail — including the one a
//! lexical check cannot see: a symlinked parent directory.

use std::path::PathBuf;

use harness_core::tools::sandbox::Sandbox;

fn workspace() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().canonicalize().expect("canonical workspace");
    (dir, root)
}

#[test]
fn paths_inside_the_workspace_resolve() {
    let (_dir, root) = workspace();
    let sandbox = Sandbox::workspace(root.clone());

    assert_eq!(sandbox.resolve("main.rs").unwrap(), root.join("main.rs"));
    assert_eq!(
        sandbox.resolve("./src/lib.rs").unwrap(),
        root.join("src/lib.rs")
    );
    assert_eq!(
        sandbox.resolve("src/../Cargo.toml").unwrap(),
        root.join("Cargo.toml")
    );
    assert_eq!(sandbox.resolve(".").unwrap(), root);
    // A file that does not exist yet still resolves: `write_file` creates it.
    assert_eq!(
        sandbox.resolve("a/b/c/new.txt").unwrap(),
        root.join("a/b/c/new.txt")
    );
}

#[test]
fn parent_directory_climbing_is_refused() {
    let (_dir, root) = workspace();
    let sandbox = Sandbox::workspace(root);

    for escape in [
        "..",
        "../",
        "../sibling.txt",
        "a/../../outside.txt",
        "../../../../etc/passwd",
    ] {
        let error = sandbox
            .resolve(escape)
            .expect_err(&format!("{escape} must be refused"));
        assert!(
            error.contains("escapes workspace root"),
            "{escape}: {error}"
        );
    }
}

#[test]
fn absolute_paths_outside_the_workspace_are_refused() {
    let (_dir, root) = workspace();
    let sandbox = Sandbox::workspace(root);

    for escape in ["/etc/passwd", "/tmp/luna-escape.txt", "/"] {
        let error = sandbox.resolve(escape).expect_err("must be refused");
        assert!(
            error.contains("escapes workspace root"),
            "{escape}: {error}"
        );
    }
}

#[test]
fn a_symlinked_parent_directory_cannot_smuggle_a_path_out() {
    // `ws/link -> /etc` makes `ws/link/passwd` lexically inside the workspace. Only canonicalizing
    // the existing ancestor catches it, which is why resolution is not lexical.
    let (_dir, root) = workspace();
    let link = root.join("link");
    std::os::unix::fs::symlink("/etc", &link).expect("symlink to /etc");

    let sandbox = Sandbox::workspace(root.clone());
    let error = sandbox
        .resolve("link/passwd")
        .expect_err("a symlinked parent must not escape");
    assert!(error.contains("escapes workspace root"), "{error}");
    assert!(
        error.contains("/etc/passwd"),
        "the message names where it actually points: {error}"
    );

    // The link *name* is inside the workspace, but resolving it lands outside, so it is refused
    // too: a path is where it points, not where it is spelled.
    let link_error = sandbox
        .resolve("link")
        .expect_err("the link points outside");
    assert!(link_error.contains("/etc"), "{link_error}");
    assert!(!sandbox.allows(&PathBuf::from("/etc")));
}

#[test]
fn a_symlinked_parent_that_stays_inside_is_allowed() {
    let (_dir, root) = workspace();
    std::fs::create_dir_all(root.join("real")).expect("real dir");
    std::os::unix::fs::symlink(root.join("real"), root.join("alias")).expect("internal symlink");

    let sandbox = Sandbox::workspace(root.clone());
    let resolved = sandbox
        .resolve("alias/file.txt")
        .expect("inside stays inside");
    assert_eq!(resolved, root.join("real/file.txt"));
}

#[test]
fn a_sibling_directory_sharing_the_root_prefix_is_not_inside() {
    // `/tmp/x/ws-evil` starts with the *string* `/tmp/x/ws`, which a naive `starts_with` on
    // strings would accept. `Path::starts_with` compares components, so it must not.
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("ws");
    std::fs::create_dir_all(&root).expect("workspace dir");
    let sibling = dir.path().join("ws-evil");
    std::fs::create_dir_all(&sibling).expect("sibling dir");

    let sandbox = Sandbox::workspace(root);
    let error = sandbox
        .resolve(sibling.to_str().unwrap())
        .expect_err("a sibling must not be inside");
    assert!(error.contains("escapes workspace root"), "{error}");
}

#[test]
fn tilde_expansion_does_not_become_a_home_directory_hole() {
    let (_dir, root) = workspace();
    let sandbox = Sandbox::workspace(root);

    // `~/…` expands to the real home, which is not the workspace, so it is refused — the
    // expansion happens before the boundary check, never after it.
    let error = sandbox
        .resolve("~/.ssh/id_rsa")
        .expect_err("home is outside the workspace");
    assert!(error.contains("escapes workspace root"), "{error}");
}

#[test]
fn extra_roots_are_honoured_and_nothing_else_widens() {
    let (_dir, root) = workspace();
    let scratch = tempfile::tempdir().expect("scratch");
    let scratch_root = scratch.path().canonicalize().expect("canonical scratch");

    let sandbox = Sandbox::with_extra_roots(root.clone(), vec![scratch_root.clone()]);

    let inside_scratch = scratch_root.join("notes.txt");
    assert_eq!(
        sandbox.resolve(inside_scratch.to_str().unwrap()).unwrap(),
        inside_scratch
    );
    assert_eq!(
        sandbox.resolve("local.txt").unwrap(),
        root.join("local.txt")
    );
    assert!(
        sandbox.resolve("/etc/passwd").is_err(),
        "an extra root does not open the rest of the filesystem"
    );
    assert!(sandbox.resolve("..").is_err());
}

#[test]
fn an_empty_path_is_an_error_not_the_workspace() {
    let (_dir, root) = workspace();
    let sandbox = Sandbox::workspace(root);
    assert!(sandbox.resolve("   ").is_err(), "empty is refused");
}

#[test]
fn the_resolved_path_is_where_the_check_looked() {
    // The check and the write must agree: resolving returns the canonicalized ancestor, so a
    // caller that writes to the returned path cannot be redirected by a symlink after the check.
    let (_dir, root) = workspace();
    std::fs::create_dir_all(root.join("dir")).expect("dir");
    let sandbox = Sandbox::workspace(root.clone());
    let resolved = sandbox.resolve("dir/new.txt").unwrap();
    assert!(resolved.is_absolute(), "resolved paths are absolute");
    assert_eq!(resolved.parent().unwrap(), root.join("dir"));
}

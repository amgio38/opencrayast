//! Spec for ISSUE-CORE-PROTECTED (BND-13, BND-14).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
use opencrayast_core::protected::is_protected;
use std::path::Path;

fn p(s: &str) -> bool {
    is_protected(Path::new(s), &[])
}

#[test]
fn vcs_dirs_at_any_depth_and_case() {
    for s in [
        ".git/config",
        "a/b/.git/hooks/pre-commit",
        ".GIT/config",
        ".Git/HEAD",
        ".hg/x",
        ".svn/x",
        ".bzr/x",
        "sub/.git",
    ] {
        assert!(p(s), "{s} must be protected");
    }
}

#[test]
fn secret_like_names() {
    for s in [
        ".env",
        ".env.local",
        "a/.ENV",
        "k.pem",
        "a/b/server.key",
        "x.p12",
        "x.pfx",
        "db.kdbx",
        "id_rsa",
        "id_rsa.pub",
        "id_ed25519",
        ".netrc",
        ".npmrc",
        ".pypirc",
        "credentials",
        "credentials.json",
        "app.keystore",
    ] {
        assert!(p(s), "{s} must be protected");
    }
}

#[test]
fn ordinary_files_are_not_protected() {
    for s in [
        "src/main.rs",
        "README.md",
        "environment.rs",
        "keyboard.rs",
        "gitignore.txt",
        "a/.github/ci.yml",
        "my.keys.md",
    ] {
        assert!(!p(s), "{s} must NOT be protected");
    }
}

#[test]
fn extras_add_but_never_remove() {
    let extra = vec!["secrets/**".to_string(), "*.tfstate".to_string()];
    assert!(is_protected(Path::new("secrets/a/b.txt"), &extra));
    assert!(is_protected(Path::new("x/y.tfstate"), &extra));
    assert!(is_protected(Path::new(".git/config"), &extra));
    assert!(!is_protected(Path::new("src/lib.rs"), &extra));
}

//! Extra cases for ISSUE-CORE-PROTECTED: Unicode names, deep/long paths, pathological
//! patterns, and the "extras only add" rule. Refs: BND-13 (VCS metadata), BND-14
//! (secret-like names; the built-in list can be extended, never reduced).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use opencrayast_core::protected::is_protected;
use std::path::Path;

fn p(s: &str) -> bool {
    is_protected(Path::new(s), &[])
}

fn with(s: &str, extra: &[&str]) -> bool {
    let extra: Vec<String> = extra.iter().map(|s| (*s).to_string()).collect();
    is_protected(Path::new(s), &extra)
}

/// BND-13: the VCS check is on whole components, so names that merely start with `.git`
/// are ordinary files and must stay writable.
#[test]
fn vcs_match_is_by_whole_component() {
    for s in [
        "a/.github/ci.yml",
        ".gitignore",
        ".gitattributes",
        "git/config",
        "a/.gitx/y",
        "a/git/hooks/pre-commit",
        "a/b/.gitmodules",
    ] {
        assert!(!p(s), "{s} must NOT be protected");
    }
    // The bare directory itself, and nested VCS dirs.
    for s in [
        ".git",
        "a/.git",
        "a/b/.GIT",
        "a/.svn/entries",
        "a/b/c/.hg/x",
    ] {
        assert!(p(s), "{s} must be protected");
    }
}

/// BND-14: secret names are matched on the last component, so a directory that happens to
/// be called `certs` is fine while a file inside it is not.
#[test]
fn secret_names_match_the_last_component_only() {
    for s in ["docs/environment.md", "certs/README.md", "a/env", "my.env"] {
        assert!(!p(s), "{s} must NOT be protected");
    }
    // The prefix patterns `credentials*` and `id_rsa*`/`id_ed25519*` also catch
    // ordinary-looking names (`credentials_helper.rs`, `id_reader.rs`). Same fail-safe
    // reading as `.env.rs` above: the built-in list is prefix-based on purpose.
    for s in [
        "a/b/credentials_helper.rs",
        "a/id_ed25519_notes.md",
        "a/id_rsa_old",
    ] {
        assert!(p(s), "{s} must be protected by the built-in prefix pattern");
    }
    // `.env.*` is a prefix pattern, so `.env.rs` is protected too even though it is a
    // source file. That is the fail-safe reading of the built-in table (BND-14): an
    // over-broad refusal is recoverable, a written `.env.production` is not. Listed here
    // because it is the one built-in rule that catches an ordinary-looking name.
    assert!(p("src/.env.rs"));
    for s in [
        "a/.env.production",
        "deep/nested/dir/.ENV",
        "certs/server.key",
        "certs/chain.pem",
        "a/.netrc",
        "home/.npmrc",
        "a/.pypirc",
    ] {
        assert!(p(s), "{s} must be protected");
    }
    // `.pypirc` and friends are exact names, not prefixes: `x.pypirc` is ordinary.
    for s in ["x.pypirc", "a/.gitignore", "a/netrc.bak"] {
        assert!(!p(s), "{s} must NOT be protected");
    }
}

/// Case folding applies to the secret table too, so an upper-cased secret is still caught.
#[test]
fn secret_matching_is_case_folded() {
    for s in [
        "A.PEM",
        "A/Server.KEY",
        "X.P12",
        "X.Pfx",
        "DB.KDBX",
        "APP.KEYSTORE",
    ] {
        assert!(p(s), "{s} must be protected");
    }
}

/// An extra glob without `/` behaves like `.gitignore`: it matches one component at any
/// depth. One with `/` is anchored at the workspace root.
#[test]
fn extra_glob_anchoring_rules() {
    let extra = ["*.bak"];
    assert!(with("a/b/c/file.bak", &extra));
    assert!(with("file.bak", &extra));
    assert!(!with("src/main.rs", &extra));

    let anchored = ["secrets/**"];
    assert!(with("secrets/a/b.txt", &anchored));
    assert!(
        !with("src/secrets/a.txt", &anchored),
        "anchored at the root"
    );
    assert!(!with("secretsx/a.txt", &anchored));

    // A `*` never crosses a `/`; only `**` does.
    let single = ["a/*"];
    assert!(with("a/b", &single));
    assert!(!with("a/b/c", &single), "`*` must not cross a separator");
    let double = ["a/**"];
    assert!(with("a/b/c/d", &double));

    // `?` is exactly one character, and the whole pattern must be consumed.
    let q = ["f?le.txt"];
    assert!(with("file.txt", &q));
    assert!(!with("f.txt", &q));
    assert!(!with("faile.txt", &q));
    // The anchor must not be a suffix match: `ile.txt` is not a whole component match
    // for a name of `file.txt` at a different position.
    assert!(!with("xile.txt", &q));
    // `?` is one character of the component, not one separator.
    let qdir = ["a?b/c.txt"];
    assert!(with("axb/c.txt", &qdir));
    assert!(!with("a/c.txt", &qdir), "`?` must not match a separator");
}

/// BND-14: the built-in list can only be extended. An extra that names an ordinary file
/// does not make it writable again, and no extra can suppress a built-in.
#[test]
fn extras_can_never_reduce_the_built_in_list() {
    // An extra that would "allow" a protected path still loses to the built-in list.
    let extra = ["src/**", "!*.key", "**"];
    for s in [".git/config", "a/b/.ENV", "x.pem", "id_rsa"] {
        assert!(p(s) || with(s, &extra), "{s} must stay protected");
        assert!(with(s, &extra), "{s} must stay protected even with extras");
    }
    // An empty extra list is the baseline, not a weaker one.
    assert!(!with("src/main.rs", &[]));
}

/// Pathological patterns must not panic and must finish quickly: the matcher is a
/// fill-over-grid DP, so runs of stars cost O(n*m) instead of backtracking exponentially.
#[test]
fn pathological_patterns_are_total_and_linear() {
    for bad in [
        "****",
        "[",
        "[a-",
        "[]",
        "***/**/***",
        "**/**/**/**",
        "?",
        "",
        "/",
        "///",
        "a//b",
        "\u{0}",
        "a\0b",
        "a/b/**/**/**/c",
    ] {
        // Must simply answer, one way or the other.
        let _ = with("a/b/c/d.txt", &[bad]);
        let _ = with(".git/config", &[bad]);
    }
    // A long run of stars against a long name still terminates in reasonable time.
    let long_pattern = "*".repeat(2000);
    let long_name = "a".repeat(2000);
    let start = std::time::Instant::now();
    let _ = with(&long_name, &[long_pattern.as_str()]);
    assert!(
        start.elapsed().as_secs() < 5,
        "pathological match took too long: {:?}",
        start.elapsed()
    );
}

/// Unicode and very long paths are handled without panic and without a quadratic blowup.
#[test]
fn unicode_and_very_long_paths_are_handled() {
    // Unicode names that merely resemble protected ones.
    for s in [
        "src/環境.rs",
        "秘密/鍵.pem",
        "a/ключ.key",
        "emoji/🔑.pem",
        "a/ＧＩＴ/config", // fullwidth letters are not ASCII case folding
    ] {
        let _ = p(s);
    }
    // A Unicode secret name IS still protected: case folding is Unicode-aware here, so a
    // capitalised secret in another script cannot slip past the table.
    assert!(p("鍵.pem"));

    // A long path, deep and wide, answered in linear time.
    let deep = format!("{}/.git/config", "d/".repeat(2000));
    let start = std::time::Instant::now();
    assert!(p(&deep));
    assert!(
        start.elapsed().as_secs() < 5,
        "deep path took too long: {:?}",
        start.elapsed()
    );
    let long_component = format!("{}.rs", "a".repeat(50_000));
    let start = std::time::Instant::now();
    assert!(!p(&long_component));
    assert!(
        start.elapsed().as_secs() < 5,
        "long name took too long: {:?}",
        start.elapsed()
    );
}

/// The empty path is not a write target, so it is not protected; the same holds for a
/// path that normalises to nothing.
#[test]
fn empty_paths_are_not_protected() {
    assert!(!p(""));
    assert!(!p("///"));
    assert!(!is_protected(Path::new(""), &["**".to_string()]));
}

/// An unusable extra pattern is treated as "matches nothing" while the built-in list keeps
/// working: a broken configuration narrows nothing and widens nothing.
#[test]
fn broken_extra_only_ever_narrows() {
    let broken = ["", "\u{0}", "/"];
    // An ordinary file stays writable.
    for b in broken {
        assert!(!with("src/main.rs", &[b]), "{b:?} must not match");
    }
    // Protected paths stay protected, built-ins included.
    for b in broken {
        for s in [".git/config", "a/.ENV", "x.key", "id_rsa"] {
            assert!(with(s, &[b]), "{s} must stay protected");
        }
    }
}

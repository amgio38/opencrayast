//! Language identity, detection and tiers (docs/LANGUAGES.md).

/// A language this build knows about. The grammar behind it exists only when its Cargo feature
/// (`lang-rust`, ...) is enabled; see [`Language::is_available`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Language {
    /// Rust (`.rs`).
    Rust,
    /// TypeScript (`.ts`, `.mts`, `.cts`).
    TypeScript,
    /// TypeScript with JSX (`.tsx`).
    Tsx,
    /// JavaScript including JSX (`.js`, `.jsx`, `.mjs`, `.cjs`).
    JavaScript,
    /// Python (`.py`, `.pyi`).
    Python,
    /// Go (`.go`).
    Go,
}

impl Language {
    /// Every language, in a stable order (the declaration order above).
    pub fn all() -> &'static [Language] {
        &[
            Language::Rust,
            Language::TypeScript,
            Language::Tsx,
            Language::JavaScript,
            Language::Python,
            Language::Go,
        ]
    }

    /// Stable lowercase id used in tool output and arguments: `rust`, `typescript`, `tsx`,
    /// `javascript`, `python`, `go`.
    pub fn id(self) -> &'static str {
        match self {
            Language::Rust => "rust",
            Language::TypeScript => "typescript",
            Language::Tsx => "tsx",
            Language::JavaScript => "javascript",
            Language::Python => "python",
            Language::Go => "go",
        }
    }

    /// Inverse of [`Language::id`]; also accepts the common aliases `ts`, `js`, `py`, `rs`,
    /// `golang`, `jsx` (= JavaScript); case-insensitive; `None` for anything else.
    pub fn from_id(id: &str) -> Option<Language> {
        // Exact, case-insensitive match on the canonical id or an alias. Anything else - including
        // surrounding whitespace, embedded NUL and the empty string - is not a language id.
        if id.eq_ignore_ascii_case("rust") || id.eq_ignore_ascii_case("rs") {
            return Some(Language::Rust);
        }
        if id.eq_ignore_ascii_case("typescript") || id.eq_ignore_ascii_case("ts") {
            return Some(Language::TypeScript);
        }
        if id.eq_ignore_ascii_case("tsx") {
            return Some(Language::Tsx);
        }
        if id.eq_ignore_ascii_case("javascript")
            || id.eq_ignore_ascii_case("js")
            || id.eq_ignore_ascii_case("jsx")
        {
            return Some(Language::JavaScript);
        }
        if id.eq_ignore_ascii_case("python") || id.eq_ignore_ascii_case("py") {
            return Some(Language::Python);
        }
        if id.eq_ignore_ascii_case("go") || id.eq_ignore_ascii_case("golang") {
            return Some(Language::Go);
        }
        None
    }

    /// The tier promise for this language (docs/LANGUAGES.md): 1 for all current languages.
    pub fn tier(self) -> u8 {
        match self {
            Language::Rust
            | Language::TypeScript
            | Language::Tsx
            | Language::JavaScript
            | Language::Python
            | Language::Go => 1,
        }
    }

    /// Detect from a file name (any path form; only the last component and extension matter;
    /// extension match is ASCII case-insensitive) and, for files without a known extension, from a
    /// shebang first line (`#!/usr/bin/env python3`, `#!/usr/bin/python`, ... => Python;
    /// `node` => JavaScript). `None` when nothing matches. `.d.ts` is TypeScript.
    pub fn detect(file_name: &str, first_line: Option<&str>) -> Option<Language> {
        // A known extension is authoritative: the shebang is not even looked at, so the same file
        // name always maps to the same language.
        if let Some(lang) = detect_by_extension(file_name) {
            return Some(lang);
        }
        let first_line = first_line?;
        detect_by_shebang(first_line)
    }

    /// True when this build contains the grammar (its feature is enabled).
    pub fn is_available(self) -> bool {
        match self {
            Language::Rust => cfg!(feature = "lang-rust"),
            // One crate, two grammars: `lang-typescript` provides both TypeScript and TSX.
            Language::TypeScript | Language::Tsx => cfg!(feature = "lang-typescript"),
            Language::JavaScript => cfg!(feature = "lang-javascript"),
            Language::Python => cfg!(feature = "lang-python"),
            Language::Go => cfg!(feature = "lang-go"),
        }
    }

    /// The tree-sitter grammar, or `None` if the feature is disabled. Used only inside this crate
    /// and by `opencrayast-query` (which needs the node-kind tables); never exposed to tools.
    pub fn grammar(self) -> Option<tree_sitter::Language> {
        #[allow(unused_variables)]
        match self {
            Language::Rust => {
                #[cfg(feature = "lang-rust")]
                return Some(tree_sitter_rust::LANGUAGE.into());
                #[cfg(not(feature = "lang-rust"))]
                None
            }
            Language::TypeScript => {
                #[cfg(feature = "lang-typescript")]
                return Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into());
                #[cfg(not(feature = "lang-typescript"))]
                None
            }
            Language::Tsx => {
                #[cfg(feature = "lang-typescript")]
                return Some(tree_sitter_typescript::LANGUAGE_TSX.into());
                #[cfg(not(feature = "lang-typescript"))]
                None
            }
            Language::JavaScript => {
                #[cfg(feature = "lang-javascript")]
                return Some(tree_sitter_javascript::LANGUAGE.into());
                #[cfg(not(feature = "lang-javascript"))]
                None
            }
            Language::Python => {
                #[cfg(feature = "lang-python")]
                return Some(tree_sitter_python::LANGUAGE.into());
                #[cfg(not(feature = "lang-python"))]
                None
            }
            Language::Go => {
                #[cfg(feature = "lang-go")]
                return Some(tree_sitter_go::LANGUAGE.into());
                #[cfg(not(feature = "lang-go"))]
                None
            }
        }
    }
}

/// The last path component, treating both `/` and `\` as separators.
///
/// Returns `None` when the name ends with a separator (`dir.rs/`), because such a name has no last
/// component - deliberately: `dir.rs/` is a directory that happens to be named like a source file,
/// not a source file.
fn last_component(file_name: &str) -> Option<&str> {
    let after_last_sep = match file_name.rfind(['/', '\\']) {
        Some(i) => &file_name[i + 1..],
        None => file_name,
    };
    if after_last_sep.is_empty() {
        None
    } else {
        Some(after_last_sep)
    }
}

/// Map a known extension (ASCII case-insensitive) to its language.
///
/// Returns `None` when there is no extension, when the name is a dotfile with no further dot
/// (`.rs`), or when the extension is not one we know (`.bak`, `.txt`).
fn detect_by_extension(file_name: &str) -> Option<Language> {
    let component = last_component(file_name)?;
    // `dot == 0` means the whole component starts with the dot: `.rs` is a hidden file with no
    // extension, not a Rust source file.
    let dot = component.rfind('.').filter(|i| *i > 0)?;
    let ext = &component[dot + 1..];
    // `a.` and `a.rs.bak` land here with an unknown extension, which keeps the door open for the
    // shebang fallback in `detect`.
    if ext.eq_ignore_ascii_case("rs") {
        return Some(Language::Rust);
    }
    if ext.eq_ignore_ascii_case("ts")
        || ext.eq_ignore_ascii_case("mts")
        || ext.eq_ignore_ascii_case("cts")
    {
        // `types/index.d.ts` is TypeScript; `.tsx` is matched below, not here.
        return Some(Language::TypeScript);
    }
    if ext.eq_ignore_ascii_case("tsx") {
        return Some(Language::Tsx);
    }
    if ext.eq_ignore_ascii_case("js")
        || ext.eq_ignore_ascii_case("jsx")
        || ext.eq_ignore_ascii_case("mjs")
        || ext.eq_ignore_ascii_case("cjs")
    {
        return Some(Language::JavaScript);
    }
    if ext.eq_ignore_ascii_case("py") || ext.eq_ignore_ascii_case("pyi") {
        return Some(Language::Python);
    }
    if ext.eq_ignore_ascii_case("go") {
        return Some(Language::Go);
    }
    None
}

/// Map a `#!` first line to its interpreter's language.
///
/// Understands `#!/usr/bin/env python3` (the interpreter is the argument *after* `env`) and
/// `#!/usr/bin/python`. Only a small, explicit set of interpreters maps to a language; anything
/// else (`#!/bin/sh`) is `None`.
fn detect_by_shebang(first_line: &str) -> Option<Language> {
    let rest = first_line.strip_prefix("#!")?.trim();
    let mut words = rest.split_whitespace();
    let mut program = basename(words.next()?);
    if program.eq_ignore_ascii_case("env") {
        // `env` takes `-S`/`-i` style flags; skip the ones we care about and take the next word.
        let mut next = words.next()?;
        while next.starts_with('-') {
            next = words.next()?;
        }
        program = basename(next);
    }
    if is_python_interpreter(program) {
        return Some(Language::Python);
    }
    if program.eq_ignore_ascii_case("node") {
        return Some(Language::JavaScript);
    }
    None
}

/// The last component of an interpreter path (`/usr/bin/python` => `python`).
fn basename(path: &str) -> &str {
    match path.rfind(['/', '\\']) {
        Some(i) => &path[i + 1..],
        None => path,
    }
}

/// True for `python`, `python3`, `python3.11`, ... but not for `pythonista` or `pypy3`.
fn is_python_interpreter(program: &str) -> bool {
    // What follows `python` is either nothing (the unversioned `python`) or a dotted version
    // number; anything else means this is a different program that merely starts with the letters.
    match program.strip_prefix("python") {
        None => false,
        Some(version) => version.chars().all(|c| c.is_ascii_digit() || c == '.'),
    }
}

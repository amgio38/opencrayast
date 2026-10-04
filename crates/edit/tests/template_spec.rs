//! Spec for ISSUE-EDIT-1, part 2: rewrite templates and overlap resolution. Never weaken.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use opencrayast_core::ErrorCode;
use opencrayast_edit::{ExpandOptions, expand_template, indent_of_line, resolve_overlaps};
use opencrayast_query::pattern::{Capture, CaptureKind};

fn one(name: &str, text: &str) -> Capture {
    Capture {
        name: name.into(),
        kind: CaptureKind::One,
        start_byte: 0,
        end_byte: text.len(),
        text: text.into(),
    }
}
fn list(name: &str, text: &str) -> Capture {
    Capture {
        name: name.into(),
        kind: CaptureKind::List,
        start_byte: 0,
        end_byte: text.len(),
        text: text.into(),
    }
}
fn opts<'a>(
    indent: &'a str,
    le: &'a str,
    verbatim: &'a [std::ops::Range<usize>],
) -> ExpandOptions<'a> {
    ExpandOptions {
        indent,
        line_ending: le,
        verbatim,
        max_output_bytes: 1 << 20,
    }
}
fn x(t: &str, caps: &[Capture]) -> String {
    expand_template(t, caps, &opts("", "\n", &[])).unwrap()
}

#[test]
fn substitution_forms() {
    let caps = [one("A", "a + b"), list("ARGS", "1, 2, 3"), one("_X1", "q")];
    assert_eq!(x("f($A)", &caps), "f(a + b)");
    assert_eq!(x("g($$$ARGS)", &caps), "g(1, 2, 3)");
    assert_eq!(x("$A$_X1", &caps), "a + bq");
    assert_eq!(x("cost: $$5 and $$$$", &caps), "cost: $5 and $$");
    // a '$' that does not start a name is a literal '$'
    assert_eq!(x("$ $1 $a $", &caps), "$ $1 $a $");
    // the name extends as far as it can
    assert_eq!(x("$A_B", &[one("A_B", "ok"), one("A", "no")]), "ok");
    // captured text is never searched for further '$' or re-expanded
    assert_eq!(x("$A", &[one("A", "$B $$ $$$C")]), "$B $$ $$$C");
    assert_eq!(x("", &caps), "");
}

#[test]
fn errors_follow_the_table() {
    let caps = [one("A", "a"), list("L", "x, y")];
    let e = expand_template("f($MISSING)", &caps, &opts("", "\n", &[])).unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidPattern);
    assert!(e.message.contains("$MISSING"), "{}", e.message);
    let e = expand_template("f($L)", &caps, &opts("", "\n", &[])).unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidPattern);
    assert!(
        e.message.contains("$$$L"),
        "says which form to use: {}",
        e.message
    );
    let e = expand_template("f($$$A)", &caps, &opts("", "\n", &[])).unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidPattern);
    let tiny = ExpandOptions {
        indent: "",
        line_ending: "\n",
        verbatim: &[],
        max_output_bytes: 5,
    };
    assert_eq!(
        expand_template("abcdefgh", &[], &tiny).unwrap_err().code,
        ErrorCode::LimitExceeded
    );
    assert_eq!(
        expand_template("$$$L", &[list("L", "123456")], &tiny)
            .unwrap_err()
            .code,
        ErrorCode::LimitExceeded
    );
    assert_eq!(expand_template("abcde", &[], &tiny).unwrap(), "abcde");
}

#[test]
fn layout_reindents_and_converts_line_endings() {
    let t = "if (a) {\n  b();\n\n}\n";
    assert_eq!(
        expand_template(t, &[], &opts("    ", "\n", &[])).unwrap(),
        "if (a) {\n      b();\n\n    }\n"
    );
    assert_eq!(
        expand_template(t, &[], &opts("\t", "\r\n", &[])).unwrap(),
        "if (a) {\r\n\t  b();\r\n\r\n\t}\r\n"
    );
    // \r\n and lone \r in the template are line breaks too
    assert_eq!(
        expand_template("a\r\nb\rc", &[], &opts("  ", "\n", &[])).unwrap(),
        "a\n  b\n  c"
    );
    // the first line is never indented; a blank line gets no trailing whitespace
    assert_eq!(
        expand_template("a\n\nb", &[], &opts("  ", "\n", &[])).unwrap(),
        "a\n\n  b"
    );
}

#[test]
fn captured_text_is_neither_reindented_nor_line_ending_converted() {
    let body = one("BODY", "x();\n    y();");
    let out = expand_template("{\n  $BODY\n}", &[body], &opts("  ", "\r\n", &[])).unwrap();
    assert_eq!(out, "{\r\n    x();\n    y();\r\n  }");
}

#[test]
fn verbatim_ranges_are_copied_exactly() {
    // the template is: s = "a\n b" then a newline, then z
    let t = "s = \"a\n b\"\nz";
    let start = t.find('"').unwrap();
    let end = t.rfind('"').unwrap() + 1;
    #[allow(clippy::single_range_in_vec_init)]
    let v = [start..end];
    let out = expand_template(t, &[], &opts("    ", "\r\n", &v)).unwrap();
    // inside the string: bytes untouched (bare \n, no indent); after it: normalised and indented
    assert_eq!(out, "s = \"a\n b\"\r\n    z");
}

#[test]
fn indent_of_line_finds_the_leading_blanks_of_the_matching_line() {
    let src = "fn a() {\n    let x = 1;\n\t\tlet y = 2;\r\n  z\rw\n";
    assert_eq!(indent_of_line(src, 0), "");
    assert_eq!(indent_of_line(src, src.find("let x").unwrap() + 4), "    ");
    assert_eq!(indent_of_line(src, src.find("let y").unwrap()), "\t\t");
    assert_eq!(indent_of_line(src, src.find('z').unwrap()), "  ");
    assert_eq!(indent_of_line(src, src.find('w').unwrap()), "");
    assert_eq!(indent_of_line(src, 10_000), "", "past the end is clamped");
    assert_eq!(indent_of_line("   ", 3), "   ", "a blank last line");
    // inside a multibyte character: no panic
    assert_eq!(indent_of_line("  é", 3), "  ");
}

#[test]
fn overlap_resolution_keeps_the_outermost() {
    // disjoint and touching
    assert_eq!(
        resolve_overlaps(&[(0, 3), (3, 6), (8, 9)]),
        (vec![0, 1, 2], vec![])
    );
    // nested: outer kept, inner dropped, whatever the input order
    assert_eq!(
        resolve_overlaps(&[(2, 4), (0, 10), (5, 6)]),
        (vec![1], vec![0, 2])
    );
    // equal spans: the lower index wins
    assert_eq!(resolve_overlaps(&[(1, 5), (1, 5)]), (vec![0], vec![1]));
    // partial overlap: the one that starts first wins
    assert_eq!(resolve_overlaps(&[(4, 9), (0, 5)]), (vec![1], vec![0]));
    // empty spans
    assert_eq!(resolve_overlaps(&[(0, 10), (5, 5)]), (vec![0], vec![1]));
    assert_eq!(
        resolve_overlaps(&[(0, 10), (10, 10), (0, 0)]),
        (vec![0, 1, 2], vec![])
    );
    assert_eq!(resolve_overlaps(&[]), (vec![], vec![]));
}

struct Lcg(u64);
impl Lcg {
    fn below(&mut self, n: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % n.max(1)
    }
}

#[test]
fn overlap_resolution_properties() {
    let mut r = Lcg(7);
    for _ in 0..3000 {
        let n = r.below(9);
        let spans: Vec<(usize, usize)> = (0..n)
            .map(|_| {
                let a = r.below(20);
                (a, a + r.below(8))
            })
            .collect();
        let (kept, dropped) = resolve_overlaps(&spans);
        // partition, ascending
        let mut all: Vec<usize> = kept.iter().chain(dropped.iter()).copied().collect();
        all.sort_unstable();
        assert_eq!(all, (0..n).collect::<Vec<_>>(), "{spans:?}");
        assert!(
            kept.windows(2).all(|w| w[0] < w[1]) && dropped.windows(2).all(|w| w[0] < w[1]),
            "{spans:?}"
        );
        let overlap = |a: (usize, usize), b: (usize, usize)| {
            if a.0 == a.1 || b.0 == b.1 {
                // an empty span overlaps a span only if strictly inside it
                (b.0 < a.0 && a.0 < b.1) || (a.0 < b.0 && b.0 < a.1)
            } else {
                a.0 < b.1 && b.0 < a.1
            }
        };
        for (i, &a) in kept.iter().enumerate() {
            for &b in &kept[i + 1..] {
                assert!(
                    !overlap(spans[a], spans[b]),
                    "kept overlap {spans:?} {kept:?}"
                );
            }
        }
        for &d in &dropped {
            assert!(
                kept.iter()
                    .any(|&k| overlap(spans[k], spans[d]) || spans[k] == spans[d]),
                "dropped without a reason {spans:?} {kept:?} {d}"
            );
        }
    }
}

/// `verbatim` is about layout only: `$NAME`, `$$$NAME` and `$$` are handled inside those ranges
/// exactly as outside them; only line endings and indentation are left alone.
#[test]
fn verbatim_ranges_still_substitute_metavariables_and_dollars() {
    let caps = [one("X", "q"), list("L", "1, 2")];
    let t = "s = \"a\n $X $$5 $$$L\"\nz";
    let start = t.find('"').unwrap();
    let end = t.rfind('"').unwrap() + 1;
    #[allow(clippy::single_range_in_vec_init)]
    let v = [start..end];
    let out = expand_template(t, &caps, &opts("    ", "\r\n", &v)).unwrap();
    assert_eq!(out, "s = \"a\n q $5 1, 2\"\r\n    z");
    // an unbound name inside a verbatim range is still an error
    assert_eq!(
        expand_template(t, &[], &opts("", "\n", &v))
            .unwrap_err()
            .code,
        ErrorCode::InvalidPattern
    );
}

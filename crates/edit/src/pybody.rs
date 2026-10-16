//! Python body text for `replace_body`: re-indent the caller's statements to the body, and check
//! the result against Python's indentation rules before it can become a plan.
//!
//! The check exists because the syntax gate cannot make it. tree-sitter-python recovers from
//! indentation mistakes without reporting an error (an unexpected indent, an unindent that matches
//! no outer level, an empty block), so a body with any of them would pass the gate with
//! `syntax errors 0 → 0` and fail only when Python reads the file. The rules here are the
//! tokenizer's: they look at logical lines, so lines inside a multi-line string, inside brackets
//! or after a backslash are continuation lines and are neither re-indented nor checked.

/// What the scanner knows about one physical line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct LineInfo {
    /// The line starts inside a string literal: its bytes, including leading whitespace, are
    /// part of the string's value and must not change.
    in_string: bool,
}

/// One logical line: where it starts and whether it opens a block.
#[derive(Debug, Clone, Copy)]
struct Logical {
    line: usize,
    width: usize,
    opens_block: bool,
}

/// Scan `lines` as Python source, tracking strings, brackets, comments and continuations.
fn scan(lines: &[&str]) -> (Vec<LineInfo>, Vec<Logical>) {
    let mut infos = vec![LineInfo::default(); lines.len()];
    let mut logicals: Vec<Logical> = Vec::new();
    // (quote byte, triple)
    let mut string: Option<(u8, bool)> = None;
    let mut depth: usize = 0;
    let mut backslash = false;
    let mut current: Option<usize> = None; // index into `logicals` of the open logical line
    let mut last_significant: Option<u8> = None;

    for (i, line) in lines.iter().enumerate() {
        let starts_in_string = string.is_some();
        let continues = !starts_in_string && (depth > 0 || backslash);
        infos[i].in_string = starts_in_string;
        backslash = false;

        let trimmed = line.trim_start();
        // Blank and comment-only lines: Python ignores their indentation.
        if !starts_in_string && !continues && (trimmed.is_empty() || trimmed.starts_with('#')) {
            continue;
        }
        if !starts_in_string && !continues {
            logicals.push(Logical {
                line: i,
                width: indent_width(line),
                opens_block: false,
            });
            current = Some(logicals.len() - 1);
            last_significant = None;
        }

        let bytes = line.as_bytes();
        let mut k = 0;
        while k < bytes.len() {
            let b = bytes[k];
            if let Some((q, triple)) = string {
                if b == b'\\' {
                    k += 2;
                    continue;
                }
                if b == q {
                    if !triple {
                        string = None;
                    } else if bytes.get(k + 1) == Some(&q) && bytes.get(k + 2) == Some(&q) {
                        string = None;
                        k += 3;
                        last_significant = Some(q);
                        continue;
                    }
                }
                k += 1;
                if string.is_none() {
                    last_significant = Some(q);
                }
                continue;
            }
            match b {
                b'#' => break,
                b'\'' | b'"' => {
                    let triple = bytes.get(k + 1) == Some(&b) && bytes.get(k + 2) == Some(&b);
                    string = Some((b, triple));
                    k += if triple { 3 } else { 1 };
                    continue;
                }
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth = depth.saturating_sub(1),
                b'\\' if k + 1 == bytes.len() => {
                    backslash = true;
                    k += 1;
                    continue;
                }
                b' ' | b'\t' => {
                    k += 1;
                    continue;
                }
                _ => {}
            }
            last_significant = Some(b);
            k += 1;
        }
        // A single-quoted string does not continue past the end of a line.
        if let Some((_, false)) = string {
            string = None;
        }
        let logical_ends = string.is_none() && depth == 0 && !backslash;
        if logical_ends && let Some(c) = current.take() {
            logicals[c].opens_block = last_significant == Some(b':');
        }
    }
    (infos, logicals)
}

/// Indentation width the way Python's tokenizer counts it: a tab advances to the next multiple
/// of eight.
fn indent_width(line: &str) -> usize {
    let mut width = 0;
    for c in line.chars() {
        match c {
            ' ' => width += 1,
            '\t' => width = (width / 8 + 1) * 8,
            _ => break,
        }
    }
    width
}

fn leading_ws_bytes(line: &str) -> usize {
    line.len() - line.trim_start_matches([' ', '\t']).len()
}

/// Re-indent `text`, the statements of a Python body, to `indent`.
///
/// Leading and trailing blank lines are dropped. The common indentation of the code lines is
/// removed; when the first line has none but the lines after it do, and the first line does not
/// open a block, the first line was pasted straight after the colon and the common indentation
/// is the one the other lines share. Every line except the first is then prefixed with `indent`
/// (the first is not: the edit starts after the body's existing indentation). Lines inside a
/// multi-line string are copied byte for byte.
///
/// `Err` when there is no statement at all.
pub(crate) fn reindent(text: &str, indent: &str, eol: &str) -> Result<String, String> {
    let all: Vec<&str> = text.lines().collect();
    let Some(first) = all.iter().position(|l| !l.trim().is_empty()) else {
        return Err("the body text is empty; a Python body needs at least one statement".into());
    };
    let last = all
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .unwrap_or(first);
    let lines = &all[first..=last];
    let (infos, logicals) = scan(lines);

    let code = |i: usize| !infos[i].in_string && !lines[i].trim().is_empty();
    let first_ws = leading_ws_bytes(lines[0]);
    let rest_min = (1..lines.len())
        .filter(|&i| code(i))
        .map(|i| leading_ws_bytes(lines[i]))
        .min();
    let first_opens = logicals.first().is_some_and(|l| l.opens_block);
    let common = match rest_min {
        Some(rest) if first_ws == 0 && rest > 0 && !first_opens => rest,
        Some(rest) => rest.min(first_ws),
        None => first_ws,
    };

    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            out.push_str(eol);
        }
        if i == 0 {
            out.push_str(line.trim_start_matches([' ', '\t']));
        } else if infos[i].in_string {
            out.push_str(line);
        } else if line.trim().is_empty() {
            // A blank line carries no indentation.
        } else {
            out.push_str(indent);
            out.push_str(&line[leading_ws_bytes(line).min(common)..]);
        }
    }
    Ok(out)
}

/// Check `block` (the body as it will stand in the file, every line including the first already
/// indented) against Python's indentation rules, with `base` as the body's own indentation.
///
/// `Err` names the first line of the body that breaks a rule, 1-based.
pub(crate) fn check_block(block: &str, base: usize) -> Result<(), String> {
    let lines: Vec<&str> = block.lines().collect();
    let (_infos, logicals) = scan(&lines);
    let Some(first) = logicals.first() else {
        return Err("the body has no statement".into());
    };
    if first.width != base {
        return Err(format!(
            "line {} of the body is not at the body's indentation",
            first.line + 1
        ));
    }
    let mut stack = vec![base];
    let mut prev_opens = false;
    for (n, logical) in logicals.iter().enumerate() {
        let top = *stack.last().unwrap_or(&base);
        if n > 0 {
            if prev_opens {
                if logical.width <= top {
                    return Err(format!(
                        "line {} of the body should be indented further than the line that opens its block",
                        logical.line + 1
                    ));
                }
                stack.push(logical.width);
            } else if logical.width > top {
                return Err(format!(
                    "line {} of the body is indented further than the block it is in",
                    logical.line + 1
                ));
            } else {
                while stack.last().is_some_and(|&w| w > logical.width) {
                    stack.pop();
                }
                if stack.last() != Some(&logical.width) {
                    return Err(format!(
                        "line {} of the body does not line up with any enclosing block",
                        logical.line + 1
                    ));
                }
            }
        }
        prev_opens = logical.opens_block;
    }
    if prev_opens {
        return Err("the body's last statement opens a block that has no statements".into());
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn block(text: &str, indent: &str) -> String {
        format!("{indent}{}", reindent(text, indent, "\n").unwrap())
    }

    #[test]
    fn statements_at_any_indentation_land_at_the_body() {
        assert_eq!(block("x = 1\nreturn x", "    "), "    x = 1\n    return x");
        assert_eq!(
            block("    x = 1\n    return x\n", "  "),
            "  x = 1\n  return x"
        );
        assert_eq!(
            block("if x:\n    y()\nreturn 1", "    "),
            "    if x:\n        y()\n    return 1"
        );
    }

    #[test]
    fn a_first_line_pasted_after_the_colon_takes_the_indentation_of_the_rest() {
        assert_eq!(
            block("x = 1\n    if x:\n        return x", "    "),
            "    x = 1\n    if x:\n        return x"
        );
    }

    #[test]
    fn lines_inside_a_multi_line_string_are_copied_unchanged() {
        let text = "s = '''\nraw\n  kept\n'''\nreturn s";
        assert_eq!(
            block(text, "    "),
            "    s = '''\nraw\n  kept\n'''\n    return s"
        );
        let text = "s = \"\"\"a\n   b\"\"\"\nx = 1";
        assert_eq!(block(text, "\t"), "\ts = \"\"\"a\n   b\"\"\"\n\tx = 1");
    }

    #[test]
    fn brackets_and_backslashes_continue_a_logical_line() {
        let out = block("x = f(\n  1,\n  2)\ny = 1 + \\\n  2\nreturn x", "    ");
        assert_eq!(
            out,
            "    x = f(\n      1,\n      2)\n    y = 1 + \\\n      2\n    return x"
        );
        assert_eq!(check_block(&out, 4), Ok(()));
    }

    #[test]
    fn blank_and_comment_lines_are_kept_and_ignored_by_the_check() {
        let out = block("a = 1\n\n# note\nreturn a\n\n", "    ");
        assert_eq!(out, "    a = 1\n\n    # note\n    return a");
        assert_eq!(check_block(&out, 4), Ok(()));
    }

    #[test]
    fn empty_text_is_refused() {
        assert!(reindent("", "    ", "\n").is_err());
        assert!(reindent("\n   \n\t\n", "    ", "\n").is_err());
    }

    #[test]
    fn the_check_follows_the_tokenizer_rules() {
        assert_eq!(check_block("    x = 1\n    return x", 4), Ok(()));
        assert_eq!(
            check_block("    if a:\n        b\n    else:\n        c", 4),
            Ok(())
        );
        assert!(
            check_block("    x = 1\n        y = 2", 4).is_err(),
            "unexpected indent"
        );
        assert!(
            check_block("    if x:\n    y = 2", 4).is_err(),
            "expected an indented block"
        );
        assert!(
            check_block("    if x:\n        a\n      b", 4).is_err(),
            "unindent mismatch"
        );
        assert!(
            check_block("    x = 1\n  y = 2", 4).is_err(),
            "outside the body"
        );
        assert!(
            check_block("  x = 1", 4).is_err(),
            "first line not at the body"
        );
        assert!(
            check_block("    for x in y:", 4).is_err(),
            "empty block at the end"
        );
        assert!(check_block("", 4).is_err());
    }

    #[test]
    fn a_colon_inside_brackets_strings_or_comments_does_not_open_a_block() {
        assert_eq!(check_block("    d = {1: 2}\n    return d", 4), Ok(()));
        assert_eq!(check_block("    s = 'a:'\n    return s", 4), Ok(()));
        assert_eq!(check_block("    x = 1  # see:\n    return x", 4), Ok(()));
        assert_eq!(check_block("    f = lambda: 1\n    return f", 4), Ok(()));
    }

    #[test]
    fn tabs_count_to_the_next_multiple_of_eight() {
        assert_eq!(indent_width("\tx"), 8);
        assert_eq!(indent_width("  \tx"), 8);
        assert_eq!(indent_width("        \tx"), 16);
        assert_eq!(check_block("\tif a:\n\t\tb", 8), Ok(()));
    }

    #[test]
    fn escapes_in_strings_do_not_end_them() {
        assert_eq!(check_block("    s = 'it\\'s:'\n    return s", 4), Ok(()));
        assert_eq!(
            block("s = '''a\\'''\nb'''\nreturn s", "    "),
            "    s = '''a\\'''\nb'''\n    return s"
        );
    }
}

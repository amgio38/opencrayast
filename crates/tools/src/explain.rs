//! `ast_explain_pattern` (docs/TOOLS.md).

use crate::context::ToolContext;
use opencrayast_core::error::{ErrorCode, ToolError};
use opencrayast_core::render::{escape_inline, fenced_block};
use opencrayast_lang::Language;
use opencrayast_query::pattern::{CaptureKind, Pattern};

/// Arguments of `ast_explain_pattern`.
#[derive(Debug, Clone, Default)]
pub struct ExplainArgs {
    /// The pattern. Required, 1..=16384 bytes.
    pub pattern: String,
    /// Its language id. Required.
    pub language: String,
}

/// Show how a pattern is understood.
///
/// ## Output format (exact; golden-tested)
///
/// ````text
/// pattern: <pattern, escaped, first line only>
/// language: <language id>
/// metavariables: $A (one), $$$B (list)          (or `metavariables: none`)
/// ```text
/// <Pattern::explain() output, escaped>
/// ```
/// ````
///
/// The tree is the text of `Pattern::explain()` (its `warning:` lines first) inside a fenced
/// block with info `text`, produced by `render::fenced_block`. Errors: unknown language =>
/// `invalid_args` (message lists the supported ids); an invalid pattern => `invalid_pattern`
/// with the position (from `PatternError`).
pub fn ast_explain_pattern(_ctx: &ToolContext, args: &ExplainArgs) -> Result<String, ToolError> {
    if args.pattern.is_empty() || args.pattern.len() > 16_384 {
        return Err(ToolError::new(
            ErrorCode::InvalidArgs,
            format!("pattern is {} bytes, outside 1..=16384", args.pattern.len()),
            "Pass a non-empty pattern of at most 16384 bytes.",
        ));
    }
    let language = Language::from_id(&args.language)
        .filter(|l| l.is_available())
        .ok_or_else(|| {
            ToolError::new(
                ErrorCode::InvalidArgs,
                format!(
                    "unknown language {:?}; supported: {}",
                    args.language,
                    supported_language_ids()
                ),
                "Pass a language id from the supported list.",
            )
        })?;

    let compiled = Pattern::compile(language, &args.pattern)?;
    let (pattern_line, _) = escape_inline(first_line(&args.pattern));
    let meta_line = format_metavars(&compiled);

    let (fence, _) = fenced_block(&compiled.explain(), "text");

    let mut out = String::new();
    out.push_str("pattern: ");
    out.push_str(&pattern_line);
    out.push('\n');
    out.push_str("language: ");
    out.push_str(language.id());
    out.push('\n');
    out.push_str(&meta_line);
    out.push('\n');
    out.push_str(&fence);
    Ok(out)
}

fn format_metavars(pattern: &Pattern) -> String {
    let vars = pattern.metavars();
    if vars.is_empty() {
        return "metavariables: none".into();
    }
    let parts: Vec<String> = vars
        .iter()
        .map(|v| match v.kind {
            CaptureKind::One => format!("${} (one)", v.name),
            CaptureKind::List => format!("$$${} (list)", v.name),
        })
        .collect();
    format!("metavariables: {}", parts.join(", "))
}

fn first_line(s: &str) -> &str {
    s.split('\n').next().unwrap_or(s)
}

fn supported_language_ids() -> String {
    Language::all()
        .iter()
        .filter(|l| l.is_available())
        .map(|l| l.id())
        .collect::<Vec<_>>()
        .join(", ")
}

use std::{cell::RefCell, collections::HashMap};
use tree_sitter::Language;
#[cfg(target_arch = "wasm32")]
use tree_sitter::ffi::TSLanguage;
use tree_sitter_highlight::{
    HighlightConfiguration, HighlightEvent, Highlighter as TreeSitterHighlighter,
};

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SyntaxKind {
    #[default]
    Plain,
    Comment,
    String,
    Number,
    Keyword,
    Type,
    Function,
    Constant,
    Attribute,
    Operator,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HighlightSpan {
    pub start: usize,
    pub end: usize,
    pub kind: SyntaxKind,
}

pub fn highlight_code(language: Option<&str>, text: &str) -> Vec<Vec<HighlightSpan>> {
    // Missing or failed browser side modules stay plain. Native builds create
    // supported grammar configurations on demand.
    highlight_code_tree_sitter(language, text)
}

fn language_token(language: &str) -> Option<String> {
    let token = language.split_whitespace().next()?.split(',').next()?;
    let token = token
        .trim_matches(|character| matches!(character, '{' | '}' | '.'))
        .trim()
        .to_ascii_lowercase();
    (!token.is_empty()).then_some(token)
}

fn plain_lines(lines: &[&str]) -> Vec<Vec<HighlightSpan>> {
    lines
        .iter()
        .map(|line| {
            (!line.is_empty())
                .then_some(vec![HighlightSpan {
                    start: 0,
                    end: line.len(),
                    kind: SyntaxKind::Plain,
                }])
                .unwrap_or_default()
        })
        .collect()
}

fn push_span(spans: &mut Vec<HighlightSpan>, start: usize, end: usize, kind: SyntaxKind) {
    if start >= end {
        return;
    }
    if let Some(previous) = spans.last_mut()
        && previous.end == start
        && previous.kind == kind
    {
        previous.end = end;
    } else {
        spans.push(HighlightSpan { start, end, kind });
    }
}

const TREE_SITTER_HIGHLIGHTS: &[(&str, SyntaxKind)] = &[
    ("comment", SyntaxKind::Comment),
    ("string", SyntaxKind::String),
    ("character", SyntaxKind::String),
    ("number", SyntaxKind::Number),
    ("float", SyntaxKind::Number),
    ("keyword", SyntaxKind::Keyword),
    ("type", SyntaxKind::Type),
    ("constructor", SyntaxKind::Type),
    ("function", SyntaxKind::Function),
    ("constant", SyntaxKind::Constant),
    ("boolean", SyntaxKind::Constant),
    ("variable.builtin", SyntaxKind::Constant),
    ("attribute", SyntaxKind::Attribute),
    ("property", SyntaxKind::Attribute),
    ("field", SyntaxKind::Attribute),
    ("label", SyntaxKind::Attribute),
    ("operator", SyntaxKind::Operator),
];

#[derive(Default)]
struct TreeSitterSyntax {
    highlighter: TreeSitterHighlighter,
    configurations: HashMap<String, HighlightConfiguration>,
}

thread_local! {
    static TREE_SITTER_SYNTAX: RefCell<TreeSitterSyntax> = RefCell::new(TreeSitterSyntax::default());
}

fn tree_sitter_configuration(
    language: Language,
    name: &str,
    highlights_query: &str,
    locals_query: &str,
) -> Result<HighlightConfiguration, String> {
    let mut configuration =
        HighlightConfiguration::new(language, name, highlights_query, "", locals_query)
            .map_err(|error| format!("invalid {name} Tree-sitter query: {error}"))?;
    configuration.configure(
        &TREE_SITTER_HIGHLIGHTS
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>(),
    );
    Ok(configuration)
}

#[cfg(not(target_arch = "wasm32"))]
fn native_tree_sitter_configuration(name: &str) -> Result<HighlightConfiguration, String> {
    match name {
        "rust" => {
            // Upstream classifies numeric literals as `constant.builtin` and
            // only marks Rust's unary `*`, `&`, and lifetime apostrophe as
            // operators. Refine those broad captures to flow's smaller palette.
            let highlights = format!(
                "{}\n(integer_literal) @number\n(float_literal) @float\n[\"+\" \"-\" \"/\" \"%\" \"==\" \"!=\" \"<\" \">\" \"<=\" \">=\" \"&&\" \"||\" \"=\" \"+=\" \"-=\" \"*=\" \"/=\" \"%=\" \"<<\" \">>\" \"^\" \"|\" \"!\"] @operator",
                tree_sitter_rust::HIGHLIGHTS_QUERY
            );
            tree_sitter_configuration(tree_sitter_rust::LANGUAGE.into(), name, &highlights, "")
        }
        "javascript" => {
            let highlights = format!(
                "{}\n{}",
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                tree_sitter_javascript::JSX_HIGHLIGHT_QUERY
            );
            tree_sitter_configuration(
                tree_sitter_javascript::LANGUAGE.into(),
                name,
                &highlights,
                tree_sitter_javascript::LOCALS_QUERY,
            )
        }
        "typescript" => {
            // The TypeScript crate ships supplemental queries. Its grammar is
            // derived from JavaScript, so the base JavaScript queries must run
            // first to cover ordinary expressions and declarations.
            let highlights = format!(
                "{}\n{}",
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                tree_sitter_typescript::HIGHLIGHTS_QUERY
            );
            let locals = format!(
                "{}\n{}",
                tree_sitter_javascript::LOCALS_QUERY,
                tree_sitter_typescript::LOCALS_QUERY
            );
            tree_sitter_configuration(
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
                name,
                &highlights,
                &locals,
            )
        }
        "bash" => tree_sitter_configuration(
            tree_sitter_bash::LANGUAGE.into(),
            name,
            tree_sitter_bash::HIGHLIGHT_QUERY,
            "",
        ),
        "go" => tree_sitter_configuration(
            tree_sitter_go::LANGUAGE.into(),
            name,
            tree_sitter_go::HIGHLIGHTS_QUERY,
            "",
        ),
        "sql" => tree_sitter_configuration(
            tree_sitter_sequel::LANGUAGE.into(),
            name,
            tree_sitter_sequel::HIGHLIGHTS_QUERY,
            "",
        ),
        _ => Err(format!("unsupported native Tree-sitter language: {name}")),
    }
}

/// Registers a dynamically instantiated Tree-sitter language module.
///
/// # Safety
///
/// `language_address` must point to a `TSLanguage` in this WebAssembly
/// instance's linear memory. Its data and indirect-function-table entries must
/// remain alive for the rest of the page. The browser loader establishes those
/// conditions by instantiating a side module against flow's memory/table
/// and retaining that instance.
#[cfg(target_arch = "wasm32")]
pub unsafe fn register_tree_sitter_language(
    name: &str,
    language_address: u32,
    highlights_query: &str,
    locals_query: &str,
) -> Result<(), String> {
    if language_address == 0 {
        return Err("Tree-sitter language module returned a null language".to_owned());
    }
    let canonical = canonical_tree_sitter_language(name)
        .ok_or_else(|| format!("unsupported Tree-sitter language: {name}"))?;
    let language = unsafe { Language::from_raw(language_address as *const TSLanguage) };
    let configuration =
        tree_sitter_configuration(language, canonical, highlights_query, locals_query)?;
    TREE_SITTER_SYNTAX.with(|state| {
        state
            .borrow_mut()
            .configurations
            .insert(canonical.to_owned(), configuration);
    });
    Ok(())
}

fn highlight_code_tree_sitter(language: Option<&str>, text: &str) -> Vec<Vec<HighlightSpan>> {
    let lines = text.split('\n').collect::<Vec<_>>();
    let Some(token) = language
        .and_then(language_token)
        .and_then(|token| canonical_tree_sitter_language(&token))
    else {
        return plain_lines(&lines);
    };

    TREE_SITTER_SYNTAX.with(|state| {
        let mut state = state.borrow_mut();
        #[cfg(not(target_arch = "wasm32"))]
        if !state.configurations.contains_key(token) {
            let Ok(configuration) = native_tree_sitter_configuration(token) else {
                return plain_lines(&lines);
            };
            state.configurations.insert(token.to_owned(), configuration);
        }
        let TreeSitterSyntax {
            highlighter,
            configurations,
        } = &mut *state;
        let Some(configuration) = configurations.get(token) else {
            return plain_lines(&lines);
        };
        let Ok(events) =
            highlighter.highlight(configuration, text.as_bytes(), None, None, |_| None)
        else {
            return plain_lines(&lines);
        };

        let mut stack = Vec::new();
        let mut global_spans = Vec::new();
        for event in events {
            match event {
                Ok(HighlightEvent::HighlightStart(highlight)) => {
                    stack.push(TREE_SITTER_HIGHLIGHTS[highlight.0].1);
                }
                Ok(HighlightEvent::HighlightEnd) => {
                    stack.pop();
                }
                Ok(HighlightEvent::Source { start, end }) => {
                    push_span(
                        &mut global_spans,
                        start,
                        end,
                        stack.last().copied().unwrap_or(SyntaxKind::Plain),
                    );
                }
                Err(_) => return plain_lines(&lines),
            }
        }
        spans_by_line(&lines, &global_spans)
    })
}

fn canonical_tree_sitter_language(language: &str) -> Option<&'static str> {
    match language {
        "rust" | "rs" => Some("rust"),
        "javascript" | "js" | "jsx" | "node" => Some("javascript"),
        "typescript" | "ts" => Some("typescript"),
        "bash" | "sh" | "shell" | "zsh" => Some("bash"),
        "go" | "golang" => Some("go"),
        "sql" => Some("sql"),
        _ => None,
    }
}

fn spans_by_line(lines: &[&str], global_spans: &[HighlightSpan]) -> Vec<Vec<HighlightSpan>> {
    let mut result = Vec::with_capacity(lines.len());
    let mut global_index = 0;
    let mut line_start = 0;
    for line in lines {
        let line_end = line_start + line.len();
        while global_spans
            .get(global_index)
            .is_some_and(|span| span.end <= line_start)
        {
            global_index += 1;
        }

        let mut spans = Vec::new();
        let mut cursor = line_start;
        let mut index = global_index;
        while let Some(span) = global_spans.get(index) {
            if span.start >= line_end {
                break;
            }
            let start = span.start.max(line_start);
            let end = span.end.min(line_end);
            if cursor < start {
                push_span(
                    &mut spans,
                    cursor - line_start,
                    start - line_start,
                    SyntaxKind::Plain,
                );
            }
            push_span(&mut spans, start - line_start, end - line_start, span.kind);
            cursor = cursor.max(end);
            index += 1;
        }
        if cursor < line_end {
            push_span(
                &mut spans,
                cursor - line_start,
                line_end - line_start,
                SyntaxKind::Plain,
            );
        }
        result.push(spans);
        line_start = line_end.saturating_add(1);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(language: &str, text: &str) -> Vec<SyntaxKind> {
        highlight_code(Some(language), text)
            .into_iter()
            .flatten()
            .map(|span| span.kind)
            .collect()
    }

    #[test]
    fn highlights_common_rust_scopes() {
        let kinds = kinds(
            "rust",
            "fn answer(value: i32) -> i32 { // note\n    let message = \"forty-two\";\n    value + 42\n}",
        );
        for expected in [
            SyntaxKind::Keyword,
            SyntaxKind::Function,
            SyntaxKind::Type,
            SyntaxKind::Comment,
            SyntaxKind::String,
            SyntaxKind::Number,
            SyntaxKind::Operator,
        ] {
            assert!(kinds.contains(&expected), "missing {expected:?}: {kinds:?}");
        }
    }

    #[test]
    fn preserves_multiline_comment_state() {
        let highlighted = highlight_code(Some("js"), "/* first\nsecond */\nconst value = 1;");
        assert_eq!(highlighted[0][0].kind, SyntaxKind::Comment);
        assert_eq!(highlighted[1][0].kind, SyntaxKind::Comment);
        assert!(
            highlighted[2]
                .iter()
                .any(|span| span.kind == SyntaxKind::Keyword)
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn native_tree_sitter_loads_each_bundled_language_on_demand() {
        for (language, source, expected) in [
            ("rust", "fn main() {}", SyntaxKind::Keyword),
            ("javascript", "const value = 1;", SyntaxKind::Keyword),
            (
                "typescript",
                "interface Value { item: string }",
                SyntaxKind::Type,
            ),
            (
                "bash",
                "for value in one two; do echo $value; done",
                SyntaxKind::Keyword,
            ),
            ("go", "func main() {}", SyntaxKind::Keyword),
            ("sql", "SELECT value FROM items", SyntaxKind::Keyword),
        ] {
            let highlighted = highlight_code(Some(language), source);
            assert_eq!(highlighted.len(), 1, "wrong line count for {language}");
            assert!(
                highlighted[0].iter().any(|span| span.kind == expected),
                "missing {expected:?} for {language}: {:?}",
                highlighted[0]
            );
            assert_eq!(
                highlighted[0]
                    .iter()
                    .map(|span| span.end - span.start)
                    .sum::<usize>(),
                source.len(),
                "spans did not cover {language} source"
            );
        }
        TREE_SITTER_SYNTAX.with(|state| {
            let state = state.borrow();
            for language in ["rust", "javascript", "typescript", "bash", "go", "sql"] {
                assert!(
                    state.configurations.contains_key(language),
                    "missing grammar {language}"
                );
            }
        });
    }

    #[test]
    fn unknown_and_plain_languages_remain_plain() {
        for language in [None, Some("text"), Some("not-a-real-language")] {
            assert_eq!(
                highlight_code(language, "some code"),
                vec![vec![HighlightSpan {
                    start: 0,
                    end: 9,
                    kind: SyntaxKind::Plain,
                }]]
            );
        }
    }

    #[test]
    fn accepts_markdown_fence_attributes() {
        assert!(
            highlight_code(Some("{.Rust}"), "let value = 1;")[0]
                .iter()
                .any(|span| span.kind == SyntaxKind::Keyword)
        );
    }
}

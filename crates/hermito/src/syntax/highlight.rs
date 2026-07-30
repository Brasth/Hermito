//! Inert highlight spans. Extraction off-thread via compiled queries.
//! Covers keywords, strings, comments, functions, types, numbers per acceptance.

use tree_sitter::Tree;
use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

use crate::document::Language;

const HIGHLIGHT_NAMES: &[&str] = &[
    "keyword", "string", "comment", "function", "type", "number", "other",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HighlightKind {
    Keyword,
    String,
    Comment,
    Function,
    Type,
    Number,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HighlightSpan {
    pub start_byte: usize,
    pub end_byte: usize,
    pub kind: HighlightKind,
}

pub fn extract_highlights(language: Language, _tree: &Tree, source: &str) -> Vec<HighlightSpan> {
    let config = match build_config(language) {
        Some(c) => c,
        None => return vec![],
    };

    let mut highlighter = Highlighter::new();
    let events = match highlighter.highlight(&config, source.as_bytes(), None, |_| None) {
        Ok(e) => e,
        Err(_) => return vec![],
    };

    let mut out = vec![];
    let mut active: Option<HighlightKind> = None;

    for ev in events {
        match ev {
            Ok(HighlightEvent::Source { start, end }) => {
                if let Some(k) = active {
                    if end > start {
                        out.push(HighlightSpan {
                            start_byte: start,
                            end_byte: end,
                            kind: k,
                        });
                    }
                }
            }
            Ok(HighlightEvent::HighlightStart(s)) => {
                let name = HIGHLIGHT_NAMES.get(s.0).copied().unwrap_or("other");
                active = Some(kind_from_name(name));
            }
            Ok(HighlightEvent::HighlightEnd) => {
                active = None;
            }
            Err(_) => {
                // malformed highlight stream: fall back safely to no highlights (plain-text semantics)
                return vec![];
            }
        }
    }
    out
}

fn kind_from_name(name: &str) -> HighlightKind {
    match name {
        "keyword" => HighlightKind::Keyword,
        "string" => HighlightKind::String,
        "comment" => HighlightKind::Comment,
        "function" => HighlightKind::Function,
        "type" => HighlightKind::Type,
        "number" => HighlightKind::Number,
        _ => HighlightKind::Other,
    }
}

fn build_config(language: Language) -> Option<HighlightConfiguration> {
    let (ts_language, query) = match language {
        Language::Rust => (tree_sitter_rust::LANGUAGE.into(), RUST_QUERY),
        Language::TypeScript => (tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(), TS_QUERY),
        Language::JavaScript => (tree_sitter_javascript::LANGUAGE.into(), JS_QUERY),
        Language::Go => (tree_sitter_go::LANGUAGE.into(), GO_QUERY),
        Language::Python => (tree_sitter_python::LANGUAGE.into(), PY_QUERY),
        Language::PlainText => return None,
    };

    let mut cfg = HighlightConfiguration::new(ts_language, "hermito", query, "", "").ok()?;
    cfg.configure(HIGHLIGHT_NAMES);
    Some(cfg)
}

// Minimal queries targeting required highlight kinds. Node names from pinned grammars.
const RUST_QUERY: &str = r#"
[
  "as" "async" "await" "break" "const" "continue" "crate" "dyn" "else" "enum"
  "extern" "false" "fn" "for" "if" "impl" "in" "let" "loop" "match" "mod"
  "move" "mut" "pub" "ref" "return" "self" "Self" "static" "struct" "super"
  "trait" "true" "type" "unsafe" "use" "where" "while" "yield"
] @keyword

(line_comment) @comment
(block_comment) @comment

(string_literal) @string
(raw_string_literal) @string

(integer_literal) @number
(float_literal) @number

(function_item name: (identifier) @function)
(impl_item trait: (type_identifier) @type type: (type_identifier) @type)
(struct_item name: (type_identifier) @type)
(enum_item name: (type_identifier) @type)
(type_identifier) @type
(primitive_type) @type
"#;

const JS_QUERY: &str = r#"
[
  "async" "await" "break" "case" "catch" "class" "const" "continue" "debugger"
  "default" "delete" "do" "else" "export" "extends" "finally" "for" "function"
  "if" "import" "in" "instanceof" "let" "new" "return" "static" "super" "switch"
  "this" "throw" "try" "typeof" "var" "void" "while" "with" "yield"
] @keyword

(comment) @comment
(string) @string
(template_string) @string
(number) @number

(function_declaration name: (identifier) @function)
(function_expression name: (identifier) @function)
(method_definition name: (property_identifier) @function)
(call_expression function: (identifier) @function)
(class_declaration name: (identifier) @type)
(type_identifier) @type
"#;

const TS_QUERY: &str = JS_QUERY; // sufficient for common constructs on TS lang

const GO_QUERY: &str = r#"
[
  "break" "case" "chan" "const" "continue" "default" "defer" "else"
  "fallthrough" "for" "func" "go" "goto" "if" "import" "interface"
  "map" "package" "range" "return" "select" "struct" "switch" "type" "var"
] @keyword

(comment) @comment
(interpreted_string_literal) @string
(raw_string_literal) @string
(int_literal) @number
(float_literal) @number

(function_declaration name: (identifier) @function)
(method_declaration name: (field_identifier) @function)
(type_identifier) @type
(primitive_type) @type
"#;

const PY_QUERY: &str = r#"
[
  "and" "as" "assert" "async" "await" "break" "class" "continue" "def" "del"
  "elif" "else" "except" "finally" "for" "from" "global" "if" "import" "in"
  "is" "lambda" "nonlocal" "not" "or" "pass" "raise" "return" "try" "while"
  "with" "yield"
] @keyword

(comment) @comment
(string) @string
(escape_sequence) @string
(integer) @number
(float) @number

(function_definition name: (identifier) @function)
(class_definition name: (identifier) @type)
(type_identifier) @type
"#;

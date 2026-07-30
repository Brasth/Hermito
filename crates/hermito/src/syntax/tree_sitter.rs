//! Incremental Tree-sitter integration. compute_syntax is the off-thread entry.
//! One bounded/coalescing in-flight syntax job for the current document.
//! Retain returned Tree and source text per document (in App cache).
//! For subsequent revisions, event_loop supplies old_tree + old_text from retained;
//! compute_syntax derives exact InputEdit from old/new source (Unicode bytes + Point cols via prefix/suffix),
//! calls tree.edit(ie), then parser.parse(new, Some(edited_old)). Full parse if no prior tree.
//! Results always tagged. Malformed/grammar error -> None tree + empty highlights (plain fallback).
//! Stale results filtered by caller; at most one job; rapid edits coalesce to latest desired.

use tree_sitter::{InputEdit, Parser, Point, Tree};

use crate::document::{DocumentId, DocumentRevision, Language, WorkspaceEpoch};
use crate::edit::TextEdit;
use crate::syntax::highlight::{extract_highlights, HighlightSpan};

#[derive(Clone, Debug)]
pub struct SyntaxRequest {
    pub epoch: WorkspaceEpoch,
    pub doc_id: DocumentId,
    pub revision: DocumentRevision,
    pub language: Language,
    pub new_text: String,
    pub old_text: Option<String>,
    pub old_tree: Option<Tree>,
    pub edit: Option<TextEdit>,
}

#[derive(Clone, Debug)]
pub struct SyntaxResult {
    pub epoch: WorkspaceEpoch,
    pub doc_id: DocumentId,
    pub revision: DocumentRevision,
    pub tree: Option<Tree>,
    pub highlights: Vec<HighlightSpan>,
    pub source: String,
}

/// Off-thread work. Incremental path (old_tree + old_text) derives exact InputEdit from
/// old/new source texts and uses edited tree. edit: field kept for compat but source-diff takes precedence.
/// Always validates language; PlainText and parse failures produce empty result.
pub fn compute_syntax(req: SyntaxRequest) -> SyntaxResult {
    if req.language == Language::PlainText {
        return plain_fallback(req);
    }

    let ts_lang = match req.language {
        Language::Rust => tree_sitter_rust::LANGUAGE.into(),
        Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        Language::Go => tree_sitter_go::LANGUAGE.into(),
        Language::Python => tree_sitter_python::LANGUAGE.into(),
        Language::PlainText => unreachable!(),
    };

    let mut parser = Parser::new();
    if parser.set_language(&ts_lang).is_err() {
        return plain_fallback(req);
    }

    let tree = if let (Some(mut old), Some(old_text)) = (req.old_tree, req.old_text.as_ref()) {
        // exact InputEdit from old/new source (prefix/suffix for the net contiguous change)
        let ie = text_diff_to_input_edit(old_text, &req.new_text);
        old.edit(&ie);
        parser.parse(&req.new_text, Some(&old))
    } else {
        parser.parse(&req.new_text, None)
    };

    let highlights = match &tree {
        Some(t) => extract_highlights(req.language, t, &req.new_text),
        None => vec![],
    };

    SyntaxResult {
        epoch: req.epoch,
        doc_id: req.doc_id,
        revision: req.revision,
        tree,
        highlights,
        source: req.new_text,
    }
}

fn plain_fallback(req: SyntaxRequest) -> SyntaxResult {
    SyntaxResult {
        epoch: req.epoch,
        doc_id: req.doc_id,
        revision: req.revision,
        tree: None,
        highlights: vec![],
        source: req.new_text,
    }
}

fn text_diff_to_input_edit(old_text: &str, new_text: &str) -> InputEdit {
    // Compute minimal contiguous edit region from common prefix + common suffix.
    // Stop ONLY at UTF-8 char boundaries (both for start and for suffix of remainder).
    // Guarantees InputEdit bytes are always char boundaries; byte values and Point columns agree.
    // Used for tree.edit + incremental parse; must match fresh parse result on shared-leading-byte cases.
    let p: usize = old_text
        .chars()
        .zip(new_text.chars())
        .take_while(|(o, n)| o == n)
        .map(|(c, _)| c.len_utf8())
        .sum();
    // suffix only from the remainder after prefix (prevents over-subtract that crosses edit region)
    let s: usize = old_text[p..]
        .chars()
        .rev()
        .zip(new_text[p..].chars().rev())
        .take_while(|(o, n)| o == n)
        .map(|(c, _)| c.len_utf8())
        .sum();
    let old_end = old_text.len() - s;
    let new_end = new_text.len() - s;
    InputEdit {
        start_byte: p,
        old_end_byte: old_end,
        new_end_byte: new_end,
        start_position: byte_to_point(old_text, p),
        old_end_position: byte_to_point(old_text, old_end),
        new_end_position: byte_to_point(new_text, new_end),
    }
}

fn byte_to_point(text: &str, byte: usize) -> Point {
    let byte = byte.min(text.len());
    let mut row = 0usize;
    let mut column = 0usize;
    let mut pos = 0usize;
    for ch in text.chars() {
        let ch_len = ch.len_utf8();
        if pos + ch_len > byte {
            break;
        }
        if ch == '\n' {
            row += 1;
            column = 0;
        } else {
            column += ch_len;
        }
        pos += ch_len;
    }
    Point { row, column }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_text_diff_insert_at_start() {
        let old = "hello";
        let new = "Xhello";
        let ie = text_diff_to_input_edit(old, new);
        assert_eq!(ie.start_byte, 0);
        assert_eq!(ie.old_end_byte, 0);
        assert_eq!(ie.new_end_byte, 1);
        assert_eq!(ie.start_position, Point { row: 0, column: 0 });
        assert_eq!(ie.new_end_position, Point { row: 0, column: 1 });
    }

    #[test]
    fn test_text_diff_insert_middle() {
        let old = "helloworld";
        let new = "helloXworld";
        let ie = text_diff_to_input_edit(old, new);
        assert_eq!(ie.start_byte, 5);
        assert_eq!(ie.old_end_byte, 5);
        assert_eq!(ie.new_end_byte, 6);
    }

    #[test]
    fn test_text_diff_delete() {
        let old = "hello world";
        let new = "helloworld";
        let ie = text_diff_to_input_edit(old, new);
        assert_eq!(ie.start_byte, 5);
        assert_eq!(ie.old_end_byte, 6);
        assert_eq!(ie.new_end_byte, 5);
    }

    #[test]
    fn test_text_diff_replace() {
        let old = "hello world";
        let new = "hi there";
        let ie = text_diff_to_input_edit(old, new);
        // since common prefix "h", then suffix none common, spans most
        assert!(ie.start_byte <= 1);
        assert!(ie.old_end_byte >= old.len() - 1 || ie.new_end_byte >= new.len() - 1);
    }

    #[test]
    fn test_text_diff_noop() {
        let t = "abc\ndef";
        let ie = text_diff_to_input_edit(t, t);
        assert_eq!(ie.start_byte, t.len());
        assert_eq!(ie.old_end_byte, t.len());
        assert_eq!(ie.new_end_byte, t.len());
    }

    #[test]
    fn test_text_diff_unicode_bytes_and_points() {
        let old = "café";
        let new = "cafXé";
        let ie = text_diff_to_input_edit(old, new);
        // 'é' = 2 bytes, insert before last char
        assert_eq!(ie.start_byte, 3); // after "caf"
        assert_eq!(ie.old_end_byte, 3);
        assert_eq!(ie.new_end_byte, 4);
        // points: column in bytes
        assert_eq!(ie.start_position.column, 3);
    }

    #[test]
    fn test_text_diff_unicode_char_boundary_shared_leading_byte() {
        // é (C3 A9) → ê (C3 AA): byte prefix would stop at 1 (shared lead), splitting UTF-8 char.
        // Must stop at 0; generated bytes and Points (cols) must agree.
        let old = "café";
        let new = "cafê";
        let ie = text_diff_to_input_edit(old, new);
        assert_eq!(ie.start_byte, 3);
        assert_eq!(ie.old_end_byte, 5);
        assert_eq!(ie.new_end_byte, 5);
        assert_eq!(ie.start_position.column, 3);
        assert_eq!(ie.old_end_position.column, 5);
        assert_eq!(ie.new_end_position.column, 5);
        assert!(old.is_char_boundary(ie.start_byte));
        assert!(old.is_char_boundary(ie.old_end_byte));
        assert!(new.is_char_boundary(ie.new_end_byte));
    }

    #[test]
    fn test_incremental_matches_fresh_parse_on_unicode_substitution() {
        // shared-leading-byte sub must produce same highlights (observable) via inc edit vs full parse
        let old_text = "let s = \"café\";";
        let new_text = "let s = \"cafê\";";
        let lang = tree_sitter_rust::LANGUAGE.into();
        let mut parser = Parser::new();
        parser.set_language(&lang).expect("set rust lang");
        let mut old_tree = parser.parse(old_text, None).expect("old tree");
        let ie = text_diff_to_input_edit(old_text, new_text);
        assert!(old_text.is_char_boundary(ie.start_byte));
        assert!(old_text.is_char_boundary(ie.old_end_byte));
        assert!(new_text.is_char_boundary(ie.new_end_byte));
        assert_eq!(ie.start_byte, ie.start_position.column);
        assert_eq!(ie.old_end_byte, ie.old_end_position.column);
        assert_eq!(ie.new_end_byte, ie.new_end_position.column);
        old_tree.edit(&ie);
        let inc = parser.parse(new_text, Some(&old_tree)).expect("inc parse");
        let fresh = parser.parse(new_text, None).expect("fresh parse");
        let inc_hl = extract_highlights(Language::Rust, &inc, new_text);
        let fresh_hl = extract_highlights(Language::Rust, &fresh, new_text);
        assert_eq!(
            inc_hl, fresh_hl,
            "inc output must match fresh for shared-byte unicode sub"
        );
    }
}

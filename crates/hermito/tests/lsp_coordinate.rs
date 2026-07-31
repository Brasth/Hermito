//! Deterministic public-contract coverage for LSP/editor coordinate translation.
//! Run with: cargo test -p hermito --test lsp_coordinate

use hermito::{
    coordinate::CellPos,
    lsp::{
        AuthorityIdentity, CoordinateMapper, DirectTransport, LspClient, LspContext,
        LspDocumentLedger, LspStaleDiscard, SentVersion, SessionGeneration,
    },
};
use hermito_protocol::request::{
    DocumentRevision, EnvironmentEpoch, ExecutionContextV1, WorkspaceEpoch,
};
use lsp_types::Position;
use ropey::Rope;

fn context(sent_version: u64) -> LspContext {
    LspContext {
        workspace_epoch: WorkspaceEpoch(7),
        environment_epoch: EnvironmentEpoch(11),
        document_revision: Some(DocumentRevision(13)),
        sent_version: SentVersion(sent_version),
        session_generation: SessionGeneration(17),
        execution_context: ExecutionContextV1::AuthorityRoot,
        authority_identity: AuthorityIdentity("local".into()),
    }
}

#[test]
fn exact_unicode_utf16_roundtrip_and_grapheme_cell_snaps_are_canonical() {
    let text = "a\té\r\nn\u{0303}😀界\n";
    let rope = Rope::from_str(text);
    let mapper = CoordinateMapper::new(&rope);
    let line_one = text.find("n\u{0303}").unwrap();
    let combining = text.find('\u{0303}').unwrap();
    let emoji = text.find('😀').unwrap();
    let cjk = text.find('界').unwrap();

    assert_eq!(mapper.byte_to_char(3), None); // second byte of é
    assert_eq!(mapper.byte_to_char(emoji + 1), None);
    assert_eq!(mapper.utf16_position_to_byte_exact(1, 3), None); // interior of 😀
    assert_eq!(mapper.utf16_position_to_byte_exact(0, 4), None); // past text before CRLF
    assert_eq!(mapper.byte_to_lsp_position(4), None); // CR terminator is not a text position
    assert_eq!(mapper.byte_to_lsp_position(emoji), Some(Position::new(1, 2)));
    assert_eq!(mapper.byte_to_lsp_position(cjk), Some(Position::new(1, 4)));
    for byte in [0, 1, 2, line_one, combining, emoji, cjk, text.len()] {
        let position = mapper.byte_to_lsp_position(byte).unwrap();
        assert_eq!(mapper.lsp_position_to_byte(position), Some(byte));
    }

    // Combining marks and a UTF-8 interior snap to the base grapheme; snap remains idempotent.
    for byte in [combining, emoji + 1, cjk + 1] {
        let snapped = mapper.snap_to_grapheme_start(byte);
        assert_eq!(mapper.snap_to_grapheme_start(snapped), snapped);
    }
    assert_eq!(mapper.snap_to_grapheme_start(combining), line_one);
    assert_eq!(mapper.snap_to_grapheme_start(emoji + 1), emoji);

    // A tab spans four cells, combining marks span none, and emoji/CJK span two.
    assert_eq!(mapper.byte_to_cell(1), CellPos::new(0, 1));
    assert_eq!(mapper.cell_to_byte(CellPos::new(0, 2)), 1);
    assert_eq!(mapper.byte_to_cell(combining), CellPos::new(1, 0));
    assert_eq!(mapper.byte_to_cell(emoji), CellPos::new(1, 1));
    assert_eq!(mapper.byte_to_cell(cjk), CellPos::new(1, 3));
    assert_eq!(mapper.cell_to_byte(CellPos::new(1, 2)), emoji);
    assert_eq!(mapper.cell_to_byte(CellPos::new(1, 4)), cjk);
}

#[test]
fn stale_ledger_context_is_rejected_with_a_typed_discard() {
    type InertDirectTransport = DirectTransport<tokio::io::DuplexStream, tokio::io::DuplexStream>;

    let sent = context(23);
    let mut received = sent.clone();
    received.sent_version = SentVersion(24);
    let ledger = LspDocumentLedger {
        context: ExecutionContextV1::AuthorityRoot,
        authority_identity: AuthorityIdentity("local".into()),
        revision: hermito::lsp::DocumentRevision(13),
        sent_version: 23,
        session_generation: 17,
        workspace_epoch: hermito::lsp::WorkspaceEpoch(7),
        environment_epoch: EnvironmentEpoch(11),
        text: "n\u{0303}😀界".into(),
    };

    assert!(matches!(
        LspClient::<InertDirectTransport>::filter_stale_context(&received, &sent, Some(&ledger)),
        Err(LspStaleDiscard::MismatchedSentVersion {
            expected: 23,
            actual: 24,
        })
    ));
}

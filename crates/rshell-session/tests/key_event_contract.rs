//! 真实引擎的兼容事件接缝；协议状态全部由远端字节经 advance 建立。

use std::sync::Arc;

use rshell_core::{
    CellPosition, DisplayRecovery, KeyCode, KeyEventPhase, KeyModifiers, RenderFrame, SearchMatch,
    SearchQuery, SelectionRange, TerminalDisplayModes, TerminalInput, TerminalKeyEvent,
    TerminalMouseEvent, TerminalOverrides, TerminalSettingsV1, TerminalSize, Viewport,
};
use rshell_session::{
    DefaultTerminalEngine, EngineDelta, EngineError, TerminalEngine, ViewportBounds,
};

const PHASES: [KeyEventPhase; 3] = [
    KeyEventPhase::Press,
    KeyEventPhase::Repeat,
    KeyEventPhase::Release,
];

fn engine(kitty: bool, csi_u: bool) -> DefaultTerminalEngine {
    let settings = TerminalSettingsV1 {
        enable_kitty_keyboard: kitty,
        enable_csi_u: csi_u,
        ..TerminalSettingsV1::default()
    };
    DefaultTerminalEngine::new(
        &settings.resolve(&TerminalOverrides::default()),
        TerminalSize {
            cols: 20,
            rows: 3,
            pixel_width: 160,
            pixel_height: 48,
            dpi: 96,
        },
    )
    .unwrap()
}

fn event(
    code: KeyCode,
    legacy_code: KeyCode,
    modifiers: KeyModifiers,
    phase: KeyEventPhase,
) -> TerminalKeyEvent {
    TerminalKeyEvent {
        code,
        legacy_code,
        modifiers,
        phase,
    }
}

fn same_key(code: KeyCode, modifiers: KeyModifiers, phase: KeyEventPhase) -> TerminalKeyEvent {
    event(code.clone(), code, modifiers, phase)
}

fn query(engine: &mut DefaultTerminalEngine, flags: u8) {
    assert_eq!(
        engine.advance(b"\x1b[?u").unwrap().outbound,
        format!("\x1b[?{flags}u").as_bytes()
    );
}

struct ShiftCase {
    code: char,
    legacy: char,
    modifiers: KeyModifiers,
    fallback: [&'static [u8]; 2],
    kitty: [&'static [u8]; 3],
    old_kitty: &'static [u8],
}

// 预期值独立写定，不调用被测编码器生成表格。
const SHIFT_CASES: [ShiftCase; 3] = [
    ShiftCase {
        code: '6',
        legacy: '^',
        modifiers: KeyModifiers {
            shift: true,
            control: true,
            alt: false,
            super_key: false,
        },
        fallback: [b"\x1e", b"\x1b[94;5u"],
        kitty: [b"\x1b[54;6u", b"\x1b[54;6:2u", b"\x1b[54;6:3u"],
        old_kitty: b"\x1b[94;6u",
    },
    ShiftCase {
        code: '.',
        legacy: '>',
        modifiers: KeyModifiers {
            shift: true,
            control: false,
            alt: true,
            super_key: false,
        },
        fallback: [b"\x1b>", b"\x1b>"],
        kitty: [b"\x1b[46;4u", b"\x1b[46;4:2u", b"\x1b[46;4:3u"],
        old_kitty: b"\x1b[62;4u",
    },
    ShiftCase {
        code: 'a',
        legacy: 'A',
        modifiers: KeyModifiers {
            shift: true,
            control: true,
            alt: false,
            super_key: false,
        },
        fallback: [b"\x01", b"\x1b[65;5u"],
        kitty: [b"\x1b[97;6u", b"\x1b[97;6:2u", b"\x1b[97;6:3u"],
        old_kitty: b"\x1b[97;6u",
    },
];

fn assert_shift_matrix(engine: &mut DefaultTerminalEngine, csi_u: bool, flags: u8) {
    for case in &SHIFT_CASES {
        let fallback = case.fallback[usize::from(csi_u)];
        let old = if flags & 1 != 0 {
            case.old_kitty
        } else {
            fallback
        };
        assert_eq!(
            engine
                .encode_input(TerminalInput::Key {
                    code: KeyCode::Character(case.legacy),
                    modifiers: case.modifiers,
                })
                .unwrap(),
            old,
            "旧入口: {} flags={flags}",
            case.legacy
        );
        for (index, phase) in PHASES.into_iter().enumerate() {
            let expected = match phase {
                KeyEventPhase::Press if flags & 1 != 0 => case.kitty[0],
                KeyEventPhase::Repeat | KeyEventPhase::Release if flags & 2 != 0 => {
                    case.kitty[index]
                }
                KeyEventPhase::Repeat if flags & 1 != 0 => case.kitty[0],
                KeyEventPhase::Release => b"",
                _ => fallback,
            };
            assert_eq!(
                engine
                    .encode_key_event(event(
                        KeyCode::Character(case.code),
                        KeyCode::Character(case.legacy),
                        case.modifiers,
                        phase,
                    ))
                    .unwrap(),
                expected,
                "新入口: {} {phase:?} flags={flags} csi_u={csi_u}",
                case.code
            );
        }
    }
}

fn assert_super(engine: &mut DefaultTerminalEngine, flags: u8) {
    let modifiers = KeyModifiers {
        super_key: true,
        ..KeyModifiers::default()
    };
    let expected: [&[u8]; 3] = [b"\x1b[97;9u", b"\x1b[97;9:2u", b"\x1b[97;9:3u"];
    for (index, phase) in PHASES.into_iter().enumerate() {
        let encoded = engine.encode_key_event(same_key(KeyCode::Character('a'), modifiers, phase));
        if flags & 3 == 0 {
            assert_eq!(
                encoded,
                Err(EngineError::UnsupportedInput("super-modified key"))
            );
        } else {
            let bytes = match phase {
                KeyEventPhase::Release if flags & 2 == 0 => b"".as_slice(),
                KeyEventPhase::Repeat if flags & 2 == 0 => expected[0],
                _ => expected[index],
            };
            assert_eq!(encoded.unwrap(), bytes);
        }
    }
    assert_eq!(
        engine.encode_input(TerminalInput::Key {
            code: KeyCode::Character('a'),
            modifiers,
        }),
        Err(EngineError::UnsupportedInput("super-modified key"))
    );
}

fn assert_state(engine: &mut DefaultTerminalEngine, csi_u: bool, flags: u8, kitty_allowed: bool) {
    if kitty_allowed {
        query(engine, flags);
    } else {
        assert!(engine.advance(b"\x1b[?u").unwrap().outbound.is_empty());
    }
    assert_shift_matrix(engine, csi_u, flags);
    assert_super(engine, flags);
}

fn visible_frame(terminal: &DefaultTerminalEngine) -> Arc<RenderFrame> {
    terminal.snapshot(
        Viewport {
            top_stable_row: terminal.viewport_bounds().bottom_top_stable_row,
            rows: 3,
        },
        None,
    )
}

fn displayed_rows(frame: &RenderFrame) -> Vec<String> {
    frame
        .rows
        .iter()
        .map(|row| {
            row.cells
                .iter()
                .map(|cell| cell.text.as_str())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}

#[test]
fn same_batch_and_split_set_sequences_keep_query_and_key_bytes_consistent() {
    for kitty in [false, true] {
        for csi_u in [false, true] {
            let mut terminal = engine(kitty, csi_u);
            for fragmented in [false, true] {
                terminal.reset();
                for (sequence, flags, replies) in [
                    (b"\x1b[=1u\x1b[?u".as_slice(), 1, b"\x1b[?1u".as_slice()),
                    (b"\x1b[=2u\x1b[?u", 2, b"\x1b[?2u"),
                    (b"\x1b[=3u\x1b[?u", 3, b"\x1b[?3u"),
                    (
                        b"\x1b[>1u\x1b[=2u\x1b[?u\x1b[>3u\x1b[?u\x1b[<u\x1b[?u",
                        2,
                        b"\x1b[?2u\x1b[?3u\x1b[?2u",
                    ),
                    (b"\x1b[<10u\x1b[?u", 0, b"\x1b[?0u"),
                ] {
                    let mut outbound = Vec::new();
                    if fragmented {
                        for byte in sequence {
                            outbound.extend(
                                terminal
                                    .advance(std::slice::from_ref(byte))
                                    .unwrap()
                                    .outbound,
                            );
                        }
                    } else {
                        outbound = terminal.advance(sequence).unwrap().outbound;
                    }
                    assert_eq!(
                        outbound,
                        if kitty { replies } else { b"" },
                        "kitty={kitty} csi_u={csi_u} fragmented={fragmented} flags={flags}"
                    );
                    assert_state(&mut terminal, csi_u, if kitty { flags } else { 0 }, kitty);
                }
            }
        }
    }
}

#[test]
fn synchronized_flush_paths_restore_live_flags_and_nonkeyboard_display() {
    #[derive(Clone, Copy, Debug)]
    enum Flush {
        SameBatch,
        Closing,
        Explicit,
        Expired,
        Capacity,
    }

    for kitty in [false, true] {
        for csi_u in [false, true] {
            let mut terminal = engine(kitty, csi_u);
            for flush in [
                Flush::SameBatch,
                Flush::Closing,
                Flush::Explicit,
                Flush::Expired,
                Flush::Capacity,
            ] {
                for (tail, flags, replies) in [
                    (
                        b"\x1b[=1u\x1b[?u".as_slice(),
                        1,
                        b"\x1b[?2u\x1b[?3u\x1b[?2u\x1b[?1u".as_slice(),
                    ),
                    (b"\x1b[=2u\x1b[?u", 2, b"\x1b[?2u\x1b[?3u\x1b[?2u\x1b[?2u"),
                    (b"\x1b[=3u\x1b[?u", 3, b"\x1b[?2u\x1b[?3u\x1b[?2u\x1b[?3u"),
                ] {
                    terminal.reset();
                    terminal.advance(b"seed\r\n").unwrap();
                    let before = visible_frame(&terminal);
                    assert_eq!(displayed_rows(&before), ["seed", "", ""]);
                    // 期望回复独立写定；协商与三个显示行交错，最后恢复已 set 的 frame。
                    let mut payload =
                        b"\x1b[>1u\x1b[=2u\x1b[?uone\r\n\x1b[>3u\x1b[?utwo\r\n\x1b[<u\x1b[?uthree"
                            .to_vec();
                    payload.extend_from_slice(tail);
                    let outbound = if matches!(flush, Flush::SameBatch) {
                        let mut batch = b"\x1b[?2026h".to_vec();
                        batch.extend_from_slice(&payload);
                        batch.extend_from_slice(b"\x1b[?2026l");
                        terminal.advance(&batch).unwrap().outbound
                    } else {
                        assert!(
                            terminal
                                .advance(b"\x1b[?2026h")
                                .unwrap()
                                .outbound
                                .is_empty()
                        );
                        assert!(terminal.sync_deadline().is_some());
                        // 分割在 CSI 中间，同时检查尚未 flush 的 query 和显示仍不可见。
                        for chunk in payload.chunks(7) {
                            assert!(terminal.advance(chunk).unwrap().outbound.is_empty());
                        }
                        assert!(terminal.sync_deadline().is_some());
                        assert_eq!(displayed_rows(&visible_frame(&terminal)), ["seed", "", ""]);
                        assert_shift_matrix(&mut terminal, csi_u, 0);
                        assert_super(&mut terminal, 0);
                        match flush {
                            Flush::Closing => terminal.advance(b"\x1b[?2026l").unwrap().outbound,
                            Flush::Explicit => {
                                let delta = terminal.end_sync().unwrap();
                                assert!(delta.dirty);
                                delta.outbound
                            }
                            Flush::Expired => {
                                let deadline = terminal.sync_deadline().unwrap();
                                std::thread::sleep(
                                    deadline.saturating_duration_since(std::time::Instant::now()),
                                );
                                assert!(std::time::Instant::now() >= deadline);
                                terminal.advance(b"\x1b[0m").unwrap().outbound
                            }
                            Flush::Capacity => {
                                // 固定 vte 0.15 的同步缓冲上限为 2MiB；NUL 不制造显示文本。
                                terminal.advance(&vec![0; 0x20_0000]).unwrap().outbound
                            }
                            Flush::SameBatch => unreachable!(),
                        }
                    };
                    assert_eq!(
                        outbound,
                        if kitty { replies } else { b"" },
                        "{flush:?} kitty={kitty} csi_u={csi_u} flags={flags}"
                    );
                    assert!(terminal.sync_deadline().is_none(), "{flush:?}");
                    assert_state(&mut terminal, csi_u, if kitty { flags } else { 0 }, kitty);
                    let after = visible_frame(&terminal);
                    assert_eq!(displayed_rows(&after), ["one", "two", "three"]);
                    assert_eq!(after.viewport_top, 1);
                    assert_eq!(
                        after
                            .rows
                            .iter()
                            .map(|row| row.stable_row)
                            .collect::<Vec<_>>(),
                        [1, 2, 3]
                    );
                    let cursor = after.cursor.as_ref().expect("同步刷新后应保留光标");
                    assert_eq!(cursor.position.stable_row, 3);
                    assert_eq!(cursor.position.column, 5);
                    let selected = SelectionRange {
                        start: CellPosition {
                            stable_row: 1,
                            column: 0,
                        },
                        end: CellPosition {
                            stable_row: 1,
                            column: 3,
                        },
                        rectangular: false,
                    };
                    assert_eq!(terminal.selection_text(selected), "one");
                    let matches = terminal.search(&SearchQuery {
                        needle: "two".into(),
                        case_sensitive: true,
                        regex: false,
                    });
                    assert_eq!(matches.len(), 1);
                    assert_eq!(
                        matches[0].start,
                        CellPosition {
                            stable_row: 2,
                            column: 0
                        }
                    );
                    assert_eq!(
                        matches[0].end,
                        CellPosition {
                            stable_row: 2,
                            column: 3
                        }
                    );
                    assert_eq!(displayed_rows(&before), ["seed", "", ""]);
                    let again = terminal.end_sync().unwrap();
                    assert!(!again.dirty);
                    assert!(again.outbound.is_empty());
                    terminal.advance(b"\x1b[<10u").unwrap();
                    assert_state(&mut terminal, csi_u, 0, kitty);
                    assert_eq!(
                        displayed_rows(&visible_frame(&terminal)),
                        ["one", "two", "three"]
                    );
                }
            }
        }
    }
}

#[test]
fn main_and_alternate_set_frames_survive_flush_and_reset_without_cross_contamination() {
    for kitty in [false, true] {
        for csi_u in [false, true] {
            let mut terminal = engine(kitty, csi_u);
            for synchronized in [false, true] {
                let apply = |terminal: &mut DefaultTerminalEngine, bytes: &[u8]| {
                    if synchronized {
                        terminal.advance(b"\x1b[?2026h").unwrap();
                        assert!(terminal.sync_deadline().is_some());
                        assert!(terminal.advance(bytes).unwrap().outbound.is_empty());
                        let delta = terminal.end_sync().unwrap();
                        assert!(delta.dirty);
                        assert!(terminal.sync_deadline().is_none());
                        delta.outbound
                    } else {
                        terminal.advance(bytes).unwrap().outbound
                    }
                };
                for remote_reset in [false, true] {
                    terminal.reset();
                    assert_eq!(
                        apply(&mut terminal, b"primary\x1b[>1u\x1b[=2u\x1b[?u"),
                        if kitty { b"\x1b[?2u".as_slice() } else { b"" }
                    );
                    assert_state(&mut terminal, csi_u, if kitty { 2 } else { 0 }, kitty);
                    let primary = visible_frame(&terminal);
                    assert!(!primary.alternate_screen);
                    assert_eq!(displayed_rows(&primary), ["primary", "", ""]);
                    // 1049 复制主光标；显式定位备用屏原点，不假设切屏会重置坐标。
                    assert_eq!(
                        apply(&mut terminal, b"\x1b[?1049h\x1b[H\x1b[=3u\x1b[?ualternate"),
                        if kitty { b"\x1b[?3u".as_slice() } else { b"" }
                    );
                    let alternate = visible_frame(&terminal);
                    assert!(alternate.alternate_screen);
                    assert_eq!(displayed_rows(&alternate), ["alternate", "", ""]);
                    assert_eq!(
                        apply(&mut terminal, b"\x1b[>1u\x1b[=2u\x1b[?u\x1b[<u\x1b[?u"),
                        if kitty {
                            b"\x1b[?2u\x1b[?3u".as_slice()
                        } else {
                            b""
                        }
                    );
                    assert_state(&mut terminal, csi_u, if kitty { 3 } else { 0 }, kitty);
                    assert_eq!(
                        apply(&mut terminal, b"\x1b[?1049l\x1b[?u"),
                        if kitty { b"\x1b[?2u".as_slice() } else { b"" }
                    );
                    let restored = visible_frame(&terminal);
                    assert!(!restored.alternate_screen);
                    assert_eq!(restored.rows, primary.rows);
                    assert_eq!(restored.cursor, primary.cursor);
                    assert_state(&mut terminal, csi_u, if kitty { 2 } else { 0 }, kitty);
                    assert_eq!(
                        apply(&mut terminal, b"\x1b[?1049h\x1b[?u"),
                        if kitty { b"\x1b[?3u".as_slice() } else { b"" }
                    );
                    assert!(visible_frame(&terminal).alternate_screen);
                    if remote_reset {
                        assert!(apply(&mut terminal, b"\x1bc").is_empty());
                    } else {
                        terminal.reset();
                    }
                    assert!(!visible_frame(&terminal).alternate_screen);
                    assert_state(&mut terminal, csi_u, 0, kitty);
                    assert_eq!(
                        apply(
                            &mut terminal,
                            b"\x1b[?1049h\x1b[?u\x1b[<u\x1b[?u\x1b[?1049l\x1b[?u\x1b[<10u\x1b[?u",
                        ),
                        if kitty {
                            b"\x1b[?0u\x1b[?0u\x1b[?0u\x1b[?0u".as_slice()
                        } else {
                            b""
                        }
                    );
                    assert_state(&mut terminal, csi_u, 0, kitty);
                    assert_eq!(displayed_rows(&visible_frame(&terminal)), ["", "", ""]);
                }
            }
        }
    }
}

#[test]
fn push_pop_phase_matrix_and_reset_cover_all_four_profiles() {
    for kitty in [false, true] {
        for csi_u in [false, true] {
            let mut terminal = engine(kitty, csi_u);
            assert_state(&mut terminal, csi_u, 0, kitty);
            for (sequence, flags) in [
                (b"\x1b[>1u".as_slice(), 1),
                (b"\x1b[<u", 0),
                (b"\x1b[>2u", 2),
                (b"\x1b[<u", 0),
                (b"\x1b[>3u", 3),
                (b"\x1b[<u", 0),
                (b"\x1b[>1u", 1),
                (b"\x1b[>2u", 2),
                (b"\x1b[>3u", 3),
                (b"\x1b[<u", 2),
                (b"\x1b[<u", 1),
                (b"\x1b[<u", 0),
                (b"\x1b[>1u\x1b[>3u\x1b[>2u\x1b[<2u", 1),
                (b"\x1b[<u", 0),
            ] {
                terminal.advance(sequence).unwrap();
                assert_state(&mut terminal, csi_u, if kitty { flags } else { 0 }, kitty);
            }
            terminal.advance(b"\x1b[>1u\x1b[>3u").unwrap();
            terminal.reset();
            assert_state(&mut terminal, csi_u, 0, kitty);
            terminal.advance(b"\x1b[<u").unwrap();
            assert_state(&mut terminal, csi_u, 0, kitty);
            terminal.advance(b"\x1b[>3u\x1bc").unwrap();
            assert_state(&mut terminal, csi_u, 0, kitty);
            terminal.advance(b"\x1b[<u").unwrap();
            assert_state(&mut terminal, csi_u, 0, kitty);
            terminal.advance(b"\x1b[>3u").unwrap();
            let mut fresh = engine(kitty, csi_u);
            assert_state(&mut fresh, csi_u, 0, kitty);
        }
    }
}

#[test]
fn real_engine_four_profiles_negotiate_push_set_pop_query_and_reset() {
    for kitty in [false, true] {
        for csi_u in [false, true] {
            let mut terminal = engine(kitty, csi_u);
            assert_state(&mut terminal, csi_u, 0, kitty);
            for (sequence, flags) in [
                (b"\x1b[>1u".as_slice(), 1),
                (b"\x1b[>2u", 2),
                (b"\x1b[>3u", 3),
            ] {
                terminal.advance(sequence).unwrap();
                assert_state(&mut terminal, csi_u, if kitty { flags } else { 0 }, kitty);
                terminal.advance(b"\x1b[<u").unwrap();
                assert_state(&mut terminal, csi_u, 0, kitty);
            }
            // 替换当前 flags，再嵌套 push；pop 必须恢复被替换后的状态。
            for (sequence, flags) in [
                (b"\x1b[>1u".as_slice(), 1),
                (b"\x1b[=2u", 2),
                (b"\x1b[>3u", 3),
                (b"\x1b[>1u", 1),
                (b"\x1b[<u", 3),
                (b"\x1b[<u", 2),
                (b"\x1b[<u", 0),
                (b"\x1b[>3u", 3),
                (b"\x1b[=0u", 0),
                (b"\x1b[<u", 0),
            ] {
                terminal.advance(sequence).unwrap();
                assert_state(&mut terminal, csi_u, if kitty { flags } else { 0 }, kitty);
            }
            terminal
                .advance(b"\x1b[>1u\x1b[>3u\x1b[>2u\x1b[<2u")
                .unwrap();
            assert_state(&mut terminal, csi_u, u8::from(kitty), kitty);
            terminal.advance(b"\x1b[<u").unwrap();
            assert_state(&mut terminal, csi_u, 0, kitty);

            terminal.advance(b"\x1b[>1u\x1b[>3u").unwrap();
            terminal.reset();
            assert_state(&mut terminal, csi_u, 0, kitty);
            terminal.advance(b"\x1b[<u").unwrap();
            assert_state(&mut terminal, csi_u, 0, kitty);
            terminal.advance(b"\x1b[>3u\x1bc").unwrap();
            assert_state(&mut terminal, csi_u, 0, kitty);
            terminal.advance(b"\x1b[<u").unwrap();
            assert_state(&mut terminal, csi_u, 0, kitty);
            terminal.advance(b"\x1b[>3u").unwrap();
            let mut fresh = engine(kitty, csi_u);
            assert_state(&mut fresh, csi_u, 0, kitty);
        }
    }
}

#[test]
fn other_flags_do_not_enable_event_types_or_super() {
    for csi_u in [false, true] {
        let mut terminal = engine(true, csi_u);
        for (sequence, flags) in [
            (b"\x1b[>4u".as_slice(), 4),
            (b"\x1b[>8u", 8),
            (b"\x1b[>16u", 16),
            (b"\x1b[>28u", 28),
            (b"\x1b[>29u", 29),
            (b"\x1b[>30u", 30),
        ] {
            terminal.advance(sequence).unwrap();
            assert_state(&mut terminal, csi_u, flags, true);
            terminal.advance(b"\x1b[<u").unwrap();
            assert_state(&mut terminal, csi_u, 0, true);
        }
        // 现有 parser 的增设/清除语义也须反映到编码当时，而非缓存增强布尔值。
        for (sequence, flags) in [
            (b"\x1b[=1u".as_slice(), 1),
            (b"\x1b[=2;2u", 3),
            (b"\x1b[=1;3u", 2),
            (b"\x1b[=2;3u", 0),
        ] {
            terminal.advance(sequence).unwrap();
            assert_state(&mut terminal, csi_u, flags, true);
        }
    }
}

#[test]
fn set_flags_query_and_nested_pop_agree_with_live_encoding() {
    let mut terminal = engine(true, false);
    terminal.advance(b"\x1b[>1u\x1b[=2u").unwrap();
    let case = &SHIFT_CASES[0];
    let repeat = event(
        KeyCode::Character(case.code),
        KeyCode::Character(case.legacy),
        case.modifiers,
        KeyEventPhase::Repeat,
    );
    let after_set = terminal.encode_key_event(repeat.clone()).unwrap();
    let set_query = terminal.advance(b"\x1b[?u").unwrap().outbound;
    terminal.advance(b"\x1b[>3u\x1b[<u").unwrap();
    let after_pop = terminal.encode_key_event(repeat).unwrap();
    let pop_query = terminal.advance(b"\x1b[?u").unwrap().outbound;
    assert_eq!(
        (after_set, set_query, after_pop, pop_query),
        (
            b"\x1b[54;6:2u".to_vec(),
            b"\x1b[?2u".to_vec(),
            b"\x1b[54;6:2u".to_vec(),
            b"\x1b[?2u".to_vec(),
        ),
        "set 必须替换当前栈帧，query 与嵌套 pop 必须对应编码当时的 flags"
    );
}

#[test]
fn empty_stack_set_replacement_preserves_native_push_capacity() {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use alacritty_terminal::{
        Term,
        event::VoidListener,
        grid::Dimensions,
        term::{Config, TermMode},
        vte::ansi::{Handler, KeyboardModes, Processor},
    };

    struct TestSize;

    impl Dimensions for TestSize {
        fn total_lines(&self) -> usize {
            3
        }

        fn screen_lines(&self) -> usize {
            3
        }

        fn columns(&self) -> usize {
            20
        }
    }

    // 容量来自固定 0.26.0 的原生栈上限；所有 push 均由原 Processor 实际解析。
    let pushes = b"\x1b[>1u".repeat(4096);
    let exercise = |replace_frame: bool| {
        catch_unwind(AssertUnwindSafe(|| {
            let mut terminal = Term::new(
                Config {
                    kitty_keyboard: true,
                    ..Config::default()
                },
                &TestSize,
                VoidListener,
            );
            let mut processor: Processor = Processor::new();
            processor.advance(&mut terminal, b"\x1b[=2u");
            assert_eq!(
                *terminal.mode() & TermMode::KITTY_KEYBOARD_PROTOCOL,
                TermMode::REPORT_EVENT_TYPES
            );
            if replace_frame {
                // 仅测试 set2 的 typed 候选原生操作；不接生产 Handler，也不维护深度。
                let mut flags = KeyboardModes::NO_MODE;
                flags.set(
                    KeyboardModes::REPORT_EVENT_TYPES,
                    terminal.mode().contains(TermMode::REPORT_EVENT_TYPES),
                );
                Handler::pop_keyboard_modes(&mut terminal, 1);
                Handler::push_keyboard_mode(&mut terminal, flags);
            }
            processor.advance(&mut terminal, &pushes);
            assert_eq!(
                *terminal.mode() & TermMode::KITTY_KEYBOARD_PROTOCOL,
                TermMode::DISAMBIGUATE_ESC_CODES
            );
        }))
    };
    let baseline = exercise(false);
    let candidate = exercise(true);
    eprintln!(
        "原 set2 + 4096 次原生 push1: 不 panic={}; 测试候选替换 + 4096 次原生 push1: 不 panic={}",
        baseline.is_ok(),
        candidate.is_ok()
    );
    assert!(baseline.is_ok(), "原 set2 必须允许随后 4096 次原生 push");
    assert!(
        candidate.is_ok(),
        "set 替换候选不得降低原生 push 容量或引入提前 panic"
    );
}

#[test]
fn kitty_functional_mapping_all_existing_keys_and_unmodified_phases() {
    // 每个键分别写定 Press/Repeat/Release 与旧入口的无修饰字节。
    let cases = [
        (
            KeyCode::Escape,
            "\x1b[27u",
            "\x1b[27;1:2u",
            "\x1b[27;1:3u",
            "\x1b[27u",
        ),
        (
            KeyCode::Insert,
            "\x1b[2~",
            "\x1b[2;1:2~",
            "\x1b[2;1:3~",
            "\x1b[2~",
        ),
        (
            KeyCode::Delete,
            "\x1b[3~",
            "\x1b[3;1:2~",
            "\x1b[3;1:3~",
            "\x1b[3~",
        ),
        (
            KeyCode::Home,
            "\x1b[H",
            "\x1b[1;1:2H",
            "\x1b[1;1:3H",
            "\x1b[H",
        ),
        (
            KeyCode::End,
            "\x1b[F",
            "\x1b[1;1:2F",
            "\x1b[1;1:3F",
            "\x1b[F",
        ),
        (
            KeyCode::PageUp,
            "\x1b[5~",
            "\x1b[5;1:2~",
            "\x1b[5;1:3~",
            "\x1b[5~",
        ),
        (
            KeyCode::PageDown,
            "\x1b[6~",
            "\x1b[6;1:2~",
            "\x1b[6;1:3~",
            "\x1b[6~",
        ),
        (
            KeyCode::ArrowUp,
            "\x1b[A",
            "\x1b[1;1:2A",
            "\x1b[1;1:3A",
            "\x1b[A",
        ),
        (
            KeyCode::ArrowDown,
            "\x1b[B",
            "\x1b[1;1:2B",
            "\x1b[1;1:3B",
            "\x1b[B",
        ),
        (
            KeyCode::ArrowRight,
            "\x1b[C",
            "\x1b[1;1:2C",
            "\x1b[1;1:3C",
            "\x1b[C",
        ),
        (
            KeyCode::ArrowLeft,
            "\x1b[D",
            "\x1b[1;1:2D",
            "\x1b[1;1:3D",
            "\x1b[D",
        ),
        (
            KeyCode::F(1),
            "\x1b[P",
            "\x1b[1;1:2P",
            "\x1b[1;1:3P",
            "\x1bOP",
        ),
        (
            KeyCode::F(2),
            "\x1b[Q",
            "\x1b[1;1:2Q",
            "\x1b[1;1:3Q",
            "\x1bOQ",
        ),
        (
            KeyCode::F(3),
            "\x1b[13~",
            "\x1b[13;1:2~",
            "\x1b[13;1:3~",
            "\x1bOR",
        ),
        (
            KeyCode::F(4),
            "\x1b[S",
            "\x1b[1;1:2S",
            "\x1b[1;1:3S",
            "\x1bOS",
        ),
        (
            KeyCode::F(5),
            "\x1b[15~",
            "\x1b[15;1:2~",
            "\x1b[15;1:3~",
            "\x1b[15~",
        ),
        (
            KeyCode::F(6),
            "\x1b[17~",
            "\x1b[17;1:2~",
            "\x1b[17;1:3~",
            "\x1b[17~",
        ),
        (
            KeyCode::F(7),
            "\x1b[18~",
            "\x1b[18;1:2~",
            "\x1b[18;1:3~",
            "\x1b[18~",
        ),
        (
            KeyCode::F(8),
            "\x1b[19~",
            "\x1b[19;1:2~",
            "\x1b[19;1:3~",
            "\x1b[19~",
        ),
        (
            KeyCode::F(9),
            "\x1b[20~",
            "\x1b[20;1:2~",
            "\x1b[20;1:3~",
            "\x1b[20~",
        ),
        (
            KeyCode::F(10),
            "\x1b[21~",
            "\x1b[21;1:2~",
            "\x1b[21;1:3~",
            "\x1b[21~",
        ),
        (
            KeyCode::F(11),
            "\x1b[23~",
            "\x1b[23;1:2~",
            "\x1b[23;1:3~",
            "\x1b[23~",
        ),
        (
            KeyCode::F(12),
            "\x1b[24~",
            "\x1b[24;1:2~",
            "\x1b[24;1:3~",
            "\x1b[24~",
        ),
        (
            KeyCode::F(13),
            "\x1b[57376u",
            "\x1b[57376;1:2u",
            "\x1b[57376;1:3u",
            "\x1b[25~",
        ),
        (
            KeyCode::F(14),
            "\x1b[57377u",
            "\x1b[57377;1:2u",
            "\x1b[57377;1:3u",
            "\x1b[26~",
        ),
        (
            KeyCode::F(15),
            "\x1b[57378u",
            "\x1b[57378;1:2u",
            "\x1b[57378;1:3u",
            "\x1b[28~",
        ),
        (
            KeyCode::F(16),
            "\x1b[57379u",
            "\x1b[57379;1:2u",
            "\x1b[57379;1:3u",
            "\x1b[29~",
        ),
        (
            KeyCode::F(17),
            "\x1b[57380u",
            "\x1b[57380;1:2u",
            "\x1b[57380;1:3u",
            "\x1b[31~",
        ),
        (
            KeyCode::F(18),
            "\x1b[57381u",
            "\x1b[57381;1:2u",
            "\x1b[57381;1:3u",
            "\x1b[32~",
        ),
        (
            KeyCode::F(19),
            "\x1b[57382u",
            "\x1b[57382;1:2u",
            "\x1b[57382;1:3u",
            "\x1b[33~",
        ),
        (
            KeyCode::F(20),
            "\x1b[57383u",
            "\x1b[57383;1:2u",
            "\x1b[57383;1:3u",
            "\x1b[34~",
        ),
        (
            KeyCode::F(21),
            "\x1b[57384u",
            "\x1b[57384;1:2u",
            "\x1b[57384;1:3u",
            "\x1b[42~",
        ),
        (
            KeyCode::F(22),
            "\x1b[57385u",
            "\x1b[57385;1:2u",
            "\x1b[57385;1:3u",
            "\x1b[43~",
        ),
        (
            KeyCode::F(23),
            "\x1b[57386u",
            "\x1b[57386;1:2u",
            "\x1b[57386;1:3u",
            "\x1b[44~",
        ),
        (
            KeyCode::F(24),
            "\x1b[57387u",
            "\x1b[57387;1:2u",
            "\x1b[57387;1:3u",
            "\x1b[45~",
        ),
    ];
    let mut terminal = engine(true, false);
    for (sequence, flags) in [(b"\x1b[>1u".as_slice(), 1), (b"\x1b[>3u", 3)] {
        terminal.advance(sequence).unwrap();
        query(&mut terminal, flags);
        for (code, press, repeat, release, old) in &cases {
            for (index, phase) in PHASES.into_iter().enumerate() {
                let expected = if flags == 1 {
                    [*press, *press, ""][index]
                } else {
                    [*press, *repeat, *release][index]
                };
                assert_eq!(
                    terminal
                        .encode_key_event(same_key(code.clone(), KeyModifiers::default(), phase))
                        .unwrap(),
                    expected.as_bytes(),
                    "{code:?} {phase:?} flags={flags}"
                );
            }
            assert_eq!(
                terminal
                    .encode_input(TerminalInput::Key {
                        code: code.clone(),
                        modifiers: KeyModifiers::default()
                    })
                    .unwrap(),
                old.as_bytes()
            );
        }
        terminal.advance(b"\x1b[<u").unwrap();
    }
}

#[test]
fn super_modifier_bits_unicode_cursor_and_tilde_event_fields() {
    let mut terminal = engine(true, false);
    terminal.advance(b"\x1b[>3u").unwrap();
    for (shift, alt, control, expected) in [
        (false, false, false, "\x1b[97;9u"),
        (true, false, false, "\x1b[97;10u"),
        (false, true, false, "\x1b[97;11u"),
        (true, true, false, "\x1b[97;12u"),
        (false, false, true, "\x1b[97;13u"),
        (true, false, true, "\x1b[97;14u"),
        (false, true, true, "\x1b[97;15u"),
        (true, true, true, "\x1b[97;16u"),
    ] {
        assert_eq!(
            terminal
                .encode_key_event(event(
                    KeyCode::Character('a'),
                    KeyCode::Character(if shift { 'A' } else { 'a' }),
                    KeyModifiers {
                        shift,
                        alt,
                        control,
                        super_key: true
                    },
                    KeyEventPhase::Press
                ))
                .unwrap(),
            expected.as_bytes()
        );
    }
    let super_key = KeyModifiers {
        super_key: true,
        ..KeyModifiers::default()
    };
    for (code, phase, bytes) in [
        (KeyCode::ArrowUp, KeyEventPhase::Repeat, "\x1b[1;9:2A"),
        (KeyCode::Delete, KeyEventPhase::Release, "\x1b[3;9:3~"),
        (KeyCode::F(3), KeyEventPhase::Repeat, "\x1b[13;9:2~"),
        (KeyCode::F(24), KeyEventPhase::Release, "\x1b[57387;9:3u"),
        (
            KeyCode::Character('界'),
            KeyEventPhase::Press,
            "\x1b[30028;9u",
        ),
        (
            KeyCode::Character('界'),
            KeyEventPhase::Repeat,
            "\x1b[30028;9:2u",
        ),
        (
            KeyCode::Character('界'),
            KeyEventPhase::Release,
            "\x1b[30028;9:3u",
        ),
        (
            KeyCode::Character('é'),
            KeyEventPhase::Repeat,
            "\x1b[233;9:2u",
        ),
    ] {
        assert_eq!(
            terminal
                .encode_key_event(same_key(code, super_key, phase))
                .unwrap(),
            bytes.as_bytes()
        );
    }
    for sequence in [b"\x1b[?1h".as_slice(), b"\x1b[?1l"] {
        terminal.advance(sequence).unwrap();
        assert_eq!(
            terminal
                .encode_key_event(same_key(KeyCode::ArrowUp, super_key, KeyEventPhase::Repeat))
                .unwrap(),
            b"\x1b[1;9:2A"
        );
        assert_eq!(
            terminal
                .encode_key_event(same_key(
                    KeyCode::ArrowUp,
                    KeyModifiers::default(),
                    KeyEventPhase::Press
                ))
                .unwrap(),
            b"\x1b[A"
        );
        assert_eq!(
            terminal
                .encode_input(TerminalInput::Key {
                    code: KeyCode::ArrowUp,
                    modifiers: KeyModifiers::default()
                })
                .unwrap(),
            if sequence == b"\x1b[?1h" {
                b"\x1bOA"
            } else {
                b"\x1b[A"
            }
        );
    }
    terminal.advance(b"\x1b[<u\x1b[?1h").unwrap();
    for phase in [KeyEventPhase::Press, KeyEventPhase::Repeat] {
        assert_eq!(
            terminal
                .encode_key_event(same_key(KeyCode::ArrowUp, KeyModifiers::default(), phase))
                .unwrap(),
            b"\x1bOA"
        );
    }
    terminal.advance(b"\x1b[>2u").unwrap();
    assert_eq!(
        terminal
            .encode_key_event(same_key(
                KeyCode::ArrowUp,
                KeyModifiers::default(),
                KeyEventPhase::Repeat
            ))
            .unwrap(),
        b"\x1b[1;1:2A"
    );
    assert_eq!(
        terminal
            .encode_key_event(same_key(
                KeyCode::Escape,
                KeyModifiers::default(),
                KeyEventPhase::Press
            ))
            .unwrap(),
        b"\x1b"
    );
    assert_eq!(
        terminal
            .encode_key_event(same_key(
                KeyCode::Escape,
                KeyModifiers::default(),
                KeyEventPhase::Repeat
            ))
            .unwrap(),
        b"\x1b[27;1:2u"
    );
}

#[test]
fn recovery_keys_release_exception_and_text_stays_on_legacy_path() {
    let mut terminal = engine(true, false);
    for (sequence, flags) in [
        (b"\x1b[>1u".as_slice(), 1),
        (b"\x1b[>2u", 2),
        (b"\x1b[>3u", 3),
    ] {
        terminal.advance(sequence).unwrap();
        for (code, legacy, repeated) in [
            (KeyCode::Enter, b"\r".as_slice(), b"\x1b[13;1:2u".as_slice()),
            (KeyCode::Tab, b"\t", b"\x1b[9;1:2u"),
            (KeyCode::Backspace, b"\x7f", b"\x1b[127;1:2u"),
        ] {
            assert_eq!(
                terminal
                    .encode_key_event(same_key(
                        code.clone(),
                        KeyModifiers::default(),
                        KeyEventPhase::Press
                    ))
                    .unwrap(),
                legacy
            );
            assert_eq!(
                terminal
                    .encode_key_event(same_key(
                        code.clone(),
                        KeyModifiers::default(),
                        KeyEventPhase::Repeat
                    ))
                    .unwrap(),
                if flags & 2 != 0 { repeated } else { legacy }
            );
            for modifiers in [
                KeyModifiers::default(),
                KeyModifiers {
                    super_key: true,
                    ..KeyModifiers::default()
                },
            ] {
                assert!(
                    terminal
                        .encode_key_event(same_key(code.clone(), modifiers, KeyEventPhase::Release))
                        .unwrap()
                        .is_empty()
                );
            }
        }
        // flag 2 不扩展纯文本键，也不从未移位身份制造第二份文本。
        for (code, legacy, shift, expected) in [
            ('a', 'A', true, "A"),
            ('界', '界', false, "界"),
            ('é', 'É', true, "É"),
        ] {
            for phase in PHASES {
                assert_eq!(
                    terminal
                        .encode_key_event(event(
                            KeyCode::Character(code),
                            KeyCode::Character(legacy),
                            KeyModifiers {
                                shift,
                                ..KeyModifiers::default()
                            },
                            phase
                        ))
                        .unwrap(),
                    if phase == KeyEventPhase::Release {
                        b""
                    } else {
                        expected.as_bytes()
                    }
                );
            }
        }
        assert_eq!(
            terminal
                .encode_input(TerminalInput::CommittedText("原生输入😀".into()))
                .unwrap(),
            "原生输入😀".as_bytes()
        );
        terminal.advance(b"\x1b[<u").unwrap();
    }
    // 只利用已存在的 flag 8 判定恢复键 release 例外，不扩展其他增强能力。
    terminal.advance(b"\x1b[>11u").unwrap();
    for (code, expected) in [
        (KeyCode::Enter, b"\x1b[13;1:3u".as_slice()),
        (KeyCode::Tab, b"\x1b[9;1:3u"),
        (KeyCode::Backspace, b"\x1b[127;1:3u"),
    ] {
        assert_eq!(
            terminal
                .encode_key_event(same_key(
                    code,
                    KeyModifiers::default(),
                    KeyEventPhase::Release
                ))
                .unwrap(),
            expected
        );
    }
}

#[test]
fn invalid_noncharacter_pairs_and_out_of_range_functions_are_rejected() {
    let mut terminal = engine(true, false);
    for sequence in [b"".as_slice(), b"\x1b[>1u", b"\x1b[=2u", b"\x1b[=3u"] {
        terminal.advance(sequence).unwrap();
        for phase in PHASES {
            for (code, legacy) in [
                (KeyCode::Enter, KeyCode::Tab),
                (KeyCode::Character('a'), KeyCode::ArrowUp),
                (KeyCode::ArrowUp, KeyCode::Character('a')),
            ] {
                assert_eq!(
                    terminal.encode_key_event(event(code, legacy, KeyModifiers::default(), phase)),
                    Err(EngineError::UnsupportedInput(
                        "inconsistent key representations"
                    ))
                );
            }
            for number in [0, 25, 255] {
                assert_eq!(
                    terminal.encode_key_event(same_key(
                        KeyCode::F(number),
                        KeyModifiers::default(),
                        phase
                    )),
                    Err(EngineError::UnsupportedInput("function key outside F1-F24"))
                );
                assert_eq!(
                    terminal.encode_input(TerminalInput::Key {
                        code: KeyCode::F(number),
                        modifiers: KeyModifiers::default()
                    }),
                    Err(EngineError::UnsupportedInput("function key outside F1-F24"))
                );
            }
        }
    }
}

// 仅实现旧 trait 必需方法；新入口必须在 trait object 上走默认实现。
struct LegacyEngine {
    inner: DefaultTerminalEngine,
    received: Vec<TerminalInput>,
}

impl TerminalEngine for LegacyEngine {
    fn display_modes(&self) -> TerminalDisplayModes {
        self.inner.display_modes()
    }
    fn recover_display(&mut self) -> Result<DisplayRecovery, EngineError> {
        self.inner.recover_display()
    }
    fn advance(&mut self, bytes: &[u8]) -> Result<EngineDelta, EngineError> {
        self.inner.advance(bytes)
    }
    fn resize(&mut self, size: TerminalSize) -> Result<(), EngineError> {
        TerminalEngine::resize(&mut self.inner, size)
    }
    fn render(
        &mut self,
        viewport: Viewport,
        selection: Option<SelectionRange>,
    ) -> Result<Arc<RenderFrame>, EngineError> {
        self.inner.render(viewport, selection)
    }
    fn encode_input(&mut self, input: TerminalInput) -> Result<Vec<u8>, EngineError> {
        self.received.push(input.clone());
        self.inner.encode_input(input)
    }
    fn encode_mouse(&mut self, input: TerminalMouseEvent) -> Result<Vec<u8>, EngineError> {
        self.inner.encode_mouse(input)
    }
    fn clear_scrollback(&mut self) -> Result<(), EngineError> {
        TerminalEngine::clear_scrollback(&mut self.inner)
    }
    fn scroll(&mut self, delta_rows: i32) -> Result<(), EngineError> {
        self.inner.scroll(delta_rows)
    }
    fn viewport_bounds(&self) -> ViewportBounds {
        self.inner.viewport_bounds()
    }
    fn search(&self, query: &SearchQuery) -> Result<Vec<SearchMatch>, EngineError> {
        TerminalEngine::search(&self.inner, query)
    }
    fn selected_text(&self, range: SelectionRange) -> Result<String, EngineError> {
        self.inner.selected_text(range)
    }
}

#[test]
fn old_implementer_default_method_passes_legacy_code_through_trait_object() {
    let mut legacy = LegacyEngine {
        inner: engine(true, true),
        received: Vec::new(),
    };
    for (sequence, flags) in [(b"".as_slice(), 0), (b"\x1b[>3u", 3), (b"\x1b[<u", 0)] {
        let object: &mut dyn TerminalEngine = &mut legacy;
        object.advance(sequence).unwrap();
        for case in &SHIFT_CASES {
            for phase in PHASES {
                let expected = if phase == KeyEventPhase::Release {
                    b"".as_slice()
                } else if flags == 3 {
                    case.old_kitty
                } else {
                    case.fallback[1]
                };
                assert_eq!(
                    object
                        .encode_key_event(event(
                            KeyCode::Character(case.code),
                            KeyCode::Character(case.legacy),
                            case.modifiers,
                            phase
                        ))
                        .unwrap(),
                    expected
                );
            }
        }
        for phase in PHASES {
            assert_eq!(
                object.encode_key_event(same_key(
                    KeyCode::Character('a'),
                    KeyModifiers {
                        super_key: true,
                        ..KeyModifiers::default()
                    },
                    phase
                )),
                Err(EngineError::UnsupportedInput("super-modified key"))
            );
        }
    }
    let expected: Vec<TerminalInput> = (0..3)
        .flat_map(|_| {
            SHIFT_CASES.iter().flat_map(|case| {
                let input = TerminalInput::Key {
                    code: KeyCode::Character(case.legacy),
                    modifiers: case.modifiers,
                };
                [input.clone(), input]
            })
        })
        .collect();
    assert_eq!(legacy.received, expected);

    let mut real: Box<dyn TerminalEngine> = Box::new(engine(true, false));
    real.advance(b"\x1b[>3u").unwrap();
    assert_eq!(
        real.encode_key_event(same_key(
            KeyCode::ArrowUp,
            KeyModifiers {
                super_key: true,
                ..KeyModifiers::default()
            },
            KeyEventPhase::Repeat
        ))
        .unwrap(),
        b"\x1b[1;9:2A"
    );
}

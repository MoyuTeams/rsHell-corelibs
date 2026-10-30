//! Modifier-key contract checks: what reaches the PTY, and what the actor does
//! when the encoder refuses a key.
//!
//! Two independent concerns live here:
//!
//! 1. `DefaultTerminalEngine::encode_input` returns `EngineError::UnsupportedInput`
//!    for keys the xterm modifier table cannot represent. These tests pin the
//!    actor's reaction, which must never be "kill the session".
//! 2. Standalone CSI-u (fixterms) and negotiated Kitty flag 1 encode shifted
//!    keys differently, without changing legacy input before negotiation.

mod support;

use std::time::Duration;

use rshell_core::{
    CellPosition, KeyCode, KeyModifiers, MouseButton, MouseEventKind, SessionState, TerminalInput,
    TerminalMouseEvent, TerminalOverrides, TerminalSettingsV1, TerminalSize,
};
use rshell_session::{
    DefaultTerminalEngine, SessionCommand, SessionEvent, SessionLaunch, SessionManager,
    TerminalEngine, TransportEvent, TransportRequest,
};
use support::{FakeFactory, TransportScript};

const WAIT: Duration = Duration::from_secs(2);

fn size() -> TerminalSize {
    TerminalSize {
        cols: 20,
        rows: 3,
        pixel_width: 160,
        pixel_height: 48,
        dpi: 96,
    }
}

fn control() -> KeyModifiers {
    KeyModifiers {
        control: true,
        ..KeyModifiers::default()
    }
}

fn key(code: KeyCode, modifiers: KeyModifiers) -> TerminalInput {
    TerminalInput::Key { code, modifiers }
}

fn control_shift() -> KeyModifiers {
    KeyModifiers {
        shift: true,
        control: true,
        ..KeyModifiers::default()
    }
}

fn engine_with(change: impl FnOnce(&mut TerminalSettingsV1)) -> DefaultTerminalEngine {
    let mut settings = TerminalSettingsV1::default();
    change(&mut settings);
    DefaultTerminalEngine::new(&settings.resolve(&TerminalOverrides::default()), size()).unwrap()
}

async fn connected_manager() -> (
    SessionManager,
    support::FactoryProbe,
    rshell_session::SessionClient,
    support::EventStream,
) {
    let (script, stream) = TransportScript::controlled();
    let (factory, probe) = FakeFactory::new([script]);
    let manager = SessionManager::new(factory);
    let terminal = TerminalSettingsV1::default().resolve(&TerminalOverrides::default());
    let engine = DefaultTerminalEngine::new(&terminal, size()).expect("engine");
    let launch = SessionLaunch::new(TransportRequest::new(size()), Box::new(engine));
    let mut client = manager.launch(launch).expect("launch");
    stream.send(TransportEvent::Connected);
    tokio::time::timeout(WAIT, async {
        loop {
            if let SessionEvent::StateChanged(SessionState::Connected) =
                client.events.recv().await.expect("event stream closed")
            {
                break;
            }
        }
    })
    .await
    .expect("state timeout");
    (manager, probe, client, stream)
}

async fn ignored_command_keeps_session_alive(command: SessionCommand, mouse_reporting: bool) {
    let (manager, probe, mut client, stream) = connected_manager().await;
    if mouse_reporting {
        client.frames.borrow_and_update();
        stream.send(TransportEvent::Output(b"\x1b[?1000h\x1b[?1006h".to_vec()));
        tokio::time::timeout(WAIT, async {
            loop {
                client.frames.changed().await.expect("frame channel closed");
                if client
                    .frames
                    .borrow_and_update()
                    .as_ref()
                    .expect("frame")
                    .mouse_reporting
                {
                    break;
                }
            }
        })
        .await
        .expect("mouse reporting frame timeout");
    }

    client.try_command(command).expect("invalid command queued");
    client
        .try_command(SessionCommand::Input(key(
            KeyCode::Character('v'),
            KeyModifiers::default(),
        )))
        .expect("valid command queued");
    tokio::time::timeout(WAIT, async {
        while probe.writes().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("valid input was not written after ignored command");
    assert_eq!(probe.writes(), vec![(1, b"v".to_vec())]);
    manager.shutdown_all().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ctrl_digit_must_not_terminate_the_session() {
    ignored_command_keeps_session_alive(
        SessionCommand::Input(key(KeyCode::Character('1'), control())),
        false,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn super_modified_key_must_not_terminate_the_session() {
    ignored_command_keeps_session_alive(
        SessionCommand::Input(key(
            KeyCode::Character('x'),
            KeyModifiers {
                super_key: true,
                ..KeyModifiers::default()
            },
        )),
        false,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mouse_side_button_must_not_terminate_the_session() {
    ignored_command_keeps_session_alive(
        SessionCommand::Mouse(TerminalMouseEvent {
            kind: MouseEventKind::Press,
            button: Some(MouseButton::Back),
            cell: CellPosition {
                stable_row: 0,
                column: 0,
            },
            viewport_row: 0,
            pixel_x: 0,
            pixel_y: 0,
            modifiers: KeyModifiers::default(),
        }),
        true,
    )
    .await;
}

#[test]
fn kitty_disambiguate_reports_escape_and_alt_as_csi_u() {
    let mut engine = engine_with(|settings| settings.enable_kitty_keyboard = true);
    assert_eq!(
        engine
            .encode_input(key(KeyCode::Escape, KeyModifiers::default()))
            .unwrap(),
        b"\x1b"
    );
    assert_eq!(
        engine
            .encode_input(key(
                KeyCode::Character('x'),
                KeyModifiers {
                    alt: true,
                    ..KeyModifiers::default()
                },
            ))
            .unwrap(),
        b"\x1bx"
    );
    assert_eq!(
        engine
            .encode_input(key(KeyCode::Character('A'), control_shift()))
            .unwrap(),
        b"\x01",
        "enabling Kitty must not change input until an application negotiates it"
    );
    engine.advance(b"\x1b[>1u").expect("push kitty flags");
    assert_eq!(
        engine.advance(b"\x1b[?u").expect("query").outbound,
        b"\x1b[?1u",
        "the terminal must not ACK a protocol it does not implement"
    );

    // kitty: "Turning on this flag will cause the terminal to report the Esc,
    // alt+key, ctrl+key, ctrl+alt+key and shift+alt+key keys using CSI u".
    assert_eq!(
        engine
            .encode_input(key(KeyCode::Escape, KeyModifiers::default()))
            .unwrap(),
        b"\x1b[27u"
    );
    assert_eq!(
        engine
            .encode_input(key(
                KeyCode::Character('x'),
                KeyModifiers {
                    alt: true,
                    ..KeyModifiers::default()
                },
            ))
            .unwrap(),
        b"\x1b[120;3u"
    );
    assert_eq!(
        engine
            .encode_input(key(KeyCode::Character('A'), control_shift()))
            .unwrap(),
        b"\x1b[97;6u"
    );
}

#[test]
fn default_keyboard_keeps_legacy_sequences() {
    let mut engine = engine_with(|_| {});
    assert_eq!(
        engine
            .encode_input(key(KeyCode::Escape, KeyModifiers::default()))
            .unwrap(),
        b"\x1b"
    );
    assert_eq!(
        engine
            .encode_input(key(KeyCode::Character('A'), control_shift()))
            .unwrap(),
        b"\x01"
    );
}

#[test]
fn standalone_csi_u_uses_fixterms_shifted_codepoint() {
    let mut engine = engine_with(|settings| settings.enable_csi_u = true);

    // Fixterms carries Shift through the character codepoint, unlike Kitty.
    assert_eq!(
        engine
            .encode_input(key(KeyCode::Character('A'), control_shift()))
            .unwrap(),
        b"\x1b[65;5u"
    );
    assert_eq!(
        engine
            .encode_input(key(KeyCode::Character('!'), control_shift()))
            .unwrap(),
        b"\x1b[33;5u",
        "Shift+1 is represented by the printable '!' codepoint"
    );
    // Fixterms explicitly retains Shift for Space: it does not change codepoint.
    assert_eq!(
        engine
            .encode_input(key(KeyCode::Character(' '), control_shift()))
            .unwrap(),
        b"\x1b[32;6u"
    );
}

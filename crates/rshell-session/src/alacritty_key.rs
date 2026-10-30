use alacritty_terminal::term::TermMode;
use rshell_core::{KeyCode, KeyEventPhase, KeyModifiers, TerminalKeyEvent};

use crate::EngineError;

const FUNCTION_CODES: [u8; 20] = [
    15, 17, 18, 19, 20, 21, 23, 24, 25, 26, 28, 29, 31, 32, 33, 34, 42, 43, 44, 45,
];

pub(crate) fn encode(
    code: KeyCode,
    modifiers: KeyModifiers,
    mode: TermMode,
    csi_u: bool,
) -> Result<Vec<u8>, EngineError> {
    if modifiers.super_key {
        return Err(EngineError::UnsupportedInput("super-modified key"));
    }
    let kitty = mode.contains(TermMode::DISAMBIGUATE_ESC_CODES);
    let protocol = csi_u || kitty;
    match code {
        KeyCode::Character(character) => encode_character(character, modifiers, csi_u, kitty),
        KeyCode::Enter => Ok(prefixed(b"\r", modifiers.alt)),
        KeyCode::Escape if kitty && no_modifiers(modifiers) => Ok(b"\x1b[27u".to_vec()),
        KeyCode::Escape if kitty => {
            Ok(format!("\x1b[27;{}u", modifier_parameter(modifiers)).into())
        }
        KeyCode::Escape => Ok(prefixed(b"\x1b", modifiers.alt)),
        KeyCode::Tab => Ok(encode_tab(modifiers, protocol)),
        KeyCode::Backspace => Ok(prefixed(
            if modifiers.control { b"\x08" } else { b"\x7f" },
            modifiers.alt,
        )),
        KeyCode::Delete => Ok(tilde(3, modifiers)),
        KeyCode::Insert => Ok(tilde(2, modifiers)),
        KeyCode::Home => Ok(cursor_key(b'H', modifiers, mode)),
        KeyCode::End => Ok(cursor_key(b'F', modifiers, mode)),
        KeyCode::PageUp => Ok(tilde(5, modifiers)),
        KeyCode::PageDown => Ok(tilde(6, modifiers)),
        KeyCode::ArrowUp => Ok(cursor_key(b'A', modifiers, mode)),
        KeyCode::ArrowDown => Ok(cursor_key(b'B', modifiers, mode)),
        KeyCode::ArrowRight => Ok(cursor_key(b'C', modifiers, mode)),
        KeyCode::ArrowLeft => Ok(cursor_key(b'D', modifiers, mode)),
        KeyCode::F(number @ 1..=24) => Ok(function_key(number, modifiers)),
        KeyCode::F(_) => Err(EngineError::UnsupportedInput("function key outside F1-F24")),
    }
}

fn encode_character(
    character: char,
    modifiers: KeyModifiers,
    csi_u: bool,
    kitty: bool,
) -> Result<Vec<u8>, EngineError> {
    if (kitty && (modifiers.control || modifiers.alt)) || (csi_u && modifiers.control) {
        let codepoint = if kitty {
            character.to_ascii_lowercase()
        } else {
            character
        };
        // Fixterms encodes Shift in the resulting character, except for Space.
        let parameter = if !kitty && modifiers.shift && character != ' ' {
            modifier_parameter(KeyModifiers {
                shift: false,
                ..modifiers
            })
        } else {
            modifier_parameter(modifiers)
        };
        return Ok(format!("\x1b[{};{}u", codepoint as u32, parameter).into());
    }

    let mut bytes = Vec::new();
    if modifiers.alt {
        bytes.push(0x1b);
    }
    if modifiers.control {
        let control = control_character(character).ok_or(EngineError::UnsupportedInput(
            "unsupported control character",
        ))?;
        bytes.push(control);
    } else {
        let mut encoded = [0; 4];
        bytes.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
    }
    Ok(bytes)
}

pub(crate) fn encode_event(
    event: TerminalKeyEvent,
    mode: TermMode,
    csi_u: bool,
    kitty_allowed: bool,
) -> Result<Vec<u8>, EngineError> {
    let disambiguate = kitty_allowed && mode.contains(TermMode::DISAMBIGUATE_ESC_CODES);
    let report_events = kitty_allowed && mode.contains(TermMode::REPORT_EVENT_TYPES);
    if event.modifiers.super_key && !(disambiguate || report_events) {
        return Err(EngineError::UnsupportedInput("super-modified key"));
    }
    if matches!(event.code, KeyCode::F(0 | 25..=u8::MAX)) {
        return Err(EngineError::UnsupportedInput("function key outside F1-F24"));
    }

    let recovery_key = matches!(
        event.code,
        KeyCode::Enter | KeyCode::Tab | KeyCode::Backspace
    );
    if event.phase == KeyEventPhase::Release
        && (!report_events || (recovery_key && !mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC)))
    {
        return Ok(Vec::new());
    }

    // 先选择协议，后选择键表示。仅 flag 2 的字符 Press 仍须走原适配码。
    let event_sequence = report_events && event.phase != KeyEventPhase::Press;
    let kitty = match event.code {
        KeyCode::Character(_) => {
            (event.modifiers.control || event.modifiers.alt || event.modifiers.super_key)
                && (disambiguate || event_sequence || event.modifiers.super_key)
        }
        KeyCode::Escape => disambiguate || event_sequence || event.modifiers.super_key,
        KeyCode::Enter | KeyCode::Tab | KeyCode::Backspace => {
            event_sequence || event.modifiers.super_key
        }
        _ => disambiguate || report_events,
    };
    if !kitty {
        return if event.phase == KeyEventPhase::Release {
            Ok(Vec::new())
        } else {
            encode(event.legacy_code, event.modifiers, mode, csi_u)
        };
    }

    let phase = if report_events {
        event.phase
    } else {
        KeyEventPhase::Press
    };
    let (number, final_byte) = match event.code {
        KeyCode::Character(character) => (character as u32, 'u'),
        KeyCode::Escape => (27, 'u'),
        KeyCode::Enter => (13, 'u'),
        KeyCode::Tab => (9, 'u'),
        KeyCode::Backspace => (127, 'u'),
        KeyCode::Insert => (2, '~'),
        KeyCode::Delete => (3, '~'),
        KeyCode::PageUp => (5, '~'),
        KeyCode::PageDown => (6, '~'),
        KeyCode::Home => (1, 'H'),
        KeyCode::End => (1, 'F'),
        KeyCode::ArrowUp => (1, 'A'),
        KeyCode::ArrowDown => (1, 'B'),
        KeyCode::ArrowRight => (1, 'C'),
        KeyCode::ArrowLeft => (1, 'D'),
        KeyCode::F(1) => (1, 'P'),
        KeyCode::F(2) => (1, 'Q'),
        // Kitty 的 F3 不使用与光标位置报告冲突的 R；F13 起使用专用码点。
        KeyCode::F(3) => (13, '~'),
        KeyCode::F(4) => (1, 'S'),
        KeyCode::F(number @ 5..=12) => (u32::from(FUNCTION_CODES[usize::from(number - 5)]), '~'),
        KeyCode::F(number @ 13..=24) => (57376 + u32::from(number - 13), 'u'),
        KeyCode::F(_) => return Err(EngineError::UnsupportedInput("function key outside F1-F24")),
    };
    Ok(kitty_sequence(number, final_byte, event.modifiers, phase))
}

fn kitty_sequence(
    number: u32,
    final_byte: char,
    modifiers: KeyModifiers,
    phase: KeyEventPhase,
) -> Vec<u8> {
    let parameter = modifier_parameter(modifiers) + 8 * u8::from(modifiers.super_key);
    if parameter == 1 && phase == KeyEventPhase::Press {
        if number == 1 && final_byte != 'u' && final_byte != '~' {
            return format!("\x1b[{final_byte}").into();
        }
        return format!("\x1b[{number}{final_byte}").into();
    }
    let event_type = match phase {
        KeyEventPhase::Press => "",
        KeyEventPhase::Repeat => ":2",
        KeyEventPhase::Release => ":3",
    };
    format!("\x1b[{number};{parameter}{event_type}{final_byte}").into()
}

fn encode_tab(modifiers: KeyModifiers, protocol: bool) -> Vec<u8> {
    if modifiers.control && protocol {
        return format!("\x1b[9;{}u", modifier_parameter(modifiers)).into();
    }
    match (modifiers.shift, modifiers.control, modifiers.alt) {
        (false, false, false) => b"\t".to_vec(),
        (true, false, false) => b"\x1b[Z".to_vec(),
        (false, true, false) => b"\x1b[9;5u".to_vec(),
        (true, true, false) => b"\x1b[1;5Z".to_vec(),
        _ => prefixed(b"\t", modifiers.alt),
    }
}

fn cursor_key(final_byte: u8, modifiers: KeyModifiers, mode: TermMode) -> Vec<u8> {
    if no_modifiers(modifiers) {
        let prefix = if mode.contains(TermMode::APP_CURSOR) {
            b"\x1bO".as_slice()
        } else {
            b"\x1b[".as_slice()
        };
        return [prefix, &[final_byte]].concat();
    }
    format!(
        "\x1b[1;{}{}",
        modifier_parameter(modifiers),
        final_byte as char
    )
    .into()
}

fn tilde(number: u8, modifiers: KeyModifiers) -> Vec<u8> {
    if no_modifiers(modifiers) {
        format!("\x1b[{number}~").into()
    } else {
        format!("\x1b[{number};{}~", modifier_parameter(modifiers)).into()
    }
}

fn function_key(number: u8, modifiers: KeyModifiers) -> Vec<u8> {
    if number <= 4 {
        let final_byte = b'P' + number - 1;
        if no_modifiers(modifiers) {
            return vec![0x1b, b'O', final_byte];
        }
        return format!(
            "\x1b[1;{}{}",
            modifier_parameter(modifiers),
            final_byte as char
        )
        .into();
    }
    tilde(FUNCTION_CODES[usize::from(number - 5)], modifiers)
}

fn modifier_parameter(modifiers: KeyModifiers) -> u8 {
    1 + u8::from(modifiers.shift) + 2 * u8::from(modifiers.alt) + 4 * u8::from(modifiers.control)
}

fn no_modifiers(modifiers: KeyModifiers) -> bool {
    !modifiers.shift && !modifiers.control && !modifiers.alt
}

fn prefixed(bytes: &[u8], alt: bool) -> Vec<u8> {
    [if alt { b"\x1b".as_slice() } else { &[] }, bytes].concat()
}

fn control_character(character: char) -> Option<u8> {
    match character {
        ' ' | '@' => Some(0),
        'a'..='z' | 'A'..='Z' => Some((character.to_ascii_uppercase() as u8) & 0x1f),
        '[' => Some(0x1b),
        '\\' => Some(0x1c),
        ']' => Some(0x1d),
        '^' => Some(0x1e),
        '_' => Some(0x1f),
        '?' => Some(0x7f),
        _ => None,
    }
}

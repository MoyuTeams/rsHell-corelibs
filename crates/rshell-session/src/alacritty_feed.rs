use alacritty_terminal::{
    Term,
    grid::Dimensions,
    term::TermMode,
    vte::ansi::{Processor, Timeout},
};
use rshell_core::ResolvedTerminalProfile;

use crate::{
    alacritty_event::EventSink,
    alacritty_primary_rows::PrimaryRows,
    alacritty_rows,
    alacritty_tracker::{ScrollTracker, Window},
};

#[path = "alacritty_feed_sync.rs"]
mod sync;

struct FeedWindow<'a> {
    bytes: &'a [u8],
    maximum_shift: usize,
    track_capacity: bool,
}

pub(crate) fn advance(
    processor: &mut Processor,
    terminal: &mut Term<EventSink>,
    events: &EventSink,
    settings: &ResolvedTerminalProfile,
    primary_rows: &mut PrimaryRows,
    tracker: &mut ScrollTracker,
    bytes: &[u8],
) -> Vec<u8> {
    // A synchronized update whose deadline already passed ends before new output is parsed,
    // exactly as if the caller's timer had fired first.
    if sync_expired(processor) {
        sync::stop(processor, terminal, settings, primary_rows);
        sync_cursor(processor, terminal, tracker);
    }
    let mut remaining = bytes;
    while let Some(sequence) = next_alt_switch(remaining) {
        advance_segment(
            processor,
            terminal,
            events,
            settings,
            primary_rows,
            tracker,
            &remaining[..sequence.start],
        );
        advance_segment(
            processor,
            terminal,
            events,
            settings,
            primary_rows,
            tracker,
            &remaining[sequence.clone()],
        );
        remaining = &remaining[sequence.end..];
    }
    advance_segment(
        processor,
        terminal,
        events,
        settings,
        primary_rows,
        tracker,
        remaining,
    );
    events.take_outbound()
}

fn advance_segment(
    processor: &mut Processor,
    terminal: &mut Term<EventSink>,
    events: &EventSink,
    settings: &ResolvedTerminalProfile,
    primary_rows: &mut PrimaryRows,
    tracker: &mut ScrollTracker,
    bytes: &[u8],
) {
    if bytes.is_empty() {
        return;
    }
    let mut remaining = bytes;
    while let Some(index) = remaining.iter().position(|byte| *byte == 0x05) {
        advance_windows(
            processor,
            terminal,
            settings,
            primary_rows,
            tracker,
            &remaining[..=index],
        );
        events.push_outbound(settings.answerback.as_bytes());
        remaining = &remaining[index + 1..];
    }
    advance_windows(
        processor,
        terminal,
        settings,
        primary_rows,
        tracker,
        remaining,
    );
}

fn advance_windows(
    processor: &mut Processor,
    terminal: &mut Term<EventSink>,
    settings: &ResolvedTerminalProfile,
    primary_rows: &mut PrimaryRows,
    tracker: &mut ScrollTracker,
    mut remaining: &[u8],
) {
    while !remaining.is_empty() {
        let was_primary = !terminal.mode().contains(TermMode::ALT_SCREEN);
        if processor.sync_timeout().pending_timeout() {
            tracker.consume(remaining, was_primary);
            sync::advance(processor, terminal, settings, primary_rows, remaining);
            sync_cursor(processor, terminal, tracker);
            return;
        }
        let (length, maximum_shift, track_capacity) = if was_primary {
            let grid = terminal.grid();
            let room = settings
                .scrollback_lines
                .saturating_sub(grid.history_size());
            let maximum = if room == 0 {
                alacritty_rows::window_capacity(terminal)
            } else {
                room
            };
            match tracker.next_window(remaining, true, maximum) {
                Window::Bounded {
                    length,
                    maximum_shift,
                } => (length, maximum_shift, true),
                // An indivisible command can exceed the remaining history room,
                // but its scrolling is still bounded by the screen height (or a
                // replacement character plus a control byte on a one-row grid).
                Window::Unsafe { length } => (length, grid.screen_lines().max(2), false),
            }
        } else {
            tracker.consume(remaining, false);
            (remaining.len(), 0, false)
        };
        advance_window(
            processor,
            terminal,
            settings,
            primary_rows,
            tracker,
            FeedWindow {
                bytes: &remaining[..length],
                maximum_shift,
                track_capacity,
            },
        );
        remaining = &remaining[length..];
    }
}

/// Deadline of the synchronized update (DEC mode 2026) the processor is buffering, if any.
pub(crate) fn sync_deadline(processor: &Processor) -> Option<std::time::Instant> {
    processor.sync_timeout().sync_timeout()
}

fn sync_expired(processor: &Processor) -> bool {
    sync_deadline(processor).is_some_and(|deadline| deadline <= std::time::Instant::now())
}

/// Ends a pending synchronized update: the buffered output is applied to the terminal with the
/// same scrollback bookkeeping as regular output. Terminals give up waiting for the closing
/// sequence after a short timeout, so a program that dies in the middle of a frame does not
/// freeze the display.
pub(crate) fn end_sync(
    processor: &mut Processor,
    terminal: &mut Term<EventSink>,
    events: &EventSink,
    settings: &ResolvedTerminalProfile,
    primary_rows: &mut PrimaryRows,
    tracker: &mut ScrollTracker,
) -> Vec<u8> {
    if processor.sync_timeout().pending_timeout() {
        sync::stop(processor, terminal, settings, primary_rows);
        sync_cursor(processor, terminal, tracker);
    }
    events.take_outbound()
}

fn advance_window(
    processor: &mut Processor,
    terminal: &mut Term<EventSink>,
    settings: &ResolvedTerminalProfile,
    primary_rows: &mut PrimaryRows,
    tracker: &mut ScrollTracker,
    window: FeedWindow<'_>,
) {
    let FeedWindow {
        bytes,
        maximum_shift,
        track_capacity,
    } = window;
    alacritty_rows::apply(
        terminal,
        settings.scrollback_lines,
        primary_rows,
        maximum_shift,
        track_capacity,
        |terminal| processor.advance(terminal, bytes),
    );
    sync_cursor(processor, terminal, tracker);
}

fn sync_cursor(processor: &Processor, terminal: &Term<EventSink>, tracker: &mut ScrollTracker) {
    // The tracker has consumed the pending bytes, but the terminal has not.
    if processor.sync_timeout().pending_timeout() {
        return;
    }
    let active_primary = !terminal.mode().contains(TermMode::ALT_SCREEN);
    let cursor = &terminal.grid().cursor;
    tracker.sync_cursor(
        active_primary,
        cursor.point.line.0 as usize,
        cursor.point.column.0,
        cursor.input_needs_wrap,
    );
}

fn next_alt_switch(bytes: &[u8]) -> Option<std::ops::Range<usize>> {
    for start in 0..bytes.len().saturating_sub(3) {
        if !bytes[start..].starts_with(b"\x1b[?") {
            continue;
        }
        let tail = &bytes[start + 3..];
        let final_offset = tail.iter().position(|byte| (0x40..=0x7e).contains(byte))?;
        let final_byte = tail[final_offset];
        if !matches!(final_byte, b'h' | b'l') {
            continue;
        }
        let parameters = &tail[..final_offset];
        if parameters
            .split(|byte| *byte == b';')
            .any(|value| value == b"1049")
        {
            return Some(start..start + 3 + final_offset + 1);
        }
    }
    None
}

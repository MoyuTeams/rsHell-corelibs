use alacritty_terminal::{Term, grid::Dimensions, vte::ansi::*};
use rshell_core::ResolvedTerminalProfile;

use crate::{alacritty_event::EventSink, alacritty_primary_rows::PrimaryRows, alacritty_rows};

pub(super) fn stop(
    processor: &mut Processor,
    terminal: &mut Term<EventSink>,
    settings: &ResolvedTerminalProfile,
    primary_rows: &mut PrimaryRows,
) {
    processor.stop_sync(&mut SyncHandler {
        terminal,
        primary_rows,
        history_limit: settings.scrollback_lines,
    });
}

pub(super) fn advance(
    processor: &mut Processor,
    terminal: &mut Term<EventSink>,
    settings: &ResolvedTerminalProfile,
    primary_rows: &mut PrimaryRows,
    bytes: &[u8],
) {
    processor.advance(
        &mut SyncHandler {
            terminal,
            primary_rows,
            history_limit: settings.scrollback_lines,
        },
        bytes,
    );
}

// VTE applies its entire buffer in stop_sync, ignoring our raw-byte windows. Observe
// actual handler operations instead: a row anchor cannot survive arbitrary ring rotations.
struct SyncHandler<'a> {
    terminal: &'a mut Term<EventSink>,
    primary_rows: &'a mut PrimaryRows,
    history_limit: usize,
}

impl SyncHandler<'_> {
    fn apply(&mut self, maximum_shift: usize, operation: impl FnOnce(&mut Term<EventSink>)) {
        let room = self
            .history_limit
            .saturating_sub(self.terminal.grid().history_size());
        alacritty_rows::apply(
            self.terminal,
            self.history_limit,
            self.primary_rows,
            maximum_shift,
            room == 0 || maximum_shift <= room,
            operation,
        );
    }
}

macro_rules! forward {
    ($($name:ident($($arg:ident: $ty:ty),*);)*) => {$ (
        fn $name(&mut self, $($arg: $ty),*) {
            self.terminal.$name($($arg),*);
        }
    )*};
}

macro_rules! track {
    ($($name:ident($($arg:ident: $ty:ty),*) => $maximum:expr;)*) => {$ (
        fn $name(&mut self, $($arg: $ty),*) {
            self.apply($maximum, |terminal| terminal.$name($($arg),*));
        }
    )*};
}

impl Handler for SyncHandler<'_> {
    track! {
        input(c: char) => 1;
        put_tab(count: u16) => 1;
        linefeed() => 1;
        newline() => 1;
        reset_state() => 0;
        set_private_mode(mode: PrivateMode) => 0;
        unset_private_mode(mode: PrivateMode) => 0;
    }

    fn scroll_up(&mut self, count: usize) {
        // Alacritty clamps the original count to the scroll region. Repeating
        // single-row operations would instead scroll past that region's height.
        self.apply(count.min(self.terminal.screen_lines()), |terminal| {
            terminal.scroll_up(count)
        });
    }

    fn delete_lines(&mut self, count: usize) {
        self.apply(count.min(self.terminal.screen_lines()), |terminal| {
            terminal.delete_lines(count)
        });
    }

    fn clear_screen(&mut self, mode: ClearMode) {
        // ED 2 moves the primary viewport into history in Alacritty.
        self.apply(self.terminal.screen_lines(), |terminal| {
            terminal.clear_screen(mode)
        });
    }

    forward! {
        set_title(title: Option<String>);
        set_cursor_style(style: Option<CursorStyle>);
        set_cursor_shape(shape: CursorShape);
        goto(line: i32, col: usize);
        goto_line(line: i32);
        goto_col(col: usize);
        insert_blank(count: usize);
        move_up(count: usize);
        move_down(count: usize);
        identify_terminal(intermediate: Option<char>);
        device_status(status: usize);
        move_forward(count: usize);
        move_backward(count: usize);
        move_down_and_cr(count: usize);
        move_up_and_cr(count: usize);
        backspace();
        carriage_return();
        bell();
        substitute();
        set_horizontal_tabstop();
        scroll_down(count: usize);
        insert_blank_lines(count: usize);
        erase_chars(count: usize);
        delete_chars(count: usize);
        move_backward_tabs(count: u16);
        move_forward_tabs(count: u16);
        save_cursor_position();
        restore_cursor_position();
        clear_line(mode: LineClearMode);
        clear_tabs(mode: TabulationClearMode);
        set_tabs(interval: u16);
        reverse_index();
        terminal_attribute(attr: Attr);
        set_mode(mode: Mode);
        unset_mode(mode: Mode);
        report_mode(mode: Mode);
        report_private_mode(mode: PrivateMode);
        set_scrolling_region(top: usize, bottom: Option<usize>);
        set_keypad_application_mode();
        unset_keypad_application_mode();
        set_active_charset(index: CharsetIndex);
        configure_charset(index: CharsetIndex, charset: StandardCharset);
        set_color(index: usize, color: Rgb);
        dynamic_color_sequence(prefix: String, index: usize, terminator: &str);
        reset_color(index: usize);
        clipboard_store(clipboard: u8, data: &[u8]);
        clipboard_load(clipboard: u8, terminator: &str);
        decaln();
        push_title();
        pop_title();
        text_area_size_pixels();
        text_area_size_chars();
        set_hyperlink(hyperlink: Option<Hyperlink>);
        set_mouse_cursor_icon(icon: cursor_icon::CursorIcon);
        report_keyboard_mode();
        push_keyboard_mode(mode: KeyboardModes);
        pop_keyboard_modes(count: u16);
        set_keyboard_mode(mode: KeyboardModes, behavior: KeyboardModesApplyBehavior);
        set_modify_other_keys(mode: ModifyOtherKeys);
        report_modify_other_keys();
        set_scp(path: ScpCharPath, mode: ScpUpdateMode);
    }
}

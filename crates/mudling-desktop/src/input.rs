//! The command input: one line of text through the platform's input handler,
//! so IME composition (marked text) works as in any native field.
//!
//! The platform talks in UTF-16 offsets (`NSTextInputClient` on macOS, TSF on
//! Windows, text-input on Wayland); the content is stored as UTF-8. Adapted
//! from gpui's `input` example, without mouse selection for now.

use crate::theme::Theme;
use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, KeyBinding,
    LayoutId, Pixels, Role, ShapedLine, SharedString, Style, TextRun, UTF16Selection,
    UnderlineStyle, Window, actions, div, fill, point, prelude::*, px, relative, size,
};
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

actions!(
    command_input,
    [
        Backspace, Delete, Left, Right, Home, End, Paste, Copy, Submit
    ]
);

const CONTEXT: &str = "CommandInput";

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, Some(CONTEXT)),
        KeyBinding::new("delete", Delete, Some(CONTEXT)),
        KeyBinding::new("left", Left, Some(CONTEXT)),
        KeyBinding::new("right", Right, Some(CONTEXT)),
        KeyBinding::new("home", Home, Some(CONTEXT)),
        KeyBinding::new("end", End, Some(CONTEXT)),
        KeyBinding::new(secondary!("v"), Paste, Some(CONTEXT)),
        KeyBinding::new(secondary!("c"), Copy, Some(CONTEXT)),
        KeyBinding::new("enter", Submit, Some(CONTEXT)),
    ]);
}

/// Sent when the player presses Return.
pub struct Submitted(pub String);

pub struct CommandInput {
    focus_handle: FocusHandle,
    content: String,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
}

impl EventEmitter<Submitted> for CommandInput {}

impl Focusable for CommandInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl CommandInput {
    pub fn new(cx: &mut Context<CommandInput>) -> CommandInput {
        CommandInput {
            focus_handle: cx.focus_handle(),
            content: String::new(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_layout: None,
            last_bounds: None,
        }
    }

    fn submit(&mut self, _: &Submit, window: &mut Window, cx: &mut Context<Self>) {
        // Return while composing confirms the composition; it doesn't send.
        if self.marked_range.is_some() {
            return;
        }
        let command = std::mem::take(&mut self.content);
        self.selected_range = 0..0;
        window.notify_text_input_changed(&self.focus_handle);
        cx.emit(Submitted(command));
        cx.notify();
    }

    fn left(&mut self, _: &Left, window: &mut Window, cx: &mut Context<Self>) {
        let target = if self.selected_range.is_empty() {
            self.previous_boundary(self.cursor_offset())
        } else {
            self.selected_range.start
        };
        self.move_to(target, window, cx);
    }

    fn right(&mut self, _: &Right, window: &mut Window, cx: &mut Context<Self>) {
        let target = if self.selected_range.is_empty() {
            self.next_boundary(self.selected_range.end)
        } else {
            self.selected_range.end
        };
        self.move_to(target, window, cx);
    }

    fn home(&mut self, _: &Home, window: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, window, cx);
    }

    fn end(&mut self, _: &End, window: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), window, cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.selected_range =
                self.previous_boundary(self.cursor_offset())..self.selected_range.end;
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.selected_range =
                self.selected_range.start..self.next_boundary(self.cursor_offset());
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace_text_in_range(None, &text.replace('\n', " "), window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            let selected = self.content[self.selected_range.clone()].to_string();
            cx.write_to_clipboard(ClipboardItem::new_string(selected));
        }
    }

    fn move_to(&mut self, offset: usize, window: &Window, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(index, _)| (index < offset).then_some(index))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(index, _)| (index > offset).then_some(index))
            .unwrap_or(self.content.len())
    }

    fn offset_from_utf16(&self, offset_utf16: usize) -> usize {
        let mut utf16 = 0;
        for (index, ch) in self.content.char_indices() {
            if utf16 >= offset_utf16 {
                return index;
            }
            utf16 += ch.len_utf16();
        }
        self.content.len()
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        self.content[..offset].encode_utf16().count()
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }

    /// The range an edit replaces: the one given, else the composition, else
    /// the selection.
    fn edit_range(&self, range_utf16: Option<Range<usize>>) -> Range<usize> {
        range_utf16
            .map(|range| self.range_from_utf16(&range))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone())
    }
}

impl EntityInputHandler for CommandInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn set_selected_text_range(
        &mut self,
        range_utf16: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected_range = self.range_from_utf16(&range_utf16);
        self.selection_reversed = false;
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    fn text_length_utf16(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        Some(self.content.encode_utf16().count())
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.marked_range = None;
        window.notify_text_input_changed(&self.focus_handle);
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = self.edit_range(range_utf16);
        self.content.replace_range(range.clone(), new_text);
        let cursor = range.start + new_text.len();
        self.selected_range = cursor..cursor;
        self.selection_reversed = false;
        self.marked_range = None;
        window.notify_text_input_changed(&self.focus_handle);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = self.edit_range(range_utf16);
        self.content.replace_range(range.clone(), new_text);
        self.marked_range =
            (!new_text.is_empty()).then(|| range.start..range.start + new_text.len());
        // The new selection is relative to the inserted text.
        let start_utf16 = self.offset_to_utf16(range.start);
        let inserted_utf16 = new_text.encode_utf16().count();
        self.selected_range = match new_selected_range_utf16 {
            Some(selected) => self.range_from_utf16(
                &(start_utf16 + selected.start.min(inserted_utf16)
                    ..start_utf16 + selected.end.min(inserted_utf16)),
            ),
            None => range.start + new_text.len()..range.start + new_text.len(),
        };
        self.selection_reversed = false;
        window.notify_text_input_changed(&self.focus_handle);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        // With nothing typed yet there's no layout; the IME's candidate
        // window then sits at the start of the field.
        let x = |offset: usize| {
            self.last_layout
                .as_ref()
                .map_or(px(0.), |line| line.x_for_index(offset))
        };
        let range = self.range_from_utf16(&range_utf16);
        Some(Bounds::from_corners(
            point(bounds.left() + x(range.start), bounds.top()),
            point(bounds.left() + x(range.end), bounds.bottom()),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: gpui::Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let local = self.last_bounds?.localize(&point)?;
        let index = self.last_layout.as_ref()?.closest_index_for_x(local.x);
        Some(self.offset_to_utf16(index.min(self.content.len())))
    }
}

impl Render for CommandInput {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        div()
            .id("command-input")
            .role(Role::TextInput)
            .aria_label("Command")
            .aria_value(SharedString::from(self.content.clone()))
            .aria_placeholder("Type a command")
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .tab_stop(true)
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::copy))
            .on_click(cx.listener(|input, _, window, cx| window.focus(&input.focus_handle, cx)))
            .w_full()
            .px_3()
            .py_2()
            .rounded_md()
            .border_1()
            .border_color(theme.input_border)
            .bg(theme.input_background)
            .child(InputText { input: cx.entity() })
    }
}

/// Draws the input's single line, and registers it as the window's text
/// input target while it has focus.
struct InputText {
    input: Entity<CommandInput>,
}

struct InputTextPrepaint {
    line: ShapedLine,
    cursor: Option<gpui::PaintQuad>,
    selection: Option<gpui::PaintQuad>,
}

impl IntoElement for InputText {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for InputText {
    type RequestLayoutState = ();
    type PrepaintState = InputTextPrepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> InputTextPrepaint {
        let theme = cx.global::<Theme>();
        let input = self.input.read(cx);
        let style = window.text_style();
        let (text, color) = if input.content.is_empty() {
            (SharedString::from("Type a command"), theme.placeholder)
        } else {
            (SharedString::from(input.content.clone()), style.color)
        };
        let run = TextRun {
            len: text.len(),
            font: style.font(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        // The composition is underlined, as native fields show it.
        let runs = match &input.marked_range {
            Some(marked) => [
                TextRun {
                    len: marked.start,
                    ..run.clone()
                },
                TextRun {
                    len: marked.end - marked.start,
                    underline: Some(UnderlineStyle {
                        color: Some(color),
                        thickness: px(1.),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: text.len() - marked.end,
                    ..run
                },
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect(),
            None => vec![run],
        };
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window
            .text_system()
            .shape_line(text, font_size, &runs, None);

        let x = |offset: usize| bounds.left() + line.x_for_index(offset);
        let (cursor, selection) = if input.selected_range.is_empty() {
            let at = x(input.cursor_offset());
            let caret = Bounds::new(point(at, bounds.top()), size(px(2.), bounds.size.height));
            (Some(fill(caret, theme.accent)), None)
        } else {
            let start = x(input.selected_range.start);
            let end = x(input.selected_range.end);
            let highlight =
                Bounds::from_corners(point(start, bounds.top()), point(end, bounds.bottom()));
            (None, Some(fill(highlight, theme.accent.opacity(0.3))))
        };
        InputTextPrepaint {
            line,
            cursor,
            selection,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        prepaint: &mut InputTextPrepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        if let Some(selection) = prepaint.selection.take() {
            window.paint_quad(selection);
        }
        let line = prepaint.line.clone();
        line.paint(
            bounds.origin,
            window.line_height(),
            gpui::TextAlign::Left,
            None,
            window,
            cx,
        )
        .ok();
        if focus_handle.is_focused(window)
            && let Some(cursor) = prepaint.cursor.take()
        {
            window.paint_quad(cursor);
        }
        let has_content = !self.input.read(cx).content.is_empty();
        self.input.update(cx, |input, _| {
            // The placeholder's layout doesn't describe the content.
            input.last_layout = has_content.then_some(line);
            input.last_bounds = Some(bounds);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext, WindowAppearance};

    fn input_window(cx: &mut TestAppContext) -> (Entity<CommandInput>, &mut VisualTestContext) {
        cx.update(|cx| cx.set_global(Theme::for_appearance(WindowAppearance::Dark)));
        cx.add_window_view(|_, cx| CommandInput::new(cx))
    }

    // These drive the same EntityInputHandler calls the platform makes for an
    // IME: on macOS, setMarkedText and insertText from NSTextInputClient.
    #[gpui::test]
    fn japanese_composition_marks_then_commits(cx: &mut TestAppContext) {
        let (input, cx) = input_window(cx);
        input.update_in(cx, |input, window, cx| {
            input.replace_text_in_range(None, "go ", window, cx);
            input.replace_and_mark_text_in_range(None, "k", Some(1..1), window, cx);
            input.replace_and_mark_text_in_range(None, "き", Some(1..1), window, cx);
            assert_eq!(input.content, "go き");
            assert_eq!(input.marked_text_range(window, cx), Some(3..4));

            input.replace_text_in_range(None, "北", window, cx);
            assert_eq!(input.content, "go 北");
            assert_eq!(input.marked_text_range(window, cx), None);
            assert_eq!(
                input.selected_text_range(false, window, cx).unwrap().range,
                4..4
            );
        });
    }

    #[gpui::test]
    fn return_while_composing_confirms_instead_of_sending(cx: &mut TestAppContext) {
        let (input, cx) = input_window(cx);
        input.update_in(cx, |input, window, cx| {
            input.replace_and_mark_text_in_range(None, "にほん", None, window, cx);
            input.submit(&Submit, window, cx);
            assert_eq!(input.content, "にほん");
        });
    }

    #[gpui::test]
    fn platform_offsets_are_utf16(cx: &mut TestAppContext) {
        let (input, cx) = input_window(cx);
        input.update_in(cx, |input, window, cx| {
            input.replace_text_in_range(None, "a🙂b", window, cx);
            assert_eq!(input.text_length_utf16(window, cx), Some(4));
            input.set_selected_text_range(1..3, window, cx);
            assert_eq!(input.selected_range, 1..5);
            let mut actual = None;
            assert_eq!(
                input
                    .text_for_range(3..4, &mut actual, window, cx)
                    .as_deref(),
                Some("b")
            );
        });
    }
}

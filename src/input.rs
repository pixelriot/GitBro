//! A small text field: one or several lines with soft wrapping, selection, clipboard and IME input.
//! gpui ships no text input, so this follows its `input` example, extended to wrapped lines.

use gpui::{
    AvailableSpace, App, Bounds, ClipboardItem, ContentMask, Context, CursorStyle, Element, ElementId,
    ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable,
    GlobalElementId, InspectorElementId, IntoElement, KeyBinding, LayoutId, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, Render, ScrollWheelEvent,
    SharedString,
    Style, TextAlign, TextRun, UTF16Selection, UnderlineStyle, Window, WrappedLine, actions, black,
    div, fill, point, prelude::*, px, relative, rgb, rgba, size,
};
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

pub const CONTEXT: &str = "TextInput";

actions!(
    text_input,
    [
        Backspace,
        Delete,
        DeleteWord,
        Left,
        Right,
        Up,
        Down,
        Home,
        End,
        WordLeft,
        WordRight,
        SelectLeft,
        SelectRight,
        SelectAll,
        Paste,
        Cut,
        Copy,
        Enter,
        Submit,
        Cancel,
    ]
);

/// Enter submits a single-line field and breaks the line in a multi-line one (Ctrl+Enter submits).
pub enum InputEvent {
    Submit,
    Cancel,
}

#[derive(Clone, Copy)]
pub struct InputColors {
    pub text: u32,
    pub hint: u32,
    pub caret: u32,
    /// 0xRRGGBBAA
    pub selection: u32,
}

impl Default for InputColors {
    fn default() -> Self {
        Self { text: 0xffffff, hint: 0x888888, caret: 0xffffff, selection: 0x4488ff55 }
    }
}

/// Key bindings of the field; they live in their own context so they win over the app's.
pub fn bind_keys(cx: &mut App) {
    let c = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, c),
        KeyBinding::new("delete", Delete, c),
        KeyBinding::new("ctrl-backspace", DeleteWord, c),
        KeyBinding::new("left", Left, c),
        KeyBinding::new("right", Right, c),
        KeyBinding::new("up", Up, c),
        KeyBinding::new("down", Down, c),
        KeyBinding::new("home", Home, c),
        KeyBinding::new("end", End, c),
        KeyBinding::new("ctrl-left", WordLeft, c),
        KeyBinding::new("ctrl-right", WordRight, c),
        KeyBinding::new("shift-left", SelectLeft, c),
        KeyBinding::new("shift-right", SelectRight, c),
        KeyBinding::new("ctrl-a", SelectAll, c),
        KeyBinding::new("ctrl-v", Paste, c),
        KeyBinding::new("ctrl-c", Copy, c),
        KeyBinding::new("ctrl-x", Cut, c),
        KeyBinding::new("enter", Enter, c),
        KeyBinding::new("ctrl-enter", Submit, c),
        KeyBinding::new("escape", Cancel, c),
    ]);
}

pub struct TextInput {
    focus: FocusHandle,
    content: String,
    placeholder: SharedString,
    /// The field is this many lines tall when empty and grows up to `max_lines`, then scrolls.
    min_lines: usize,
    max_lines: usize,
    selected: Range<usize>,
    reversed: bool,
    marked: Option<Range<usize>>,
    colors: InputColors,
    shown: Option<Shown>,
    scroll: Point<Pixels>,
    /// Caret position and text length the view last scrolled to. Only a change of either brings
    /// the caret back into view, so the mouse wheel can scroll it away.
    followed: (usize, usize),
    selecting: bool,
}

impl EventEmitter<InputEvent> for TextInput {}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl TextInput {
    pub fn new(cx: &mut Context<Self>, placeholder: &str, lines: (usize, usize)) -> Self {
        let min_lines = lines.0.max(1);
        Self {
            focus: cx.focus_handle(),
            content: String::new(),
            placeholder: placeholder.to_owned().into(),
            min_lines,
            max_lines: lines.1.max(min_lines),
            selected: 0..0,
            reversed: false,
            marked: None,
            colors: InputColors::default(),
            shown: None,
            scroll: Point::default(),
            followed: (0, 0),
            selecting: false,
        }
    }

    pub fn text(&self) -> &str {
        &self.content
    }

    /// Colours follow the app theme; the parent sets them on every render (no repaint needed).
    pub fn set_colors(&mut self, colors: InputColors) {
        self.colors = colors;
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.set_text("", cx);
    }

    /// Replaces everything; the caret ends up after the new text.
    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.content = text.to_owned();
        self.selected = text.len()..text.len();
        self.reversed = false;
        self.marked = None;
        self.shown = None;
        self.scroll = Point::default();
        cx.notify();
    }

    fn multiline(&self) -> bool {
        self.max_lines > 1
    }

    fn cursor(&self) -> usize {
        if self.reversed { self.selected.start } else { self.selected.end }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected = offset..offset;
        self.reversed = false;
        cx.notify();
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.reversed {
            self.selected.start = offset;
        } else {
            self.selected.end = offset;
        }
        if self.selected.end < self.selected.start {
            self.reversed = !self.reversed;
            self.selected = self.selected.end..self.selected.start;
        }
        cx.notify();
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(i, _)| (i < offset).then_some(i))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(i, _)| (i > offset).then_some(i))
            .unwrap_or(self.content.len())
    }

    /// Start and end of the line (up to the next line break) around `offset`.
    fn line_bounds(&self, offset: usize) -> (usize, usize) {
        let start = self.content[..offset].rfind('\n').map_or(0, |i| i + 1);
        let end = self.content[offset..].find('\n').map_or(self.content.len(), |i| offset + i);
        (start, end)
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            self.move_to(self.previous_boundary(self.cursor()), cx);
        } else {
            self.move_to(self.selected.start, cx);
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            self.move_to(self.next_boundary(self.selected.end), cx);
        } else {
            self.move_to(self.selected.end, cx);
        }
    }

    /// Moves one visual row up or down, keeping the horizontal position.
    fn vertical(&mut self, rows: f32, cx: &mut Context<Self>) {
        let Some(shown) = &self.shown else { return };
        let caret = shown.point_for(self.cursor());
        let target = point(caret.x, caret.y + shown.line_height * (rows + 0.5));
        let ix = shown.index_at(target, self.content.len());
        let ix = floor_boundary(&self.content, ix);
        self.move_to(ix, cx);
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        self.vertical(-1., cx);
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        self.vertical(1., cx);
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.line_bounds(self.cursor()).0, cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.line_bounds(self.cursor()).1, cx);
    }

    fn word_left(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(prev_word(&self.content, self.cursor()), cx);
    }

    fn word_right(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(next_word(&self.content, self.cursor()), cx);
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.selected = 0..self.content.len();
        self.reversed = false;
        cx.notify();
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            self.select_to(self.previous_boundary(self.cursor()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            self.select_to(self.next_boundary(self.cursor()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete_word(&mut self, _: &DeleteWord, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            self.select_to(prev_word(&self.content, self.cursor()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let text = text.replace("\r\n", "\n").replace('\r', "\n");
            self.replace_text_in_range(None, &text, window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(self.content[self.selected.clone()].to_owned()));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(self.content[self.selected.clone()].to_owned()));
            self.replace_text_in_range(None, "", window, cx);
        }
    }

    fn enter(&mut self, _: &Enter, window: &mut Window, cx: &mut Context<Self>) {
        if self.multiline() {
            self.replace_text_in_range(None, "\n", window, cx);
        } else {
            cx.emit(InputEvent::Submit);
        }
    }

    fn submit(&mut self, _: &Submit, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(InputEvent::Submit);
    }

    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(InputEvent::Cancel);
    }

    fn index_at(&self, position: Point<Pixels>) -> usize {
        let Some(shown) = &self.shown else { return self.content.len() };
        let ix = shown.index_at(position - shown.bounds.origin + self.scroll, self.content.len());
        floor_boundary(&self.content, ix)
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus);
        // The window's own click handler would move the focus back to the main view.
        cx.stop_propagation();
        self.selecting = true;
        let ix = self.index_at(event.position);
        if event.modifiers.shift {
            self.select_to(ix, cx);
        } else {
            self.move_to(ix, cx);
        }
    }

    /// The wheel scrolls text taller than the field; prepaint keeps the offset within the text.
    fn scroll_wheel(&mut self, event: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.multiline() {
            return;
        }
        self.scroll.y -= event.delta.pixel_delta(window.line_height()).y;
        cx.notify();
        cx.stop_propagation();
    }

    fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.selecting = false;
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.selecting {
            self.select_to(self.index_at(event.position), cx);
        }
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let (mut utf8, mut utf16) = (0, 0);
        for ch in self.content.chars() {
            if utf16 >= offset {
                break;
            }
            utf16 += ch.len_utf16();
            utf8 += ch.len_utf8();
        }
        utf8
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let (mut utf8, mut utf16) = (0, 0);
        for ch in self.content.chars() {
            if utf8 >= offset {
                break;
            }
            utf8 += ch.len_utf8();
            utf16 += ch.len_utf16();
        }
        utf16
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }
}

/// The largest char boundary not after `ix` (a layout from before an edit may be slightly stale).
fn floor_boundary(text: &str, ix: usize) -> usize {
    let mut ix = ix.min(text.len());
    while !text.is_char_boundary(ix) {
        ix -= 1;
    }
    ix
}

/// 0 = whitespace, 1 = word character, 2 = anything else.
fn class(c: char) -> u8 {
    if c.is_whitespace() {
        0
    } else if c.is_alphanumeric() || c == '_' {
        1
    } else {
        2
    }
}

/// Start of the word before `offset` (skipping whitespace first), like Ctrl+Left.
fn prev_word(text: &str, offset: usize) -> usize {
    let mut at = offset;
    let mut kind = None;
    for (i, c) in text[..offset].char_indices().rev() {
        let k = class(c);
        match kind {
            None if k == 0 => {}
            None => kind = Some(k),
            Some(prev) if prev != k => break,
            Some(_) => {}
        }
        at = i;
    }
    at
}

/// End of the word after `offset` (skipping whitespace first), like Ctrl+Right.
fn next_word(text: &str, offset: usize) -> usize {
    let mut at = offset;
    let mut kind = None;
    for (i, c) in text[offset..].char_indices() {
        let k = class(c);
        match kind {
            None if k == 0 => {}
            None => kind = Some(k),
            Some(prev) if prev != k => break,
            Some(_) => {}
        }
        at = offset + i + c.len_utf8();
    }
    at
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_owned())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection { range: self.range_to_utf16(&self.selected), reversed: self.reversed })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked.as_ref().map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or(self.marked.clone())
            .unwrap_or(self.selected.clone());
        let new_text = if self.multiline() { new_text.to_owned() } else { new_text.replace(['\n', '\r'], " ") };
        self.content.replace_range(range.clone(), &new_text);
        let at = range.start + new_text.len();
        self.selected = at..at;
        self.reversed = false;
        self.marked = None;
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or(self.marked.clone())
            .unwrap_or(self.selected.clone());
        let new_text = if self.multiline() { new_text.to_owned() } else { new_text.replace(['\n', '\r'], " ") };
        self.content.replace_range(range.clone(), &new_text);
        self.marked = (!new_text.is_empty()).then(|| range.start..range.start + new_text.len());
        self.selected = new_selected_range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .map(|r| r.start + range.start..r.end + range.start)
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
        self.reversed = false;
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let shown = self.shown.as_ref()?;
        let range = self.range_from_utf16(&range_utf16);
        let origin = shown.bounds.origin - self.scroll;
        let (a, b) = (shown.point_for(range.start), shown.point_for(range.end));
        Some(Bounds::from_corners(origin + a, origin + point(b.x, b.y + shown.line_height)))
    }

    fn character_index_for_point(
        &mut self,
        position: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        self.shown.as_ref()?;
        Some(self.offset_to_utf16(self.index_at(position)))
    }
}

/// Text laid out by the last paint: lines (split at `\n`, each possibly wrapped) and their geometry.
struct Shown {
    lines: Vec<WrappedLine>,
    /// Byte offset in the text where each line starts.
    starts: Vec<usize>,
    /// Vertical offset of each line from the top of the content.
    tops: Vec<Pixels>,
    height: Pixels,
    line_height: Pixels,
    bounds: Bounds<Pixels>,
}

impl Shown {
    fn new(lines: Vec<WrappedLine>, line_height: Pixels, bounds: Bounds<Pixels>) -> Self {
        let mut starts = Vec::with_capacity(lines.len());
        let mut tops = Vec::with_capacity(lines.len());
        let (mut start, mut top) = (0, px(0.));
        for line in &lines {
            starts.push(start);
            tops.push(top);
            start += line.len() + 1; // the line break
            top += line.size(line_height).height;
        }
        Self { lines, starts, tops, height: top, line_height, bounds }
    }

    fn line_of(&self, index: usize) -> usize {
        self.starts.partition_point(|&s| s <= index).saturating_sub(1)
    }

    /// Top-left of the caret at `index`, relative to the content's origin.
    fn point_for(&self, index: usize) -> Point<Pixels> {
        let i = self.line_of(index);
        let Some(line) = self.lines.get(i) else { return Point::default() };
        let local = line
            .position_for_index(index.saturating_sub(self.starts[i]), self.line_height)
            .unwrap_or_default();
        point(local.x, self.tops[i] + local.y)
    }

    /// Text index nearest to `p` (relative to the content's origin).
    fn index_at(&self, p: Point<Pixels>, len: usize) -> usize {
        if p.y < px(0.) || self.lines.is_empty() {
            return 0;
        }
        if p.y >= self.height {
            return len;
        }
        let i = self.tops.partition_point(|&t| t <= p.y).saturating_sub(1);
        let local = point(p.x.max(px(0.)), p.y - self.tops[i]);
        let ix = match self.lines[i].closest_index_for_position(local, self.line_height) {
            Ok(ix) | Err(ix) => ix,
        };
        (self.starts[i] + ix).min(len)
    }

    /// One rectangle per visual row touched by `range`, relative to the content's origin.
    fn selection_rects(&self, range: &Range<usize>) -> Vec<Bounds<Pixels>> {
        let lh = self.line_height;
        let mut rects = Vec::new();
        for (i, line) in self.lines.iter().enumerate() {
            let start = self.starts[i];
            let end = start + line.len();
            if range.start > end || range.end < start {
                continue;
            }
            let (a, b) = (range.start.max(start) - start, range.end.min(end) - start);
            // The line break after this line is selected too: show it as a short stub.
            let break_selected = range.end > end && range.start <= end && i + 1 < self.lines.len();

            let mut row_starts = vec![0];
            row_starts.extend(line.wrap_boundaries().iter().map(|w| line.runs()[w.run_ix].glyphs[w.glyph_ix].index));
            for (r, &row_start) in row_starts.iter().enumerate() {
                let last = r + 1 == row_starts.len();
                let row_end = row_starts.get(r + 1).copied().unwrap_or(line.len());
                let (from, to) = (a.max(row_start), b.min(row_end));
                let stub = if break_selected && last { px(6.) } else { px(0.) };
                if from > to || (from == to && stub == px(0.)) {
                    continue;
                }
                let x = |ix: usize| {
                    if ix == row_start {
                        px(0.)
                    } else {
                        line.position_for_index(ix, lh).map_or(px(0.), |p| p.x)
                    }
                };
                let (x0, x1) = (x(from), x(to) + stub);
                rects.push(Bounds::new(point(x0, self.tops[i] + lh * r as f32), size(x1 - x0, lh)));
            }
        }
        rects
    }
}

struct TextElement {
    input: Entity<TextInput>,
}

struct Prepaint {
    shown: Shown,
    scroll: Point<Pixels>,
    followed: (usize, usize),
    selection: Vec<PaintQuad>,
    caret: Option<PaintQuad>,
}

impl IntoElement for TextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let input = self.input.read(cx);
        let (min, max) = (input.min_lines, input.max_lines);
        let text: SharedString = input.content.clone().into();
        let style = window.text_style();
        let font = style.font();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();

        let mut layout_style = Style::default();
        layout_style.size.width = relative(1.).into();
        // The height depends on how the text wraps at the width the parent gives us.
        let layout = window.request_measured_layout(layout_style, move |known, available, window, _| {
            let width = known.width.or(match available.width {
                AvailableSpace::Definite(w) => Some(w),
                _ => None,
            });
            let rows = if max > 1 {
                let run = TextRun {
                    len: text.len(),
                    font: font.clone(),
                    color: black(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                window
                    .text_system()
                    .shape_text(text.clone(), font_size, &[run], width, None)
                    .map_or(1, |lines| lines.iter().map(|l| l.wrap_boundaries().len() + 1).sum())
            } else {
                1
            };
            size(width.unwrap_or(px(200.)), line_height * rows.clamp(min, max) as f32)
        });
        (layout, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let style = window.text_style();
        let line_height = window.line_height();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let colors = input.colors;
        let empty = input.content.is_empty();
        let (text, color): (SharedString, gpui::Hsla) = if empty {
            (input.placeholder.clone(), rgb(colors.hint).into())
        } else {
            (input.content.clone().into(), rgb(colors.text).into())
        };

        let run = TextRun {
            len: text.len(),
            font: style.font(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = match input.marked.as_ref().filter(|_| !empty) {
            Some(marked) => [
                TextRun { len: marked.start, ..run.clone() },
                TextRun {
                    len: marked.end - marked.start,
                    underline: Some(UnderlineStyle { color: Some(color), thickness: px(1.), wavy: false }),
                    ..run.clone()
                },
                TextRun { len: text.len() - marked.end, ..run },
            ]
            .into_iter()
            .filter(|r| r.len > 0)
            .collect(),
            None => vec![run],
        };

        let wrap = input.multiline().then_some(bounds.size.width);
        let lines = window
            .text_system()
            .shape_text(text, font_size, &runs, wrap, None)
            .map(|lines| lines.into_vec())
            .unwrap_or_default();
        let shown = Shown::new(lines, line_height, bounds);

        // Keep the caret in view: vertically for tall text, horizontally for a long single line.
        let cursor = input.cursor();
        let caret = shown.point_for(cursor);
        let mut scroll = input.scroll;
        let followed = (cursor, input.content.len());
        if followed != input.followed {
            if caret.y < scroll.y {
                scroll.y = caret.y;
            } else if caret.y + line_height > scroll.y + bounds.size.height {
                scroll.y = caret.y + line_height - bounds.size.height;
            }
        }
        scroll.y = scroll.y.min(shown.height - bounds.size.height).max(px(0.));
        if input.multiline() {
            scroll.x = px(0.);
        } else if caret.x < scroll.x {
            scroll.x = caret.x;
        } else if caret.x + px(2.) > scroll.x + bounds.size.width {
            scroll.x = caret.x + px(2.) - bounds.size.width;
        }
        scroll.x = scroll.x.max(px(0.));

        let origin = bounds.origin - scroll;
        let selection = input
            .selected
            .is_empty()
            .then(Vec::new)
            .unwrap_or_else(|| {
                shown
                    .selection_rects(&input.selected)
                    .into_iter()
                    .map(|r| fill(Bounds::new(origin + r.origin, r.size), rgba(colors.selection)))
                    .collect()
            });
        let caret = input.selected.is_empty().then(|| {
            fill(
                Bounds::new(origin + caret, size(px(1.5), line_height)),
                rgb(colors.caret),
            )
        });
        Prepaint { shown, scroll, followed, selection, caret }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.input.read(cx).focus.clone();
        window.handle_input(&focus, ElementInputHandler::new(bounds, self.input.clone()), cx);

        let focused = focus.is_focused(window);
        let line_height = prepaint.shown.line_height;
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for quad in prepaint.selection.drain(..) {
                window.paint_quad(quad);
            }
            for (line, top) in prepaint.shown.lines.iter().zip(&prepaint.shown.tops) {
                let origin = point(bounds.origin.x - prepaint.scroll.x, bounds.origin.y - prepaint.scroll.y + *top);
                line.paint(origin, line_height, TextAlign::Left, None, window, cx).ok();
            }
            if focused && let Some(caret) = prepaint.caret.take() {
                window.paint_quad(caret);
            }
        });

        let (scroll, followed) = (prepaint.scroll, prepaint.followed);
        let shown = std::mem::replace(
            &mut prepaint.shown,
            Shown::new(Vec::new(), line_height, bounds),
        );
        self.input.update(cx, |input, _| {
            input.shown = Some(shown);
            input.scroll = scroll;
            input.followed = followed;
        });
    }
}

impl Render for TextInput {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::delete_word))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::enter))
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::cancel))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_scroll_wheel(cx.listener(Self::scroll_wheel))
            .w_full()
            .child(TextElement { input: cx.entity() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_jumps_skip_whitespace_then_one_class() {
        let t = "foo  bar.baz";
        assert_eq!(next_word(t, 0), 3);
        assert_eq!(next_word(t, 3), 8);
        assert_eq!(next_word(t, 8), 9);
        assert_eq!(prev_word(t, t.len()), 9);
        assert_eq!(prev_word(t, 9), 8);
        assert_eq!(prev_word(t, 8), 5);
        assert_eq!(prev_word(t, 5), 0);
        assert_eq!(next_word(t, t.len()), t.len());
        assert_eq!(prev_word(t, 0), 0);
    }

    #[test]
    fn word_jumps_respect_multibyte_chars() {
        let t = "größe ändern";
        assert_eq!(next_word(t, 0), "größe".len());
        assert_eq!(prev_word(t, t.len()), "größe ".len());
    }

    #[test]
    fn stale_indices_snap_to_char_boundaries() {
        assert_eq!(floor_boundary("aä", 2), 1);
        assert_eq!(floor_boundary("ab", 9), 2);
    }
}

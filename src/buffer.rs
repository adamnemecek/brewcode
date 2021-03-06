use crate::rectangle_brush::RectangleBrush;
use std::{
    ops::Range,
    path::{Path, PathBuf},
};
use syntect::{
    highlighting::{HighlightState, Highlighter, RangedHighlightIterator, Style, ThemeSet},
    parsing::{ParseState, SyntaxSet},
};
use wgpu_glyph::{GlyphBrush, Point, Scale, SectionText, VariedSection};
use winit::{
    dpi::{PhysicalPosition, PhysicalSize},
    event::{ElementState, KeyboardInput, MouseButton, VirtualKeyCode},
};

const SCALE: f32 = 40.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Location {
    // Row must be before col so that ordering is done properly!
    row: usize,
    col: usize,
}

impl Location {
    fn new() -> Self {
        Self { row: 0, col: 0 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Span {
    start: Location,
    end: Location,
}

impl Span {
    /// Creates a new span and ensures that start <= end.
    fn new(start: Location, end: Location) -> Self {
        Self {
            start: start.min(end),
            end: start.max(end),
        }
    }

    fn contains_line(&self, line: usize) -> bool {
        self.start.row <= line && self.end.row >= line
    }

    fn get_char_indices_for_line(&self, line: usize, line_length: usize) -> Option<(usize, usize)> {
        if !self.contains_line(line) {
            return None;
        }

        // 4 Cases:
        // Start/End line
        // Start line
        // Entire line
        // End line

        if self.start.row == self.end.row {
            Some((self.start.col, self.end.col))
        } else if self.start.row == line {
            Some((self.start.col, line_length))
        } else if self.end.row == line {
            Some((0, self.end.col))
        } else {
            Some((0, line_length))
        }
    }
}

#[derive(Debug)]
struct Cursor {
    location: Location,
    col_affinity: usize,
    selection_start: Option<Location>,
}

impl Cursor {
    fn new() -> Self {
        Self {
            location: Location::new(),
            col_affinity: 0,
            selection_start: None,
        }
    }

    fn set_row(&mut self, row: usize) {
        self.location.row = row;
    }

    fn set_col(&mut self, col: usize) {
        self.location.col = col;
    }

    fn set_col_with_affinity(&mut self, col: usize) {
        self.location.col = col;
        self.col_affinity = col;
    }

    /// Takes the current selection and creates a span.
    /// Returns `None` if nothing is selected.
    fn selection_span(&self) -> Option<Span> {
        let selection_start = match self.selection_start {
            Some(selection_start) => selection_start,
            None => return None,
        };

        Some(Span::new(selection_start, self.location))
    }
}

pub struct Buffer {
    // TODO: Chunk this at maybe a few thousand lines per chunk?
    lines: Vec<String>,
    // Ughh, we have to keep this in sync with the lines vec.
    // I don't really want to create a monolith line struct which contains all this info though.
    // We will see how much of a pain it is to maintain this here, if its too difficult maybe we will combine?
    highlight_info: Vec<Vec<(Range<usize>, [f32; 4])>>,
    scroll: f32,
    cursor: Cursor,
    dragging: bool,
    size: PhysicalSize<u32>,
    path: PathBuf,
    // TODO: Move those to editor?
    syntax_set: SyntaxSet,
    theme_set: ThemeSet,
}

fn generate_highlight_info(
    lines: &[String],
    info: &mut Vec<Vec<(Range<usize>, [f32; 4])>>,
    syntax_set: &SyntaxSet,
    theme_set: &ThemeSet,
) {
    info.clear();
    // TODO: Not every file is .rs
    let syntax = syntax_set.find_syntax_by_extension("rs").unwrap();
    let highlighter = Highlighter::new(&theme_set.themes["Solarized (dark)"]);
    let mut highlight_state = HighlightState::new(&highlighter, Default::default());
    let mut parse_state = ParseState::new(syntax);

    for line in lines {
        let ops = parse_state.parse_line(line, syntax_set);
        let iter = RangedHighlightIterator::new(&mut highlight_state, &ops[..], line, &highlighter);
        info.push(
            iter.map(|(Style { foreground, .. }, _, range)| {
                (
                    range,
                    [
                        foreground.r as f32 / 255.0,
                        foreground.g as f32 / 255.0,
                        foreground.b as f32 / 255.0,
                        foreground.a as f32 / 255.0,
                    ],
                )
            })
            .collect(),
        );
    }
}

impl Buffer {
    pub fn new(size: PhysicalSize<u32>, file_name: String) -> Self {
        let path = Path::new(&file_name);
        let file = std::fs::read_to_string(path).expect("Failed to read file.");
        // TODO: Not sure if just splitting '\n' is right here.
        // I was using lines, but the trailing empty newline was omitted by lines.
        let mut lines: Vec<String> = file.split('\n').map(|line| line.to_owned()).collect();
        // Make sure we have at least one line
        if lines.is_empty() {
            lines.push(String::new());
        }
        let syntax_set = SyntaxSet::load_defaults_newlines();
        let theme_set = ThemeSet::load_defaults();
        let mut highlight_info = vec![];
        generate_highlight_info(&lines, &mut highlight_info, &syntax_set, &theme_set);
        Self {
            highlight_info,
            scroll: 0.0,
            lines,
            cursor: Cursor::new(),
            size,
            path: path.into(),
            syntax_set,
            theme_set,
            dragging: false,
        }
    }

    pub fn save(&self) {
        std::fs::write(&self.path, self.lines.join("\n")).expect("Failed to save file.");
    }

    pub fn update_size(&mut self, size: PhysicalSize<u32>) {
        self.size = size;
    }

    fn ensure_cursor_in_view(&mut self) {
        let cursor_y = self.cursor.location.row as f32 * SCALE;
        let bottom = self.scroll + self.size.height as f32;

        if cursor_y < self.scroll {
            self.scroll = cursor_y;
        } else if cursor_y + SCALE > bottom {
            self.scroll = cursor_y - self.size.height as f32 + SCALE + 5.0;
        }
    }

    pub fn scroll(&mut self, delta: f32) {
        let max_scroll = if self.lines.is_empty() {
            0.0
        } else {
            // TODO: Find better way to calculate max scroll based on line count
            ((self.lines.len() - 1) as f32 * SCALE) + 5.0
        };

        self.scroll = (self.scroll + delta).max(0.0).min(max_scroll);
    }

    pub fn handle_mouse_input(
        &mut self,
        button: MouseButton,
        state: ElementState,
        position: PhysicalPosition<i32>,
    ) {
        if button == MouseButton::Left {
            if state == ElementState::Pressed {
                self.cursor.selection_start = None;
                let location = self.hit_test(position);
                self.cursor.set_row(location.row);
                self.cursor.set_col_with_affinity(location.col);
                self.dragging = true;
            } else {
                self.dragging = false;
            }
        }
    }

    pub fn handle_mouse_move(&mut self, position: PhysicalPosition<i32>) {
        if self.dragging {
            if self.cursor.selection_start.is_none() {
                self.cursor.selection_start = Some(self.cursor.location);
            }
            let location = self.hit_test(position);
            self.cursor.set_row(location.row);
            self.cursor.set_col_with_affinity(location.col);
        }
    }

    fn hit_test(&self, position: PhysicalPosition<i32>) -> Location {
        let x_pad = 10.0;
        let digit_count = self.lines.len().to_string().chars().count();
        let gutter_offset = x_pad + 30.0 + digit_count as f32 * (SCALE / 2.0);

        let abs_position = PhysicalPosition::new(
            (position.x as f32 - gutter_offset).max(0.0),
            position.y as f32 + self.scroll,
        );

        let line = (abs_position.y / 40.0).floor() as usize;
        if line >= self.lines.len() {
            let row = self.lines.len() - 1;
            let col = self.lines.last().unwrap().len();
            Location { row, col }
        } else {
            // TODO: HACK this should not be hardcoded
            let h_advance = 19.065777;
            let col = (abs_position.x / h_advance).round() as usize;
            let row = line;
            let col = col.min(self.lines[line].len());
            Location { row, col }
        }
    }

    pub fn handle_char_input(&mut self, input: char) {
        if input == '\n' || input == '\r' {
            let new_line = self.lines[self.cursor.location.row].split_off(self.cursor.location.col);
            self.cursor.set_row(self.cursor.location.row + 1);
            self.lines.insert(self.cursor.location.row, new_line);
            self.cursor.set_col_with_affinity(0);
        // this is Backspace
        } else if input == '\u{8}' {
            if self.cursor.location.col > 0 {
                self.lines[self.cursor.location.row].remove(self.cursor.location.col - 1);
                self.cursor
                    .set_col_with_affinity(self.cursor.location.col - 1);
            } else if self.cursor.location.row > 0 {
                let remaining = self.lines.remove(self.cursor.location.row);
                self.cursor.set_row(self.cursor.location.row - 1);
                self.cursor
                    .set_col_with_affinity(self.lines[self.cursor.location.row].len());
                self.lines[self.cursor.location.row].push_str(&remaining);
            }
        // this is Delete
        } else if input == '\u{7f}' {
            if self.lines[self.cursor.location.row].len() > self.cursor.location.col {
                self.lines[self.cursor.location.row].remove(self.cursor.location.col);
            }
        } else if input == '\t' {
            // Do nothing, unless we consider how to display tab,
            // because now cursor should be moved to right one character when deleting
            // Also, now when there is \t in the file, it will not be displayed correctly
        } else {
            self.lines[self.cursor.location.row].insert(self.cursor.location.col, input);
            self.cursor.set_col(self.cursor.location.col + 1);
        }
        self.ensure_cursor_in_view();
        // TODO: recalculating highlighting every time an edit happes is pretty expensive
        // We should minimize the amount of recomputation and maybe allow for highlighting to be done
        // in a more async manner?
        generate_highlight_info(
            &self.lines,
            &mut self.highlight_info,
            &self.syntax_set,
            &self.theme_set,
        );
    }

    pub fn handle_keyboard_input(&mut self, input: KeyboardInput) {
        let keycode = match input.virtual_keycode {
            Some(keycode) => keycode,
            None => return,
        };

        if input.state == ElementState::Released {
            return;
        }

        // TODO: Support changing selection via Shift modifier and arrow keys!
        // Should be pretty easy: don't reset selection start if Shift modifier is active.
        match keycode {
            VirtualKeyCode::Up => {
                self.cursor.selection_start = None;
                let row = (self.cursor.location.row as isize - 1)
                    .max(0)
                    .min(self.lines.len() as isize) as usize;
                let col = self.lines[row].len().min(self.cursor.col_affinity);
                self.cursor.set_row(row);
                self.cursor.set_col(col);
            }
            VirtualKeyCode::Down => {
                self.cursor.selection_start = None;
                let row = (self.cursor.location.row as isize + 1)
                    .max(0)
                    .min(self.lines.len() as isize - 1) as usize;
                let col = self.lines[row].len().min(self.cursor.col_affinity);
                self.cursor.set_row(row);
                self.cursor.set_col(col);
            }
            VirtualKeyCode::Left => {
                self.cursor.selection_start = None;
                if self.cursor.location.col == 0 {
                    if self.cursor.location.row > 0 {
                        self.cursor.set_row(self.cursor.location.row - 1);
                        self.cursor
                            .set_col_with_affinity(self.lines[self.cursor.location.row].len());
                    }
                } else {
                    self.cursor
                        .set_col_with_affinity(self.cursor.location.col - 1);
                }
            }
            VirtualKeyCode::Right => {
                self.cursor.selection_start = None;
                if self.cursor.location.col >= self.lines[self.cursor.location.row].len() {
                    if self.cursor.location.row < self.lines.len() - 1 {
                        self.cursor.set_row(self.cursor.location.row + 1);
                        self.cursor.set_col_with_affinity(0);
                    }
                } else {
                    self.cursor
                        .set_col_with_affinity(self.cursor.location.col + 1);
                }
            }
            _ => {}
        }
        self.ensure_cursor_in_view();
    }

    pub fn draw(
        &self,
        size: PhysicalSize<u32>,
        glyph_brush: &mut GlyphBrush<()>,
        rect_brush: &mut RectangleBrush,
    ) {
        // TODO: This draw method is getting a bit unweidly, we should split some stuff
        // into a layout pass to simplify drawing.

        let x_pad = 10.0;
        let digit_count = self.lines.len().to_string().chars().count();
        let gutter_offset = x_pad + 30.0 + digit_count as f32 * (SCALE / 2.0);
        let mut y = 5.0 - self.scroll;

        // gutter color
        rect_brush.queue_rectangle(
            0,
            0,
            (digit_count as f32 * (SCALE / 2.0) + x_pad * 2.0) as i32,
            size.height as i32,
            [0.06, 0.06, 0.06, 1.0],
        );

        let selection_span = self.cursor.selection_span();

        for (index, (line, highlight)) in self
            .lines
            .iter()
            .zip(self.highlight_info.iter())
            .enumerate()
        {
            if y < -SCALE {
                y += SCALE;
                continue;
            }
            if y > size.height as f32 {
                break;
            }

            let mut line_no_color = [0.4, 0.4, 0.4, 1.0];

            // Paint selection boxes
            if let Some((start, end)) =
                selection_span.and_then(|span| span.get_char_indices_for_line(index, line.len()))
            {
                // TODO: Gah, we should not do this. We should do a single layout pass and add some
                // methods that lets us query glyph locations.
                let layout = glyph_brush.fonts().first().unwrap().layout(
                    line,
                    Scale::uniform(SCALE),
                    Point { x: 0.0, y: 0.0 },
                );
                let mut x_pos = 0.0;
                let mut x_start = 0.0;
                for (i, positioned_glyph) in layout.enumerate().take(end) {
                    if i == start {
                        x_start = x_pos;
                    }
                    x_pos += positioned_glyph.unpositioned().h_metrics().advance_width;
                }
                let width = (x_pos - x_start) as i32;
                let x = x_start as i32;

                rect_brush.queue_rectangle(
                    (x as f32 + gutter_offset) as i32,
                    y as i32,
                    width,
                    SCALE as i32,
                    [0.0, 0.0, 1.0, 0.1],
                );
            }

            if index == self.cursor.location.row {
                line_no_color = [1.0, 1.0, 1.0, 1.0];

                let mut layout = glyph_brush.fonts().first().unwrap().layout(
                    line,
                    Scale::uniform(SCALE),
                    Point { x: 0.0, y: 0.0 },
                );
                let mut x_pos = 0.0;
                for _ in 0..self.cursor.location.col {
                    let positioned_glyph = layout.next().unwrap();
                    x_pos += positioned_glyph.unpositioned().h_metrics().advance_width;
                }

                let cursor_x = gutter_offset + x_pos;
                // active line
                rect_brush.queue_rectangle(
                    0,
                    y as i32,
                    size.width as i32,
                    SCALE as i32,
                    [1.0, 1.0, 1.0, 0.05],
                );

                rect_brush.queue_rectangle(
                    cursor_x as i32 - 2,
                    y as i32,
                    4,
                    SCALE as i32,
                    [1.0, 1.0, 1.0, 1.0],
                );
            }

            let line_number = index + 1;

            glyph_brush.queue(VariedSection {
                screen_position: (x_pad, y),
                text: vec![SectionText {
                    text: &line_number.to_string(),
                    // TODO: Don't hardcode scale
                    scale: Scale::uniform(SCALE),
                    color: line_no_color,
                    ..SectionText::default()
                }],
                ..VariedSection::default()
            });

            let text = highlight
                .iter()
                .map(|(range, color)| SectionText {
                    text: &line[range.clone()],
                    scale: Scale::uniform(SCALE),
                    color: *color,
                    ..SectionText::default()
                })
                .collect();

            glyph_brush.queue(VariedSection {
                screen_position: (gutter_offset, y),
                text,
                ..VariedSection::default()
            });

            y += SCALE;
        }
    }
}

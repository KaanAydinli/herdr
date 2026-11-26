use super::Document;
use ratatui::{layout::Rect, style::Color, Frame};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

fn width(g: &str, col: usize) -> usize {
    if g == "\t" {
        4 - col % 4
    } else {
        g.width().max(1)
    }
}

pub fn char_at_cell(line: &str, cell: usize) -> usize {
    let (mut col, mut index) = (0, 0);
    for g in line.graphemes(true) {
        col += width(g, col);
        if col > cell {
            break;
        }
        index += g.chars().count();
    }
    index
}

fn reveal(top: usize, cursor: usize, size: u16) -> usize {
    top.min(cursor)
        .max((cursor + 1).saturating_sub(usize::from(size.max(1))))
}

pub fn prepare(r: Rect, d: &mut Document) {
    if r.is_empty() {
        return;
    }
    let cursor = d.area.cursor();
    let mut cursor_col = 0;
    let mut chars = 0;
    for g in d.area.lines()[cursor.0].graphemes(true) {
        if chars >= cursor.1 {
            break;
        }
        cursor_col += width(g, cursor_col);
        chars += g.chars().count();
    }
    d.top = reveal(d.top, cursor.0, r.height);
    d.left = reveal(d.left, cursor_col, r.width);
    d.colors
        .advance(&d.path, d.area.lines(), d.top + usize::from(r.height));
    d.cursor_cell = cursor_col;
}

pub fn draw(f: &mut Frame<'_>, r: Rect, d: &Document) {
    use ratatui::style::Style;
    if r.is_empty() {
        return;
    }
    let selection = d.area.selection_range();
    for ((row, line), y) in d
        .area
        .lines()
        .iter()
        .enumerate()
        .skip(d.top)
        .zip(r.y..r.bottom())
    {
        let mut tokens = d.colors.line(row).iter().peekable();
        let mut matches = d
            .area
            .search_pattern()
            .map(|p| p.find_iter(line).peekable());
        let (mut col, mut index) = (0, 0);

        for (i, g) in line.grapheme_indices(true) {
            if col >= d.left + usize::from(r.width) {
                break;
            }
            let w = width(g, col);
            if col >= d.left {
                while tokens.peek().is_some_and(|(end, _)| *end <= i) {
                    tokens.next();
                }
                let fg = tokens.peek().map_or(super::FG, |(_, fg)| *fg);
                let mut style = Style::new().fg(fg).bg(super::BG);
                if let Some(matches) = &mut matches {
                    while matches.peek().is_some_and(|m| m.end() <= i) {
                        matches.next();
                    }
                    if matches.peek().is_some_and(|m| m.start() <= i) {
                        style = style.bg(Color::Rgb(158, 106, 3));
                    }
                }
                if selection.is_some_and(|(a, b)| a <= (row, index) && (row, index) < b) {
                    style = style.bg(Color::Rgb(38, 79, 120));
                }
                let x = r.x + (col - d.left) as u16;
                let text = if g == "\t" {
                    &"    "[..w]
                } else if g.chars().any(char::is_control) {
                    "�"
                } else {
                    g
                };
                f.buffer_mut()
                    .set_stringn(x, y, text, usize::from(r.right() - x), style);
            }
            col += w;
            index += g.chars().count();
        }
    }
}

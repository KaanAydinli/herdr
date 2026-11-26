use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Visible bytes and caret column for a single-line field. The cursor is a
/// byte offset on a grapheme boundary. Reserve one cell for the terminal caret.
pub(crate) fn input_viewport(text: &str, cursor: usize, width: u16) -> (Range<usize>, u16) {
    if width == 0 {
        return (cursor..cursor, 0);
    }
    let mut start = cursor;
    let mut cells = 0;
    for (index, grapheme) in text[..cursor].grapheme_indices(true).rev() {
        let next = cells + grapheme.width();
        if next >= usize::from(width) {
            break;
        }
        start = index;
        cells = next;
    }
    let mut end = cursor;
    let mut used = cells;
    for grapheme in text[cursor..].graphemes(true) {
        used += grapheme.width();
        if used > usize::from(width) {
            break;
        }
        end += grapheme.len();
    }
    (start..end, cells as u16)
}

pub(crate) fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

pub(crate) fn truncate_end(text: &str, max_width: usize) -> String {
    if display_width(text) <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    if max_width == 1 {
        return "…".to_string();
    }

    let prefix = take_prefix_width(text, max_width.saturating_sub(1));
    format!("{prefix}…")
}

fn take_prefix_width(text: &str, max_width: usize) -> String {
    let mut output = String::new();
    let mut width = 0usize;
    for ch in text.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width > max_width {
            break;
        }
        output.push(ch);
        width += ch_width;
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_end_uses_display_width() {
        let text = truncate_end("提交 herdr 的反馈", 16);

        assert_eq!(text, "提交 herdr 的反…");
        assert!(display_width(&text) <= 16);
    }
}

use super::*;
use ratatui::layout::Position;

pub(super) fn send_clipboard(buffer: &mut String, text: String) -> io::Result<()> {
    if !text.is_empty() {
        *buffer = text;
        if buffer.len() > ghostty_vt::MAX_CLIPBOARD_BYTES {
            return Err(io::Error::other(format!(
                "Copied in Code. System clipboard limit is {} KiB.",
                ghostty_vt::MAX_CLIPBOARD_BYTES / 1024
            )));
        }
        // The workspace may be remote. Herdr forwards OSC 52 to the attached client.
        let mut out = io::stdout().lock();
        out.write_all(crate::selection::osc52_sequence(buffer.as_bytes()).as_bytes())?;
        out.flush()?;
    }
    Ok(())
}

impl Editor {
    pub(super) fn select_text_at(&mut self, pos: Position, begin: bool) -> bool {
        let area = self.geometry.editor;
        let right = area.right().min(self.geometry.tree.x);
        if area.is_empty() || right <= area.x {
            return false;
        }
        let row = usize::from(pos.y.clamp(area.y, area.bottom() - 1) - area.y);
        let col = usize::from(pos.x.clamp(area.x, right) - area.x);
        let Some(tab) = self.docs.get_mut(self.active) else {
            return false;
        };
        if let Some(preview) = tab.preview_mut() {
            return preview.select_at(row, col, begin);
        }
        let Some(doc) = tab.doc_mut() else {
            return false;
        };
        let row = (doc.top + row).min(doc.area.lines().len() - 1);
        let col = syntax::char_at_cell(&doc.area.lines()[row], doc.left + col);
        if begin {
            doc.area.cancel_selection();
        } else if doc.area.cursor() != (row, col) && !doc.area.is_selecting() {
            doc.area.start_selection();
        }
        doc.jump(row, col);
        true
    }

    pub(super) fn copy_selection(&mut self) -> io::Result<()> {
        let Some(tab) = self.docs.get(self.active) else {
            return Ok(());
        };
        let selected = if let Some(preview) = tab.preview() {
            preview.selected_text()
        } else {
            tab.doc().and_then(Document::selected_text)
        };
        if let Some(text) = selected {
            send_clipboard(&mut self.clipboard, text)?;
        }
        Ok(())
    }
}

impl Document {
    fn selected_text(&self) -> Option<String> {
        let (start, end) = self.area.selection_range()?;
        if start == end {
            return None;
        }
        let mut text = String::new();
        for row in start.0..=end.0 {
            let line = &self.area.lines()[row];
            let byte = |col| line.char_indices().nth(col).map_or(line.len(), |(i, _)| i);
            let first = if row == start.0 { byte(start.1) } else { 0 };
            let last = if row == end.0 {
                byte(end.1)
            } else {
                line.len()
            };
            if row > start.0 {
                text.push('\n');
            }
            text.push_str(&line[first..last]);
        }
        Some(text)
    }
}

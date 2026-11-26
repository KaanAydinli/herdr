use super::explorer::TreeMode;
use super::*;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    layout::Position,
    style::{Modifier, Style},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
    Frame,
};
use ratatui_textarea::TextArea;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const PANEL: Color = Color::Rgb(24, 24, 24);
const SUBTLE: Color = Color::Rgb(157, 157, 157);
const ACTIVE: Color = Color::Rgb(0, 120, 212);

fn line(f: &mut Frame<'_>, r: Rect, text: &str, style: Style) {
    f.render_widget(Paragraph::new(text).style(style), r);
}
fn row(r: Rect, y: u16) -> Rect {
    Rect::new(r.x, y, r.width, 1)
}
fn label(f: &mut Frame<'_>, x: u16, y: u16, max: u16, text: &str, style: Style) -> Rect {
    let width = max
        .saturating_sub(x)
        .min(text.width().min(u16::MAX as usize) as u16);
    let r = Rect::new(x, y, width, 1);
    line(f, r, text, style);
    r
}

fn draw_rename_field(f: &mut Frame<'_>, field: Rect, input: &TextArea<'_>, style: Style) {
    let name = &input.lines()[0];
    // TextArea indexes Unicode scalars. Terminal cells can contain a whole grapheme.
    let byte_at = |column: usize| {
        let mut chars = 0;
        for (byte, grapheme) in name.grapheme_indices(true) {
            if chars >= column {
                return byte;
            }
            chars += grapheme.chars().count();
        }
        name.len()
    };
    let (visible, caret) = crate::ui::input_viewport(name, byte_at(input.cursor().1), field.width);
    line(f, field, &name[visible.clone()], style);
    if let Some(((_, start), (_, end))) = input.selection_range() {
        let start = byte_at(start).clamp(visible.start, visible.end);
        let end = byte_at(end).clamp(start, visible.end);
        let x = field.x + name[visible.start..start].width() as u16;
        let width = name[start..end].width() as u16;
        f.buffer_mut().set_style(
            Rect::new(x, field.y, width, 1),
            style.bg(Color::Rgb(38, 79, 120)),
        );
    }
    f.set_cursor_position((field.x + caret, field.y));
}
fn offset(index: usize, height: u16) -> usize {
    index.saturating_add(1).saturating_sub(height as usize)
}

// Preserve the trailing characters of a path or an input wider than its field.
fn path_tail(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let mut used = 1;
    let mut start = text.len();
    for (i, g) in text.grapheme_indices(true).rev() {
        used += g.width();
        if used > width {
            break;
        }
        start = i;
    }
    format!("…{}", &text[start..])
}

impl Editor {
    pub(super) fn layout(&self, r: Rect) -> (Rect, Rect, Rect, bool) {
        let narrow = r.width < 80;
        let tree_width = if self.tree {
            self.tree_width
                .unwrap_or((r.width / 3).clamp(20, 36))
                .min(if narrow {
                    r.width
                } else {
                    r.width.saturating_sub(28)
                })
        } else {
            0
        };
        let body = Rect::new(0, 2, r.width, r.height.saturating_sub(3));
        // Native image placements cannot be covered by text drawn over them.
        // Fit media into the visible part of the editor even on a narrow pane.
        let overlay_tree =
            narrow && !matches!(self.docs.get(self.active), Some(FileTab::Media { .. }));
        let editor = Rect {
            width: if overlay_tree {
                r.width
            } else {
                r.width - tree_width
            },
            ..body
        };
        let tree = Rect {
            x: r.width - tree_width,
            width: tree_width,
            ..body
        };
        (body, editor, tree, narrow)
    }

    pub(super) fn draw(&self, f: &mut Frame<'_>) -> Geometry {
        let r = f.area();
        if r.is_empty() {
            return Geometry::default();
        }
        let base = Style::new().bg(BG).fg(FG);
        let chrome = Style::new().bg(PANEL).fg(SUBTLE);
        f.render_widget(Block::default().style(base), r);
        let (body, editor, tree, narrow) = self.layout(r);
        let mut g = Geometry {
            editor,
            tree,
            narrow,
            ..Default::default()
        };
        let files_x = r.width.saturating_sub(9);
        let search_end = files_x.saturating_sub(2);
        let search_x = search_end.saturating_sub(9);
        g.files = label(
            f,
            files_x,
            0,
            r.width,
            " ☰ Files ",
            if self.tree { chrome.fg(FG) } else { chrome },
        );
        g.search = label(
            f,
            search_x,
            0,
            search_end,
            " ⌕ Search",
            if self.search.is_some() {
                chrome.fg(Color::White)
            } else {
                chrome
            },
        );
        let tab_end = if self.docs.get(self.active).and_then(FileTab::doc).is_none() {
            search_x
        } else {
            let save_x = search_x.saturating_sub(6);
            g.save = label(f, save_x, 0, search_x, " Save ", chrome);
            save_x
        };
        let border = chrome.fg(Color::Rgb(79, 79, 79));
        if r.height > 1 {
            f.render_widget(
                Block::default().borders(Borders::TOP).border_style(border),
                row(r, 1),
            );
        }
        let name = |idx: usize| {
            let doc = &self.docs[idx];
            let marker = if doc.doc().is_some_and(|d| d.external) {
                " !"
            } else if doc.dirty() {
                " •"
            } else {
                ""
            };
            format!(
                " {}{} ",
                doc.path().file_name().unwrap_or_default().to_string_lossy(),
                marker
            )
        };
        let mut first = self.active;
        if !self.docs.is_empty() {
            let mut used = name(first).width() + 4;
            while first > 0 {
                let prev = name(first - 1).width() + 3;
                if used + prev > usize::from(tab_end) {
                    break;
                }
                used += prev;
                first -= 1;
            }
        }
        let mut x = 0;
        if !self.docs.is_empty() && tab_end > 0 {
            label(f, 0, 0, tab_end, "│", border);
            x = 1;
        }
        for idx in first..self.docs.len() {
            let remaining = tab_end.saturating_sub(x);
            if remaining < 4 {
                break;
            }
            let title = name(idx);
            let style = if idx == self.active {
                base.fg(Color::White).add_modifier(Modifier::BOLD)
            } else {
                chrome
            };
            let title_rect = label(f, x, 0, tab_end - 3, &title, style);
            let close = label(f, title_rect.right(), 0, tab_end, "× ", style.fg(SUBTLE));
            let edge = label(f, close.right(), 0, tab_end, "│", border);
            if r.height > 1 {
                label(f, edge.x, 1, tab_end, "┴", border);
            }
            g.tabs.push((Rect::new(x, 0, edge.right() - x, 1), idx));
            g.close_icons.push((close, idx));
            x = edge.right();
        }
        let tab = self.docs.get(self.active);
        if let Some(view) = tab.and_then(FileTab::preview) {
            view.draw(f, editor);
        } else if let Some(doc) = tab.and_then(FileTab::doc) {
            syntax::draw(f, editor, doc);
            let x = doc.cursor_cell.saturating_sub(doc.left);
            let y = doc.area.cursor().0.saturating_sub(doc.top);
            if self.search.is_none()
                && self.compare.is_none()
                && self.close_menu.is_none()
                && self.tree_dialog.is_none()
                && x < usize::from(editor.width)
                && y < usize::from(editor.height)
            {
                let pos = Position::new(editor.x + x as u16, editor.y + y as u16);
                if !tree.contains(pos) {
                    f.set_cursor_position(pos);
                }
            }
        } else {
            line(f, editor, "Open a file from Files or press Ctrl+P", base);
        }
        if self.tree && tree.height > 0 {
            f.render_widget(Clear, tree);
            f.render_widget(Block::default().style(chrome).borders(Borders::LEFT), tree);
            let width = tree.width.saturating_sub(1);
            let inner = Rect::new(tree.x + 1, tree.y, width, tree.height);
            for ((path, dir, depth), y) in self
                .files
                .rows
                .iter()
                .skip(self.files.scroll)
                .zip(inner.y..inner.bottom())
            {
                let arrow = if *dir {
                    if self.files.is_open(path) {
                        "▾ "
                    } else {
                        "▸ "
                    }
                } else {
                    "  "
                };
                let text = format!(
                    "{}{}{}{}",
                    "  ".repeat((*depth).min(16)),
                    arrow,
                    path.file_name().unwrap_or_default().to_string_lossy(),
                    if *dir { "/" } else { "" }
                );
                let style = if *dir {
                    chrome
                        .fg(Color::Rgb(117, 190, 255))
                        .add_modifier(Modifier::BOLD)
                } else if self.docs.get(self.active).is_some_and(|d| d.path() == path) {
                    base.bg(Color::Rgb(38, 79, 120)).fg(Color::White)
                } else {
                    chrome.fg(FG)
                };
                line(f, row(inner, y), &text, style);
            }
        }
        let bottom = row(r, r.bottom() - 1);
        let mut x = 0;
        if let Some(tab) = tab {
            let preview = tab.preview();
            let toggle = if tab.markdown() {
                Some(if preview.is_some() {
                    " Source "
                } else {
                    " Preview "
                })
            } else if let Some(p) = preview.filter(|p| p.kind == Format::Pdf) {
                Some(if p.text_mode {
                    " Page view "
                } else {
                    " Text view "
                })
            } else {
                None
            };
            if let Some(title) = toggle {
                g.toggle_preview = label(f, x, bottom.y, r.width, title, base.bg(ACTIVE));
                x = g.toggle_preview.right();
            }
            if let Some(p) = preview.filter(|p| p.kind == Format::Pdf) {
                g.previous_page = label(f, x, bottom.y, r.width, " ‹ ", chrome);
                x = g.previous_page.right();
                let pages = p.pages.map_or_else(|| "…".into(), |n| n.to_string());
                x = label(
                    f,
                    x,
                    bottom.y,
                    r.width,
                    &format!("{}/{}", p.page, pages),
                    base,
                )
                .right();
                g.next_page = label(f, x, bottom.y, r.width, " › ", chrome);
                x = g.next_page.right();
            }
        }
        let status = if !self.status.is_empty() {
            self.status.clone()
        } else if let Some(tab) = tab.filter(|t| t.preview().is_some()) {
            format!(
                " {}",
                tab.path()
                    .strip_prefix(&self.files.root)
                    .unwrap_or(tab.path())
                    .display()
            )
        } else if let Some(d) = tab.and_then(FileTab::doc) {
            let pos = d.area.cursor();
            format!(
                " {}   Ln {}, Col {}   {}",
                d.path
                    .strip_prefix(&self.files.root)
                    .unwrap_or(&d.path)
                    .display(),
                pos.0 + 1,
                pos.1 + 1,
                if d.external {
                    "Changed on disk · Save to compare"
                } else {
                    "F2 save · Ctrl+F find"
                }
            )
        } else {
            " Ctrl+P files · Ctrl+B explorer".into()
        };
        line(
            f,
            Rect::new(x, bottom.y, tree.x.saturating_sub(x), 1),
            &status,
            chrome,
        );
        if self.tree && r.height > 2 && tree.width > 1 {
            let footer = Rect::new(tree.x, bottom.y, tree.width, 1);
            f.render_widget(
                Block::default().style(chrome).borders(Borders::LEFT),
                footer,
            );
            g.parent_directory = label(
                f,
                footer.x + 1,
                footer.y,
                footer.right(),
                " < ",
                if self.files.root.parent().is_some() {
                    chrome.fg(FG)
                } else {
                    border
                },
            );
            let available = usize::from(footer.right().saturating_sub(g.parent_directory.right()));
            let cwd = &self.files.display_root;
            let path = if cwd.starts_with('~') && cwd.width() > available && available > 4 {
                format!(
                    "~{}{}",
                    std::path::MAIN_SEPARATOR,
                    path_tail(cwd, available - 2)
                )
            } else {
                path_tail(cwd, available)
            };
            label(
                f,
                g.parent_directory.right(),
                footer.y,
                footer.right(),
                &path,
                chrome.fg(FG),
            );
        }
        if let Some(search) = &self.search {
            let title = match search.kind {
                SearchKind::Files => " Open file ",
                SearchKind::Workspace => " Search files ",
                SearchKind::Find => " Find in file ",
            };
            let results = if search.kind == SearchKind::Find {
                0
            } else {
                search.hits.len().clamp(1, 7) as u16
            };
            let width = r.width.min(58);
            let panel = Rect::new(
                g.search.right().saturating_sub(width),
                body.y,
                width,
                (results + 3).min(body.height),
            );
            let block = Block::bordered()
                .border_type(BorderType::Rounded)
                .title(title)
                .style(chrome.fg(FG))
                .border_style(border);
            let inside = block.inner(panel);
            g.search_panel = panel;
            f.render_widget(Clear, panel);
            f.render_widget(block, panel);
            if panel.width >= 12 {
                g.search_close = label(
                    f,
                    panel.right() - 4,
                    panel.y,
                    panel.right() - 1,
                    " × ",
                    chrome,
                );
            }
            if !inside.is_empty() {
                let input = Rect::new(inside.x, inside.y, inside.width, 1);
                let query = path_tail(&search.query, usize::from(input.width.saturating_sub(2)));
                line(f, input, &format!(" {query}"), base);
                f.set_cursor_position(Position::new(
                    (input.x + 1 + query.width() as u16).min(input.right() - 1),
                    input.y,
                ));
                g.picker = Rect::new(
                    inside.x,
                    inside.y + 1,
                    inside.width,
                    inside.height.saturating_sub(1),
                );
                let start = offset(search.selected, g.picker.height);
                for (i, y) in (start..).zip(g.picker.y..g.picker.bottom()) {
                    let text = if let Some(hit) = search.hits.get(i) {
                        let path = hit.path.strip_prefix(&self.files.root).unwrap_or(&hit.path);
                        if search.kind == SearchKind::Files {
                            format!(" {}", path.display())
                        } else {
                            format!(" {}:{}  {}", path.display(), hit.row + 1, hit.preview)
                        }
                    } else if search.job.is_some() {
                        " Searching…".into()
                    } else if search.query.is_empty() {
                        " Type to search".into()
                    } else {
                        " No matches".into()
                    };
                    let selected = i == search.selected && i < search.hits.len();
                    line(
                        f,
                        row(g.picker, y),
                        &text,
                        if selected {
                            base.bg(Color::Rgb(38, 79, 120))
                        } else {
                            chrome
                        },
                    );
                }
            }
        }
        if let (Some(c), Some(d)) = (&self.compare, tab.and_then(FileTab::doc)) {
            f.render_widget(Clear, r);
            let left = Rect::new(0, 0, r.width / 2, r.height);
            let right = Rect::new(left.width, 0, r.width - left.width, r.height);
            line(f, row(left, 0), " Your buffer", base.bg(ACTIVE));
            line(
                f,
                row(right, 0),
                if c.disk.is_some() {
                    " On disk"
                } else {
                    " Deleted on disk"
                },
                chrome,
            );
            let start = c.row.min(
                d.area
                    .lines()
                    .len()
                    .max(c.disk.as_ref().map_or(0, |s| s.lines().count()))
                    .saturating_sub(1),
            );
            let mut disk = c.disk.as_deref().unwrap_or("").lines().skip(start);
            for (i, y) in (start..).zip(body.y..body.bottom()) {
                line(
                    f,
                    row(left, y),
                    d.area.lines().get(i).map_or("", String::as_str),
                    base,
                );
                line(f, row(right, y), disk.next().unwrap_or(""), chrome);
            }
            let mut x = 0;
            for (name, key) in [
                (" Esc Back ", KeyCode::Esc),
                (" R Load disk ", KeyCode::Char('r')),
                (" W Write buffer ", KeyCode::Char('w')),
            ] {
                let button = label(f, x, bottom.y, r.width, name, chrome);
                x = button.right();
                g.compare_buttons.push((button, key));
            }
            if !self.status.is_empty() && r.height > 2 {
                line(f, row(r, bottom.y - 1), &self.status, base);
            }
        }
        if let Some(menu) = self.close_menu {
            let action = " Discard changes ";
            let cancel = " Cancel ";
            let width = action.width() + cancel.width();
            let x = menu.x.min(r.width.saturating_sub(width as u16));
            let y = 1.min(r.bottom() - 1);
            g.close_action = label(f, x, y, r.width, action, base.bg(Color::Rgb(114, 46, 46)));
            label(f, g.close_action.right(), y, r.width, cancel, chrome);
        }
        if let Some(dialog) = &self.tree_dialog {
            match &dialog.mode {
                TreeMode::Menu(pos) => {
                    let width = tree.width.min(14);
                    let height = tree.height.min(4);
                    let menu = Rect::new(
                        pos.x.clamp(tree.x, tree.right() - width),
                        pos.y.clamp(tree.y, tree.bottom() - height),
                        width,
                        height,
                    );
                    let block = Block::bordered()
                        .border_type(BorderType::Rounded)
                        .style(chrome)
                        .border_style(border);
                    let inner = block.inner(menu);
                    f.render_widget(Clear, menu);
                    f.render_widget(block, menu);
                    if inner.height > 0 {
                        g.tree_primary = row(inner, inner.y);
                        line(f, g.tree_primary, " Rename", chrome.fg(FG));
                    }
                    if inner.height > 1 {
                        g.tree_secondary = row(inner, inner.y + 1);
                        line(f, g.tree_secondary, " Delete", chrome.fg(Color::LightRed));
                    }
                }
                mode => {
                    let rename = matches!(mode, TreeMode::Rename(_));
                    let title = if rename { " Rename " } else { " Delete " };
                    // Native bitmaps sit above terminal text. Keep file actions in
                    // the explorer so the open image can keep its placement.
                    let bounds = if matches!(tab, Some(FileTab::Media { .. })) {
                        tree
                    } else {
                        r
                    };
                    let width = bounds.width.min(54);
                    let field_width = width.saturating_sub(2);
                    let stacked = field_width < 17;
                    let prompt = match mode {
                        TreeMode::Delete => {
                            let name = dialog
                                .path
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy();
                            let kind = if dialog.dir {
                                "folder and contents"
                            } else {
                                "file"
                            };
                            format!("Delete {kind}?  {name}")
                        }
                        TreeMode::Deleting => "Deleting…".into(),
                        _ => String::new(),
                    };
                    let prompt = Paragraph::new(prompt)
                        .style(chrome.fg(FG))
                        .wrap(Wrap { trim: true });
                    let error = Paragraph::new(dialog.error.as_str())
                        .style(chrome.fg(Color::LightRed))
                        .wrap(Wrap { trim: true });
                    let field_rows = prompt.line_count(field_width).clamp(1, 4) as u16;
                    let error_rows = if dialog.error.is_empty() {
                        0
                    } else {
                        error.line_count(field_width).min(3) as u16
                    };
                    let height = bounds
                        .height
                        .min(4 + field_rows + error_rows + u16::from(stacked));
                    let panel = Rect::new(
                        bounds.x + (bounds.width - width) / 2,
                        bounds.y + (bounds.height - height) / 2,
                        width,
                        height,
                    );
                    let block = Block::bordered()
                        .title(title)
                        .border_type(BorderType::Rounded)
                        .style(chrome.fg(FG))
                        .border_style(border);
                    let inner = block.inner(panel);
                    f.render_widget(Clear, panel);
                    f.render_widget(block, panel);
                    if !inner.is_empty() {
                        let field = Rect {
                            height: field_rows.min(inner.height),
                            ..inner
                        };
                        if let TreeMode::Rename(input) = mode {
                            draw_rename_field(f, field, input, base);
                        } else {
                            f.render_widget(prompt, field);
                        }
                        if error_rows > 0 {
                            let height = error_rows.min(inner.bottom() - field.bottom());
                            f.render_widget(
                                error,
                                Rect::new(inner.x, field.bottom(), inner.width, height),
                            );
                        }
                        if inner.height >= 2 && !matches!(mode, TreeMode::Deleting) {
                            g.tree_secondary = label(
                                f,
                                inner.x,
                                inner.bottom() - 1 - u16::from(stacked),
                                inner.right(),
                                " Cancel ",
                                chrome,
                            );
                            g.tree_primary = label(
                                f,
                                if stacked {
                                    inner.x
                                } else {
                                    g.tree_secondary.right().saturating_add(1)
                                },
                                inner.bottom() - 1,
                                inner.right(),
                                if rename { " Rename " } else { " Delete " },
                                base.bg(if rename {
                                    ACTIVE
                                } else {
                                    Color::Rgb(114, 46, 46)
                                }),
                            );
                        }
                    }
                }
            }
        }
        g
    }

    pub(super) fn mouse(&mut self, m: MouseEvent) -> io::Result<()> {
        let pos = Position::new(m.column, m.row);
        let g = &self.geometry;
        let click = m.kind == MouseEventKind::Down(MouseButton::Left);
        let release = m.kind == MouseEventKind::Up(MouseButton::Left);
        if self.selecting_text && (release || m.kind == MouseEventKind::Drag(MouseButton::Left)) {
            self.select_text_at(pos, false);
            if release {
                self.selecting_text = false;
                if self.copy_on_select {
                    self.copy_selection()?;
                }
            }
            return Ok(());
        }
        if let Some(s) = &mut self.search {
            if click && (g.search_close.contains(pos) || !g.search_panel.contains(pos)) {
                self.dismiss_search();
            } else if click && g.picker.contains(pos) {
                let index = offset(s.selected, g.picker.height) + (m.row - g.picker.y) as usize;
                if index < s.hits.len() {
                    s.selected = index;
                    self.choose()?;
                }
            }
            return Ok(());
        }
        if m.kind == MouseEventKind::Down(MouseButton::Right) {
            if self.tree && g.tree.contains(pos) && pos.x > g.tree.x {
                let index = self.files.scroll + usize::from(pos.y - g.tree.y);
                if let Some((path, dir, _)) = self.files.rows.get(index) {
                    self.tree_menu(path.clone(), *dir, pos);
                }
            }
            return Ok(());
        }
        if click && g.parent_directory.contains(pos) {
            if let Some(parent) = self.files.root.parent().map(|p| p.to_owned()) {
                self.enter_directory(&parent)?;
            }
            return Ok(());
        }
        if click && !g.tree.contains(pos) {
            self.last_folder_click = None;
        }
        if click {
            if let Some((rect, index)) = g.close_icons.iter().find(|(rect, _)| rect.contains(pos)) {
                return self.request_close(*index, rect.x);
            }
        }
        if self.resizing {
            if let MouseEventKind::Drag(MouseButton::Left) = m.kind {
                self.tree_width = Some(g.tree.right().saturating_sub(m.column).max(12));
            } else if matches!(m.kind, MouseEventKind::Up(_)) {
                self.resizing = false;
            }
            return Ok(());
        }
        if click && self.tree && g.tree.contains(pos) && m.column == g.tree.x {
            self.resizing = true;
            return Ok(());
        }
        if click && g.toggle_preview.contains(pos) {
            self.toggle_preview();
            return Ok(());
        }
        if click && (g.previous_page.contains(pos) || g.next_page.contains(pos)) {
            if let Some(p) = self
                .docs
                .get_mut(self.active)
                .and_then(FileTab::preview_mut)
            {
                p.set_page(if g.previous_page.contains(pos) { -1 } else { 1 });
            }
            return Ok(());
        }
        if click && g.save.contains(pos) {
            return self.save();
        }
        if click && g.files.contains(pos) {
            self.tree = !self.tree;
        } else if click && g.search.contains(pos) {
            self.begin(SearchKind::Workspace);
        } else if let Some((_, i)) = g.tabs.iter().find(|(r, _)| click && r.contains(pos)) {
            self.active = *i;
        } else if self.tree && g.tree.contains(pos) {
            match m.kind {
                MouseEventKind::ScrollDown => {
                    self.files.scroll = (self.files.scroll + 3)
                        .min(self.files.rows.len().saturating_sub(g.tree.height as usize))
                }
                MouseEventKind::ScrollUp => self.files.scroll = self.files.scroll.saturating_sub(3),
                _ if click => {
                    if let Some((path, dir, _)) = self
                        .files
                        .rows
                        .get(self.files.scroll + (m.row - g.tree.y) as usize)
                        .cloned()
                    {
                        if dir {
                            self.click_folder(path)?;
                        } else {
                            self.last_folder_click = None;
                            self.open(path, None)?;
                        }
                    }
                }
                _ => {}
            }
        } else if g.editor.contains(pos) {
            if click {
                self.selecting_text = self.select_text_at(pos, true);
                return Ok(());
            }
            if let Some(p) = self
                .docs
                .get_mut(self.active)
                .and_then(FileTab::preview_mut)
            {
                match m.kind {
                    MouseEventKind::ScrollDown => p.scroll(3, g.editor.height),
                    MouseEventKind::ScrollUp => p.scroll(-3, g.editor.height),
                    _ => {}
                }
                return Ok(());
            }
            if let Some(d) = self.docs.get_mut(self.active).and_then(FileTab::doc_mut) {
                match m.kind {
                    MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                        let (row, col) = d.area.cursor();
                        let max = d.area.lines().len().saturating_sub(1);
                        d.top = if m.kind == MouseEventKind::ScrollDown {
                            (d.top + 3).min(
                                max.saturating_sub(usize::from(g.editor.height.saturating_sub(1))),
                            )
                        } else {
                            d.top.saturating_sub(3)
                        };
                        d.jump(
                            row.clamp(
                                d.top,
                                d.top + usize::from(g.editor.height.saturating_sub(1)),
                            ),
                            col,
                        );
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }
}

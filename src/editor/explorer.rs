//! Mouse actions on explorer entries. A menu keeps its target path even if a
//! filesystem notification reorders the rows while the menu is open.
use super::*;
use crossterm::event::{MouseButton, MouseEventKind};
use ratatui::layout::Position;
use ratatui_textarea::TextArea;
use std::{
    fs,
    path::{Component, Path},
    time::{Duration, Instant},
};

pub(super) struct TreeDialog {
    pub path: PathBuf,
    pub dir: bool,
    pub mode: TreeMode,
    pub error: String,
}

pub(super) enum TreeMode {
    Menu(Position),
    Rename(Box<TextArea<'static>>),
    Delete,
    Deleting,
}

impl Editor {
    pub(super) fn enter_directory(&mut self, path: &Path) -> io::Result<()> {
        self.last_folder_click = None;
        self.files.enter(path)?;
        self.status.clear();
        self.files.watch(&self.docs)
    }

    pub(super) fn click_folder(&mut self, path: PathBuf) -> io::Result<()> {
        let now = Instant::now();
        if self
            .last_folder_click
            .as_ref()
            .is_some_and(|(previous, at)| {
                previous == &path && now.duration_since(*at) <= Duration::from_millis(350)
            })
        {
            return self.enter_directory(&path);
        }
        self.files.toggle(&path)?;
        self.last_folder_click = Some((path, now));
        self.files.watch(&self.docs)
    }

    pub(super) fn tree_menu(&mut self, path: PathBuf, dir: bool, pos: Position) {
        self.last_folder_click = None;
        self.tree_dialog = Some(TreeDialog {
            path,
            dir,
            mode: TreeMode::Menu(pos),
            error: String::new(),
        });
    }

    pub(super) fn tree_dialog_input(&mut self, ev: Event) -> io::Result<()> {
        let Some(dialog) = self.tree_dialog.as_mut() else {
            return Ok(());
        };
        if matches!(dialog.mode, TreeMode::Deleting) {
            return Ok(());
        }
        let key = match &ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => Some(*k),
            _ => None,
        };
        let click = match ev {
            Event::Mouse(m) if m.kind == MouseEventKind::Down(MouseButton::Left) => {
                Some(Position::new(m.column, m.row))
            }
            _ => None,
        };
        let primary = click.is_some_and(|p| self.geometry.tree_primary.contains(p));
        let secondary = click.is_some_and(|p| self.geometry.tree_secondary.contains(p));
        let code = key.map(|k| k.code);
        if code == Some(KeyCode::Esc) || (secondary && !matches!(dialog.mode, TreeMode::Menu(_))) {
            self.tree_dialog = None;
            return Ok(());
        }
        match &mut dialog.mode {
            TreeMode::Menu(_) => {
                if primary || code == Some(KeyCode::Char('r')) {
                    let name = dialog
                        .path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned();
                    let mut input = TextArea::new(vec![name]);
                    input.select_all();
                    dialog.mode = TreeMode::Rename(Box::new(input));
                } else if secondary || code == Some(KeyCode::Char('d')) {
                    dialog.mode = TreeMode::Delete;
                } else if click.is_some() {
                    self.tree_dialog = None;
                }
            }
            TreeMode::Rename(input) => {
                if primary || code == Some(KeyCode::Enter) {
                    let path = dialog.path.clone();
                    let name = input.lines()[0].clone();
                    match rename_entry(&path, &name) {
                        Ok(dest) => {
                            self.tree_dialog = None;
                            for tab in &mut self.docs {
                                tab.relocate(&path, &dest);
                            }
                            self.files.entry_changed(&path)?;
                            self.files.watch(&self.docs)?;
                            self.status.clear();
                        }
                        Err(err) => dialog.error = err.to_string(),
                    }
                } else if let Event::Paste(text) = ev {
                    if !text.chars().any(char::is_control) {
                        input.insert_str(text);
                    }
                } else if let Some(k) = key {
                    dialog.error.clear();
                    match k.code {
                        KeyCode::Char('a') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                            input.select_all()
                        }
                        KeyCode::Backspace
                        | KeyCode::Delete
                        | KeyCode::Left
                        | KeyCode::Right
                        | KeyCode::Home
                        | KeyCode::End => {
                            input.input(k);
                        }
                        KeyCode::Char(ch)
                            if !ch.is_control() && !k.modifiers.contains(KeyModifiers::CONTROL) =>
                        {
                            input.input(k);
                        }
                        _ => {}
                    }
                }
            }
            TreeMode::Delete if primary || code == Some(KeyCode::Enter) => {
                let path = dialog.path.clone();
                let dir = dialog.dir;
                let tx = self.tx.clone();
                thread::spawn(move || {
                    let result = if dir {
                        fs::remove_dir_all(&path)
                    } else {
                        fs::remove_file(&path)
                    };
                    let _ = tx.send(Message::Deleted(path, result));
                });
                dialog.mode = TreeMode::Deleting;
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn finish_delete(
        &mut self,
        path: PathBuf,
        result: io::Result<()>,
    ) -> io::Result<()> {
        if let Err(err) = result {
            if let Some(dialog) = &mut self.tree_dialog {
                dialog.mode = TreeMode::Delete;
                dialog.error = err.to_string();
            }
        } else {
            self.tree_dialog = None;
            // Keep modified buffers, including those inside a removed directory.
            for i in (0..self.docs.len()).rev() {
                if !self.docs[i].path().starts_with(&path) {
                    continue;
                }
                if self.docs[i].dirty() {
                    if let Some(doc) = self.docs[i].doc_mut() {
                        doc.external = true;
                    }
                } else {
                    self.docs.remove(i);
                    if i < self.active {
                        self.active -= 1;
                    }
                }
            }
            self.active = self.active.min(self.docs.len().saturating_sub(1));
            self.status.clear();
        }
        self.files.entry_changed(&path)?;
        self.files.watch(&self.docs)
    }
}

fn rename_entry(path: &Path, name: &str) -> io::Result<PathBuf> {
    let component = Path::new(name);
    if !matches!(component.components().next(), Some(Component::Normal(_)))
        || component.file_name() != Some(component.as_os_str())
        || name.chars().any(char::is_control)
    {
        return Err(io::Error::other("Enter a single file or folder name"));
    }
    let destination = path.with_file_name(name);
    if destination != path {
        crate::platform::rename_without_replace(path, &destination)?;
    }
    Ok(destination)
}

//! Workspace-hosted editor. Input and filesystem changes wake a single UI loop.
mod bitmap;
mod document;
mod explorer;
mod files;
mod highlight;
mod preview;
mod search;
mod selection;
mod syntax;
mod view;

use std::{
    collections::HashSet,
    io::{self, Write},
    path::PathBuf,
    sync::{mpsc, Arc, Mutex},
    thread,
};

use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
};
use document::Document;
use explorer::TreeDialog;
use files::Files;
use notify::{
    event::{AccessKind, ModifyKind},
    EventKind,
};
use preview::{FileTab, Format};
use ratatui::{layout::Rect, style::Color};
use search::{Hit, Job, SearchKind};

pub const ENTRY: &str = "--internal-workspace-editor";
const FG: Color = Color::Rgb(204, 204, 204);
const BG: Color = Color::Rgb(31, 31, 31);

enum Message {
    Input(io::Result<Event>),
    Disk,
    Deleted(PathBuf, io::Result<()>),
    Results(u64, Vec<Hit>),
    Preview(u64, io::Result<preview::Rendered>),
}

#[derive(Default)]
struct Changes {
    paths: HashSet<PathBuf>,
    dirs: HashSet<PathBuf>,
    all: bool,
    pending: bool,
}

impl Changes {
    fn record(&mut self, event: notify::Result<notify::Event>) -> bool {
        let Ok(event) = event else {
            self.all = true;
            return true;
        };
        if event.need_rescan() {
            self.all = true;
            return true;
        }
        if matches!(event.kind, EventKind::Access(access) if access!=AccessKind::Close(notify::event::AccessMode::Write))
        {
            return false;
        }
        let structural = matches!(
            event.kind,
            EventKind::Create(_)
                | EventKind::Remove(_)
                | EventKind::Modify(ModifyKind::Name(_))
                | EventKind::Any
        );
        for path in event.paths {
            if structural {
                if let Some(parent) = path.parent() {
                    self.dirs.insert(parent.to_owned());
                }
            }
            self.paths.insert(path);
        }
        true
    }
}

struct Search {
    kind: SearchKind,
    query: String,
    hits: Vec<Hit>,
    selected: usize,
    job: Option<Job>,
}
#[derive(Clone, Copy)]
struct CloseMenu {
    index: usize,
    x: u16,
}

struct Compare {
    disk: Option<String>,
    row: usize,
}
#[derive(Default)]
struct Geometry {
    editor: Rect,
    tree: Rect,
    tabs: Vec<(Rect, usize)>,
    close_icons: Vec<(Rect, usize)>,
    close_action: Rect,
    parent_directory: Rect,
    search_panel: Rect,
    search_close: Rect,
    tree_primary: Rect,
    tree_secondary: Rect,
    search: Rect,
    files: Rect,
    save: Rect,
    toggle_preview: Rect,
    previous_page: Rect,
    next_page: Rect,
    compare_buttons: Vec<(Rect, KeyCode)>,
    picker: Rect,
    narrow: bool,
}

struct Editor {
    files: Files,
    docs: Vec<FileTab>,
    rendering: bool,
    render_id: u64,
    active: usize,
    tree: bool,
    tree_width: Option<u16>,
    resizing: bool,
    last_folder_click: Option<(PathBuf, std::time::Instant)>,
    tree_dialog: Option<TreeDialog>,
    geometry: Geometry,
    search: Option<Search>,
    serial: u64,
    compare: Option<Compare>,
    close_menu: Option<CloseMenu>,
    status: String,
    tx: mpsc::Sender<Message>,
    changes: Arc<Mutex<Changes>>,
    clipboard: String,
    copy_on_select: bool,
    selecting_text: bool,
}

pub fn run() -> io::Result<()> {
    let (tx, rx) = mpsc::channel();
    let changes = Arc::new(Mutex::new(Changes::default()));
    let pending = changes.clone();
    let wake = tx.clone();
    let watcher = notify::recommended_watcher(move |event| {
        let Ok(mut changes) = pending.lock() else {
            return;
        };
        if changes.record(event) && !changes.pending {
            changes.pending = true;
            let _ = wake.send(Message::Disk);
        }
    })
    .map_err(io::Error::other)?;
    let mut files = Files::new(std::env::current_dir()?.canonicalize()?, watcher)?;
    files.watch(&[])?;
    let mut editor = Editor {
        files,
        docs: vec![],
        rendering: false,
        render_id: 0,
        active: 0,
        tree: crossterm::terminal::size()?.0 >= 80,
        tree_width: None,
        resizing: false,
        last_folder_click: None,
        tree_dialog: None,
        geometry: Geometry::default(),
        search: None,
        serial: 0,
        compare: None,
        close_menu: None,
        status: String::new(),
        tx: tx.clone(),
        changes,
        clipboard: String::new(),
        copy_on_select: crate::config::Config::load().config.ui.copy_on_select,
        selecting_text: false,
    };
    // Ask the owning server to forward ordinary right-clicks to this editor pane.
    if let Ok(pane_id) = std::env::var(crate::integration::HERDR_PANE_ID_ENV_VAR) {
        use crate::api::schema::{Method, PaneInputSetParams, PaneRightClickTarget, Request};
        if let Err(err) = crate::api::client::ApiClient::local().request(Request {
            id: "editor:right-click".into(),
            method: Method::PaneInputSet(PaneInputSetParams {
                pane_id,
                right_click: PaneRightClickTarget::Pane,
            }),
        }) {
            editor.status = format!("File menu unavailable ({err})");
        }
    }
    let mut terminal = ratatui::try_init()?;
    let mut display = bitmap::Display::default();
    let mut cell = bitmap::CellPixels::read();
    let result = (|| {
        execute!(
            io::stdout(),
            // Button and drag events suffice. Hover traffic would cross SSH on every mouse move.
            crossterm::style::Print("\x1b[?1002h\x1b[?1006h"),
            crossterm::cursor::SetCursorStyle::SteadyBar,
            event::EnableBracketedPaste
        )?;
        thread::spawn(move || loop {
            let ev = event::read();
            if matches!(
                ev,
                Ok(Event::Mouse(event::MouseEvent {
                    kind: event::MouseEventKind::Moved,
                    ..
                }))
            ) {
                continue;
            }
            let done = ev.is_err();
            if tx.send(Message::Input(ev)).is_err() || done {
                break;
            }
        });
        loop {
            let size = terminal.size()?;
            let (_, area, _, _) = editor.layout(Rect::new(0, 0, size.width, size.height));
            editor.prepare_view(area, cell);
            let mut geometry = Geometry::default();
            terminal.draw(|f| geometry = editor.draw(f))?;
            let unobstructed =
                editor.search.is_none() && editor.close_menu.is_none() && editor.compare.is_none();
            let image = editor
                .docs
                .get(editor.active)
                .and_then(FileTab::preview)
                .filter(|_| unobstructed)
                .and_then(preview::Preview::bitmap);
            display.sync(&mut io::stdout(), image, geometry.editor, cell)?;
            editor.geometry = geometry;
            let msg = if editor
                .docs
                .get(editor.active)
                .filter(|tab| tab.preview().is_none())
                .and_then(FileTab::doc)
                .is_some_and(|d| d.colors.pending)
            {
                match rx.try_recv() {
                    Ok(msg) => msg,
                    Err(mpsc::TryRecvError::Empty) => continue,
                    Err(e) => return Err(io::Error::other(e)),
                }
            } else {
                rx.recv().map_err(io::Error::other)?
            };
            match msg {
                Message::Input(Ok(Event::Resize(..))) => cell = bitmap::CellPixels::read(),
                Message::Input(ev) => match editor.input(ev?) {
                    Ok(true) => break,
                    Err(e) => editor.status = e.to_string(),
                    _ => {}
                },
                Message::Preview(id, result) => {
                    editor.rendering = false;
                    if let Some(view) = editor
                        .docs
                        .iter_mut()
                        .filter_map(FileTab::preview_mut)
                        .find(|v| v.matches_request(id))
                    {
                        view.accept(id, result);
                    }
                }
                Message::Deleted(path, result) => {
                    if let Err(err) = editor.finish_delete(path, result) {
                        editor.status = err.to_string();
                    }
                }
                Message::Disk => {
                    if let Err(e) = editor.refresh() {
                        editor.status = e.to_string();
                    }
                }
                Message::Results(id, hits) => {
                    if id == editor.serial {
                        if let Some(s) = &mut editor.search {
                            s.hits = hits;
                            s.job = None;
                        }
                    }
                }
            }
        }
        Ok(())
    })();
    let _ = display.sync(&mut io::stdout(), None, Rect::default(), cell);
    let _ = execute!(
        io::stdout(),
        crossterm::cursor::SetCursorStyle::DefaultUserShape,
        event::DisableBracketedPaste,
        event::DisableMouseCapture
    );
    ratatui::restore();
    result
}

impl Editor {
    fn prepare_view(&mut self, area: Rect, cell: bitmap::CellPixels) {
        let Some(tab) = self.docs.get_mut(self.active) else {
            return;
        };
        if let Some(preview) = tab.preview() {
            if self.rendering || !preview.needs_render(area, cell) {
                return;
            }
            let source = tab.doc().map(|d| d.area.lines().join("\n"));
            let path = tab.path().to_owned();
            if let Some(preview) = tab.preview_mut() {
                self.render_id += 1;
                self.rendering = true;
                preview.start(self.render_id, path, source, area, cell, self.tx.clone());
            }
        } else if let Some(doc) = tab.doc_mut() {
            syntax::prepare(area, doc);
        }
    }

    fn toggle_preview(&mut self) {
        let Some(tab) = self.docs.get_mut(self.active) else {
            return;
        };
        if tab.markdown() {
            tab.toggle_markdown();
        } else if let Some(p) = tab.preview_mut() {
            p.toggle_pdf();
        }
    }

    fn refresh(&mut self) -> io::Result<()> {
        let changes = std::mem::take(
            &mut *self
                .changes
                .lock()
                .map_err(|_| io::Error::other("File watcher stopped"))?,
        );
        for doc in &mut self.docs {
            if changes.all || changes.paths.contains(doc.path()) {
                if let Err(e) = doc.refresh() {
                    self.status = e.to_string();
                }
            }
        }
        self.files.refresh(&changes.dirs, changes.all)
    }

    fn open(&mut self, path: PathBuf, row: Option<usize>) -> io::Result<()> {
        let path = path.canonicalize()?;
        if let Some(i) = self.docs.iter().position(|d| d.path() == path) {
            self.active = i;
        } else {
            let doc = FileTab::open(path)?;
            self.docs.push(doc);
            self.active = self.docs.len() - 1;
            self.files.watch(&self.docs)?;
            // Close the read-before-watch window for files opened through search.
            self.docs[self.active].refresh()?;
        }
        if let Some(row) = row {
            self.docs[self.active].show_source();
            if let Some(doc) = self.docs[self.active].doc_mut() {
                doc.jump(row, 0);
            }
        }
        self.status.clear();
        if self.geometry.narrow {
            self.tree = false;
        }
        Ok(())
    }

    fn begin(&mut self, kind: SearchKind) {
        self.search = Some(Search {
            kind,
            query: String::new(),
            hits: vec![],
            selected: 0,
            job: None,
        });
        self.query_changed();
    }

    fn dismiss_search(&mut self) {
        self.search = None;
        if let Some(doc) = self.docs.get_mut(self.active).and_then(FileTab::doc_mut) {
            let _ = doc.area.set_search_pattern("");
        }
    }

    fn query_changed(&mut self) {
        let Some(s) = &mut self.search else { return };
        self.serial += 1;
        s.job = None;
        s.selected = 0;
        s.hits.clear();
        if s.kind == SearchKind::Find {
            if let Some(d) = self.docs.get_mut(self.active).and_then(FileTab::doc_mut) {
                let _ = d.area.set_search_pattern(regex::escape(&s.query));
                if !s.query.is_empty() {
                    d.area.search_forward(true);
                }
            }
        } else if s.kind == SearchKind::Files || !s.query.is_empty() {
            s.job = Some(search::start(
                self.files.root.clone(),
                s.query.clone(),
                s.kind,
                self.serial,
                self.tx.clone(),
            ));
        }
    }

    fn choose(&mut self) -> io::Result<()> {
        let Some(s) = &mut self.search else {
            return Ok(());
        };
        if s.kind == SearchKind::Find {
            if let Some(d) = self.docs.get_mut(self.active).and_then(FileTab::doc_mut) {
                d.area.search_forward(false);
            }
        } else if let Some(h) = s.hits.get(s.selected) {
            let path = h.path.clone();
            let row = (s.kind == SearchKind::Workspace).then_some(h.row);
            self.open(path, row)?;
            self.search = None;
        }
        Ok(())
    }

    fn save(&mut self) -> io::Result<()> {
        let Some(tab) = self.docs.get_mut(self.active) else {
            return Ok(());
        };
        let Some(d) = tab.doc_mut() else {
            return Ok(());
        };
        let disk = d.disk()?;
        if disk.as_deref() != Some(d.baseline()) {
            if !d.dirty && disk.is_some() {
                tab.refresh()?;
                return Ok(());
            }
            let row = d
                .area
                .lines()
                .iter()
                .map(String::as_str)
                .zip(disk.as_deref().unwrap_or("").lines())
                .position(|(a, b)| a != b)
                .unwrap_or_else(|| d.area.cursor().0);
            d.external = true;
            self.status.clear();
            self.compare = Some(Compare { disk, row });
        } else if d.dirty {
            d.save(disk.as_deref())?;
            self.status = "Saved".into();
        }
        Ok(())
    }

    fn request_close(&mut self, index: usize, x: u16) -> io::Result<()> {
        if self.docs[index].dirty() {
            self.close_menu = Some(CloseMenu { index, x });
            Ok(())
        } else {
            self.close_file(index)
        }
    }

    fn close_file(&mut self, index: usize) -> io::Result<()> {
        self.docs.remove(index);
        if index < self.active {
            self.active -= 1;
        }
        self.active = self.active.min(self.docs.len().saturating_sub(1));
        self.close_menu = None;
        self.status.clear();
        self.files.watch(&self.docs)
    }

    fn input(&mut self, ev: Event) -> io::Result<bool> {
        if !matches!(
            ev,
            Event::Mouse(event::MouseEvent {
                kind: event::MouseEventKind::Drag(event::MouseButton::Left)
                    | event::MouseEventKind::Up(event::MouseButton::Left),
                ..
            })
        ) {
            self.selecting_text = false;
        }
        if let Event::Resize(w, _) = ev {
            if w < 80 && !self.geometry.narrow {
                self.tree = false;
            }
        }
        if self.tree_dialog.is_some() {
            self.tree_dialog_input(ev)?;
            return Ok(false);
        }
        if let Some(menu) = self.close_menu {
            let activate = match ev {
                Event::Mouse(m)
                    if m.kind == event::MouseEventKind::Down(event::MouseButton::Left) =>
                {
                    self.geometry
                        .close_action
                        .contains((m.column, m.row).into())
                }
                Event::Key(k) if k.kind != KeyEventKind::Release => match k.code {
                    KeyCode::Char('d' | 'D') => true,
                    KeyCode::Esc => false,
                    _ => return Ok(false),
                },
                _ => return Ok(false),
            };
            self.close_menu = None;
            if activate {
                self.close_file(menu.index)?;
            }
            return Ok(false);
        }
        if let Some(c) = &mut self.compare {
            let code = match ev {
                Event::Key(key) if key.kind != KeyEventKind::Release => Some(key.code),
                Event::Mouse(m) if m.kind == event::MouseEventKind::ScrollDown => {
                    Some(KeyCode::Down)
                }
                Event::Mouse(m) if m.kind == event::MouseEventKind::ScrollUp => Some(KeyCode::Up),
                Event::Mouse(m)
                    if m.kind == event::MouseEventKind::Down(event::MouseButton::Left) =>
                {
                    self.geometry
                        .compare_buttons
                        .iter()
                        .find(|(r, _)| r.contains((m.column, m.row).into()))
                        .map(|(_, code)| *code)
                }
                _ => None,
            };
            let Some(code) = code else { return Ok(false) };
            let Some(doc) = self.docs.get_mut(self.active).and_then(FileTab::doc_mut) else {
                return Ok(false);
            };
            match code {
                KeyCode::Esc => self.compare = None,
                KeyCode::Up => c.row = c.row.saturating_sub(1),
                KeyCode::Down => c.row = c.row.saturating_add(1),
                KeyCode::Char('w' | 'W') => {
                    doc.save(c.disk.as_deref())?;
                    self.compare = None;
                    self.status = "Saved".into();
                }
                KeyCode::Char('r' | 'R') => {
                    if let Some(text) = doc.disk()? {
                        doc.reload(text);
                        if let Some(preview) = self.docs[self.active].preview_mut() {
                            preview.invalidate();
                        }
                        self.compare = None;
                    } else {
                        self.status = "File was removed".into();
                    }
                }
                _ => {}
            }
            return Ok(false);
        }
        match ev {
            Event::Key(key) if key.kind != KeyEventKind::Release => return self.key(key),
            Event::Mouse(mouse) => self.mouse(mouse)?,
            Event::Paste(text) => {
                if let Some(s) = &mut self.search {
                    s.query.push_str(&text);
                    self.query_changed();
                } else if let Some(d) = self
                    .docs
                    .get_mut(self.active)
                    .filter(|t| t.preview().is_none())
                    .and_then(FileTab::doc_mut)
                {
                    let from = d
                        .area
                        .selection_range()
                        .map_or(d.area.cursor().0, |r| r.0 .0);
                    if d.area.insert_str(text) {
                        d.dirty = true;
                        d.colors.invalidate(from);
                    }
                }
            }
            _ => {}
        }
        Ok(false)
    }

    fn key(&mut self, key: KeyEvent) -> io::Result<bool> {
        if let Some(s) = &mut self.search {
            match key.code {
                KeyCode::Esc => self.dismiss_search(),
                KeyCode::Enter => self.choose()?,
                KeyCode::Up => s.selected = s.selected.saturating_sub(1),
                KeyCode::Down => s.selected = (s.selected + 1).min(s.hits.len().saturating_sub(1)),
                KeyCode::Backspace => {
                    s.query.pop();
                    self.query_changed();
                }
                KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    s.query.push(ch);
                    self.query_changed();
                }
                _ => {}
            }
            return Ok(false);
        }
        if key.code == KeyCode::F(3) {
            self.toggle_preview();
            return Ok(false);
        }
        if !key.modifiers.contains(KeyModifiers::CONTROL) {
            if let Some(view) = self
                .docs
                .get_mut(self.active)
                .and_then(FileTab::preview_mut)
            {
                let h = self.geometry.editor.height;
                match key.code {
                    KeyCode::Left => view.set_page(-1),
                    KeyCode::Right => view.set_page(1),
                    KeyCode::Up => view.scroll(-1, h),
                    KeyCode::Down => view.scroll(1, h),
                    KeyCode::PageUp => view.scroll(-isize::try_from(h).unwrap_or(isize::MAX), h),
                    KeyCode::PageDown => view.scroll(isize::try_from(h).unwrap_or(isize::MAX), h),
                    KeyCode::F(2) => self.save()?,
                    _ => {}
                }
                return Ok(false);
            }
        }
        if key.code == KeyCode::F(2) {
            self.save()?;
            return Ok(false);
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl {
            match key.code {
                KeyCode::Char('s') => self.save()?,
                KeyCode::Char('p') => self.begin(SearchKind::Files),
                KeyCode::Char('F') => self.begin(SearchKind::Workspace),
                KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    self.begin(SearchKind::Workspace)
                }
                KeyCode::Char('f') => {
                    if let Some(tab) = self.docs.get_mut(self.active).filter(|t| t.doc().is_some())
                    {
                        tab.show_source();
                        self.begin(SearchKind::Find);
                    }
                }
                KeyCode::Char('b') => self.tree = !self.tree,
                KeyCode::Char('c') => self.copy_selection()?,
                KeyCode::Char('x') => {
                    if self
                        .docs
                        .get(self.active)
                        .is_some_and(|tab| tab.preview().is_none())
                    {
                        self.copy_selection()?;
                        self.edit(key)?;
                    }
                }
                KeyCode::Char('w') if !self.docs.is_empty() => {
                    self.request_close(self.active, 0)?
                }
                KeyCode::Char('q') => {
                    if self.docs.iter().all(|d| !d.dirty()) {
                        return Ok(true);
                    }
                    self.status = "Save edited files before quitting".into();
                }
                _ => self.edit(key)?,
            }
        } else {
            self.edit(key)?;
        }
        Ok(false)
    }

    fn edit(&mut self, key: KeyEvent) -> io::Result<()> {
        let Some(d) = self
            .docs
            .get_mut(self.active)
            .filter(|t| t.preview().is_none())
            .and_then(FileTab::doc_mut)
        else {
            return Ok(());
        };
        let from = d
            .area
            .selection_range()
            .map_or(d.area.cursor().0, |r| r.0 .0);
        if matches!(key.code, KeyCode::PageUp | KeyCode::PageDown) {
            let page = usize::from(self.geometry.editor.height.max(1));
            let (row, col) = d.area.cursor();
            d.jump(
                if key.code == KeyCode::PageUp {
                    row.saturating_sub(page)
                } else {
                    row.saturating_add(page)
                },
                col,
            );
            return Ok(());
        }
        let modified = if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('a') => {
                    d.area.select_all();
                    false
                }
                KeyCode::Char('x') => d.area.cut(),
                KeyCode::Char('v') => d.area.insert_str(&self.clipboard),
                KeyCode::Char('z') => d.area.undo(),
                KeyCode::Char('y' | 'Z') => d.area.redo(),
                _ => d.area.input(key),
            }
        } else {
            d.area.input(key)
        };
        if modified {
            d.dirty = true;
            d.colors.invalidate(from.min(d.area.cursor().0));
            self.status.clear();
        }
        Ok(())
    }
}

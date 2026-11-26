//! Workspace-host previews. Only terminal-sized pixels or styled lines reach the pane.
use image::{metadata::Orientation, DynamicImage, ImageDecoder, ImageReader, Limits};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::Paragraph,
    Frame,
};
use std::{
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::Stdio,
    thread,
    time::{Duration, Instant},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::{
    bitmap::{Bitmap, CellPixels},
    Document,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Markdown,
    Image,
    Pdf,
}

fn format(path: &Path) -> Option<Format> {
    let ext = path.extension()?.to_str()?;
    let matches = |extensions: &[&str]| extensions.iter().any(|e| ext.eq_ignore_ascii_case(e));
    if matches(&["md", "markdown", "mdown"]) {
        Some(Format::Markdown)
    } else if matches(&["png", "jpg", "jpeg", "webp", "gif", "bmp"]) {
        Some(Format::Image)
    } else if ext.eq_ignore_ascii_case("pdf") {
        Some(Format::Pdf)
    } else {
        None
    }
}

pub enum FileTab {
    Source {
        doc: Document,
        preview: Option<Preview>,
    },
    Media {
        path: PathBuf,
        preview: Preview,
    },
}

impl FileTab {
    pub fn open(path: PathBuf) -> io::Result<Self> {
        let kind = format(&path);
        if let Some(kind @ (Format::Pdf | Format::Image)) = kind {
            if !path.metadata()?.is_file() {
                return Err(io::Error::other("Choose a regular file"));
            }
            return Ok(Self::Media {
                path,
                preview: Preview::new(kind),
            });
        }
        Ok(Self::Source {
            doc: Document::open(path)?,
            preview: kind.map(Preview::new),
        })
    }
    pub fn path(&self) -> &Path {
        match self {
            Self::Source { doc, .. } => &doc.path,
            Self::Media { path, .. } => path,
        }
    }
    pub fn relocate(&mut self, old: &Path, new: &Path) {
        let Ok(relative) = self.path().strip_prefix(old) else {
            return;
        };
        let next = if relative.as_os_str().is_empty() {
            new.to_owned()
        } else {
            new.join(relative)
        };
        match self {
            Self::Source { doc, preview } => {
                if doc.path.extension() != next.extension() {
                    doc.colors = Default::default();
                    *preview = (format(&next) == Some(Format::Markdown))
                        .then(|| Preview::new(Format::Markdown));
                }
                doc.path = next;
            }
            Self::Media { path, preview } => {
                *path = next;
                if let Some(kind @ (Format::Image | Format::Pdf)) = format(path) {
                    if preview.kind != kind {
                        *preview = Preview::new(kind);
                    }
                }
                preview.invalidate();
            }
        }
    }
    pub fn doc(&self) -> Option<&Document> {
        match self {
            Self::Source { doc, .. } => Some(doc),
            _ => None,
        }
    }
    pub fn doc_mut(&mut self) -> Option<&mut Document> {
        match self {
            Self::Source { doc, .. } => Some(doc),
            _ => None,
        }
    }
    pub fn preview(&self) -> Option<&Preview> {
        match self {
            Self::Source { preview, .. } => preview.as_ref(),
            Self::Media { preview, .. } => Some(preview),
        }
    }
    pub fn preview_mut(&mut self) -> Option<&mut Preview> {
        match self {
            Self::Source { preview, .. } => preview.as_mut(),
            Self::Media { preview, .. } => Some(preview),
        }
    }
    pub fn markdown(&self) -> bool {
        format(self.path()) == Some(Format::Markdown)
    }
    pub fn toggle_markdown(&mut self) {
        if let Self::Source { doc, preview } = self {
            if format(&doc.path) == Some(Format::Markdown) {
                *preview = if preview.is_some() {
                    None
                } else {
                    Some(Preview::new(Format::Markdown))
                };
            }
        }
    }
    pub fn show_source(&mut self) {
        if let Self::Source { preview, .. } = self {
            *preview = None;
        }
    }
    pub fn dirty(&self) -> bool {
        self.doc().is_some_and(|d| d.dirty)
    }
    pub fn refresh(&mut self) -> io::Result<()> {
        if let Some(doc) = self.doc_mut() {
            doc.refresh()?;
        }
        if let Some(preview) = self.preview_mut() {
            preview.invalidate();
            preview.pages = None;
        }
        Ok(())
    }
}

enum Content {
    Lines(Vec<Line<'static>>),
    Pixels(Bitmap),
}
pub struct Rendered {
    content: Content,
    pages: Option<u32>,
    page: u32,
}
pub struct Preview {
    pub kind: Format,
    pub page: u32,
    pub pages: Option<u32>,
    pub text_mode: bool,
    scroll: usize,
    // Absolute wrapped row and display column, in the current text preview.
    selection: Option<std::ops::Range<(usize, usize)>>,
    request: Option<(u64, u16, u16, CellPixels)>,
    rendered: Option<Result<Content, String>>,
}

impl Preview {
    fn new(kind: Format) -> Self {
        Self {
            kind,
            page: 1,
            pages: None,
            text_mode: false,
            scroll: 0,
            selection: None,
            request: None,
            rendered: None,
        }
    }
    pub fn invalidate(&mut self) {
        self.request = None;
        self.rendered = None;
        self.selection = None;
    }
    pub fn set_page(&mut self, delta: i32) {
        let next = self
            .page
            .saturating_add_signed(delta)
            .clamp(1, self.pages.unwrap_or(1));
        if next != self.page {
            self.page = next;
            self.scroll = 0;
            self.invalidate();
        }
    }
    pub fn toggle_pdf(&mut self) {
        if self.kind == Format::Pdf {
            self.text_mode = !self.text_mode;
            self.scroll = 0;
            self.invalidate();
        }
    }
    pub fn scroll(&mut self, delta: isize, height: u16) {
        if let Some(Ok(Content::Lines(lines))) = &self.rendered {
            self.scroll = self
                .scroll
                .saturating_add_signed(delta)
                .min(lines.len().saturating_sub(usize::from(height)));
        } else if self.kind == Format::Pdf {
            self.set_page(delta.signum() as i32);
        }
    }
    pub fn needs_render(&self, r: Rect, cell: CellPixels) -> bool {
        !r.is_empty()
            && self
                .request
                .is_none_or(|(_, w, h, old_cell)| w != r.width || h != r.height || old_cell != cell)
    }
    pub fn matches_request(&self, id: u64) -> bool {
        self.request.is_some_and(|(req, _, _, _)| req == id)
    }
    pub fn accept(&mut self, id: u64, result: io::Result<Rendered>) {
        if let Some((req, _, height, _)) = self.request {
            if req != id {
                return;
            }
            self.rendered = Some(
                result
                    .map(|r| {
                        self.pages = r.pages;
                        self.page = r.page;
                        if let Content::Lines(lines) = &r.content {
                            self.scroll = self
                                .scroll
                                .min(lines.len().saturating_sub(usize::from(height)));
                        }
                        r.content
                    })
                    .map_err(|e| e.to_string()),
            );
        }
    }
    pub fn start(
        &mut self,
        id: u64,
        path: PathBuf,
        source: Option<String>,
        r: Rect,
        cell: CellPixels,
        tx: std::sync::mpsc::Sender<super::Message>,
    ) {
        self.request = Some((id, r.width, r.height, cell));
        self.rendered = None;
        self.selection = None;
        let (kind, page, pages, text_mode) = (self.kind, self.page, self.pages, self.text_mode);
        thread::spawn(move || {
            let result = match kind {
                Format::Markdown => Ok(Rendered {
                    content: Content::Lines(markdown(source.as_deref().unwrap_or(""), r.width)),
                    pages: None,
                    page: 1,
                }),
                Format::Image => load_image(&path, r, cell).map(|pixels| Rendered {
                    content: Content::Pixels(pixels),
                    pages: None,
                    page: 1,
                }),
                Format::Pdf => render_pdf(&path, page, pages, text_mode, r, cell),
            };
            let _ = tx.send(super::Message::Preview(id, result));
        });
    }
    pub fn bitmap(&self) -> Option<(u64, &Bitmap)> {
        if let Some(Ok(Content::Pixels(img))) = &self.rendered {
            Some((self.request?.0, img))
        } else {
            None
        }
    }
    pub fn select_at(&mut self, row: usize, col: usize, begin: bool) -> bool {
        let Some(Ok(Content::Lines(lines))) = &self.rendered else {
            return false;
        };
        let Some(last) = lines.len().checked_sub(1) else {
            return false;
        };
        let row = (self.scroll + row).min(last);
        let mut cell = 0;
        for g in lines[row].styled_graphemes(Style::default()) {
            let next = cell + g.symbol.width();
            if next > col {
                break;
            }
            cell = next;
        }
        let point = (row, cell);
        if begin {
            self.selection = Some(point..point);
        } else if let Some(range) = &mut self.selection {
            range.end = point;
        }
        true
    }

    fn selected_range(&self) -> Option<std::ops::Range<(usize, usize)>> {
        let r = self.selection.as_ref()?;
        (r.start != r.end).then(|| r.start.min(r.end)..r.start.max(r.end))
    }

    pub fn selected_text(&self) -> Option<String> {
        let r = self.selected_range()?;
        let Some(Ok(Content::Lines(lines))) = &self.rendered else {
            return None;
        };
        let mut text = String::new();
        for row in r.start.0..=r.end.0 {
            if row > r.start.0 {
                text.push('\n');
            }
            let mut col = 0;
            for g in lines[row].styled_graphemes(Style::default()) {
                if (row, col) >= r.end {
                    break;
                }
                if (row, col) >= r.start {
                    text.push_str(g.symbol);
                }
                col += g.symbol.width();
            }
        }
        Some(text)
    }

    pub fn draw(&self, f: &mut Frame<'_>, r: Rect) {
        match &self.rendered {
            None => f.render_widget(Paragraph::new("Loading preview…"), r),
            Some(Err(err)) => f.render_widget(
                Paragraph::new(err.as_str()).wrap(ratatui::widgets::Wrap { trim: false }),
                r,
            ),
            Some(Ok(Content::Lines(lines))) => {
                let selected = self.selected_range();
                for (line, y) in lines.iter().skip(self.scroll).zip(r.y..r.bottom()) {
                    f.render_widget(line, Rect::new(r.x, y, r.width, 1));
                    let row = self.scroll + usize::from(y - r.y);
                    if let Some(range) = selected
                        .as_ref()
                        .filter(|s| (s.start.0..=s.end.0).contains(&row))
                    {
                        let first = if row == range.start.0 {
                            range.start.1
                        } else {
                            0
                        };
                        let last = if row == range.end.0 {
                            range.end.1
                        } else {
                            line.width()
                        };
                        let first = first.min(usize::from(r.width)) as u16;
                        let last = last.min(usize::from(r.width)) as u16;
                        f.buffer_mut().set_style(
                            Rect::new(r.x + first, y, last.saturating_sub(first), 1),
                            Style::new().bg(Color::Rgb(38, 79, 120)),
                        );
                    }
                }
            }
            Some(Ok(Content::Pixels(_))) => {}
        }
    }
}

#[derive(Clone)]
struct MarkdownStyle;
impl tui_markdown::StyleSheet for MarkdownStyle {
    fn heading(&self, level: u8) -> Style {
        let color = if level <= 2 {
            Color::White
        } else {
            Color::Rgb(156, 220, 254)
        };
        Style::new().fg(color).add_modifier(Modifier::BOLD)
    }
    fn code(&self) -> Style {
        Style::new()
            .fg(Color::Rgb(206, 145, 120))
            .bg(Color::Rgb(40, 40, 40))
    }
    fn link(&self) -> Style {
        Style::new()
            .fg(Color::Rgb(117, 190, 255))
            .add_modifier(Modifier::UNDERLINED)
    }
    fn blockquote(&self) -> Style {
        Style::new().fg(Color::Rgb(157, 157, 157))
    }
    fn heading_marker(&self, _level: u8) -> &str {
        ""
    }
    fn code_block_fence(&self) -> &str {
        ""
    }
}

fn markdown(source: &str, width: u16) -> Vec<Line<'static>> {
    let options = tui_markdown::Options::new(MarkdownStyle)
        .table_width(width)
        .table_row_dividers(true);
    wrap(tui_markdown::from_str_with_options(source, &options), width)
}

// Wrap once on the worker, retaining emphasis and Unicode display widths.
fn wrap(text: Text<'_>, width: u16) -> Vec<Line<'static>> {
    let width = usize::from(width.max(1));
    let mut output = Vec::new();
    for line in text.lines {
        let mut spans = Vec::new();
        let mut column = 0;
        for span in line.spans {
            let style = text.style.patch(line.style).patch(span.style);
            let mut chunk = String::new();
            for word in span.content.split_word_bounds() {
                let word_width = word.width();
                if column > 0 && word_width <= width && column + word_width > width {
                    spans.push(Span::styled(std::mem::take(&mut chunk), style));
                    output.push(Line::from(std::mem::take(&mut spans)));
                    column = 0;
                    if word.chars().all(char::is_whitespace) {
                        continue;
                    }
                }
                for g in word.graphemes(true) {
                    let g = if g == "\t" {
                        "    "
                    } else if g.chars().any(char::is_control) {
                        continue;
                    } else {
                        g
                    };
                    if column > 0 && column + g.width() > width {
                        spans.push(Span::styled(std::mem::take(&mut chunk), style));
                        output.push(Line::from(std::mem::take(&mut spans)));
                        column = 0;
                    }
                    chunk.push_str(g);
                    column += g.width();
                }
            }
            if !chunk.is_empty() {
                spans.push(Span::styled(chunk, style));
            }
        }
        output.push(Line::from(spans));
    }
    output
}

fn load_image(path: &Path, area: Rect, cell: CellPixels) -> io::Result<Bitmap> {
    if path.metadata()?.len() > 96 * 1024 * 1024 {
        return Err(io::Error::other("Image exceeds 96 MiB"));
    }
    let mut reader = ImageReader::open(path)?.with_guessed_format()?;
    let mut limits = Limits::default();
    limits.max_alloc = Some(128 * 1024 * 1024);
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    reader.limits(limits.clone());
    let mut decoder = reader.into_decoder().map_err(io::Error::other)?;
    limits
        .reserve(decoder.total_bytes())
        .map_err(io::Error::other)?;
    decoder.set_limits(limits).map_err(io::Error::other)?;
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let image = DynamicImage::from_decoder(decoder).map_err(io::Error::other)?;
    Bitmap::from_image(image, orientation, area, cell)
}

// Poppler runs on the workspace host. Bound the runtime and captured output of each page.
fn poppler(program: &str, args: &[&str], path: &Path) -> io::Result<Vec<u8>> {
    let mut output = tempfile::tempfile()?;
    let mut cmd = crate::noninteractive_process::command(program);
    cmd.env("LC_ALL", "C").args(args).arg(path);
    if program == "pdftotext" {
        cmd.arg("-");
    }
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                io::Error::other(format!(
                    "PDF preview requires {program} (Poppler) on the workspace host"
                ))
            } else {
                e
            }
        })?;
    let start = Instant::now();
    const LIMIT: u64 = 16 * 1024 * 1024;
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                return Err(io::Error::other(
                    "Could not read PDF page (damaged or encrypted document)",
                ));
            }
            break;
        }
        if start.elapsed() > Duration::from_secs(12) || output.metadata()?.len() > LIMIT {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other("PDF page exceeds preview limits"));
        }
        thread::sleep(Duration::from_millis(15));
    }
    if output.metadata()?.len() > LIMIT {
        return Err(io::Error::other("PDF page exceeds preview limits"));
    }
    output.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    output.take(LIMIT).read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn render_pdf(
    path: &Path,
    requested: u32,
    known: Option<u32>,
    text_mode: bool,
    r: Rect,
    cell: CellPixels,
) -> io::Result<Rendered> {
    let pages = match known {
        Some(n) => n,
        None => String::from_utf8_lossy(&poppler("pdfinfo", &[], path)?)
            .lines()
            .find_map(|line| line.strip_prefix("Pages:")?.trim().parse::<u32>().ok())
            .filter(|n| *n > 0)
            .ok_or_else(|| io::Error::other("PDF page count unavailable"))?,
    };
    let page = requested.clamp(1, pages);
    let n = page.to_string();
    let content = if text_mode {
        let bytes = pdf_text(path, &n)?;
        let text = String::from_utf8_lossy(&bytes);
        let text = if text.trim().is_empty() {
            "No selectable text on this page. Switch to Page view."
        } else {
            text.trim_end()
        };
        Content::Lines(wrap(Text::from(text), r.width))
    } else {
        let (w, h) = cell.bounds(r);
        let scale = w.max(h).min(2560).to_string();
        let bytes = poppler(
            "pdftoppm",
            &[
                "-f",
                &n,
                "-l",
                &n,
                "-singlefile",
                "-scale-to",
                &scale,
                "-png",
            ],
            path,
        )?;
        Content::Pixels(Bitmap::from_pdf_page(bytes)?)
    };
    Ok(Rendered {
        content,
        pages: Some(pages),
        page,
    })
}

fn pdf_text(path: &Path, n: &str) -> io::Result<Vec<u8>> {
    poppler(
        "pdftotext",
        &["-f", n, "-l", n, "-layout", "-nopgbrk", "-enc", "UTF-8"],
        path,
    )
}

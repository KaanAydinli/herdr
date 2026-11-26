use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use ratatui_textarea::{CursorMove, TextArea};

pub const MAX_BYTES: u64 = 8 * 1024 * 1024;

/// Bound reads even when another process grows a file during the read.
pub fn read_text(path: &Path) -> io::Result<String> {
    let meta = path.metadata()?;
    if meta.len() > MAX_BYTES {
        return Err(io::Error::other("Choose a file under 8 MiB"));
    }
    if !meta.is_file() {
        return Err(io::Error::other("Choose a regular file"));
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES || bytes.contains(&0) {
        return Err(io::Error::other("Choose a UTF-8 text file under 8 MiB"));
    }
    String::from_utf8(bytes).map_err(io::Error::other)
}

pub struct Document {
    pub path: PathBuf,
    pub colors: super::highlight::Colors,
    pub area: TextArea<'static>,
    baseline: String,
    crlf: bool,
    pub top: usize,
    pub left: usize,
    pub cursor_cell: usize,
    pub dirty: bool,
    pub external: bool,
}

impl Document {
    pub fn open(path: PathBuf) -> io::Result<Self> {
        let text = read_text(&path)?;
        Ok(Self::new(path, text))
    }

    fn new(path: PathBuf, baseline: String) -> Self {
        let crlf = baseline.contains("\r\n")
            && baseline
                .split_inclusive('\n')
                .all(|line| !line.ends_with('\n') || line.ends_with("\r\n"));
        let mut area: TextArea = baseline
            .split('\n')
            .map(|line| {
                if crlf {
                    line.strip_suffix('\r').unwrap_or(line)
                } else {
                    line
                }
            })
            .collect();
        area.set_tab_length(4);
        Self {
            path,
            area,
            colors: Default::default(),
            baseline,
            crlf,
            top: 0,
            left: 0,
            cursor_cell: 0,
            dirty: false,
            external: false,
        }
    }

    pub fn reload(&mut self, text: String) {
        let cursor = self.area.cursor();
        let (top, left) = (self.top, self.left);
        *self = Self::new(self.path.clone(), text);
        self.jump(cursor.0, cursor.1);
        self.top = top;
        self.left = left;
    }

    pub fn jump(&mut self, row: usize, col: usize) {
        self.area.move_cursor(CursorMove::Jump(
            row.min(u16::MAX as usize) as u16,
            col.min(u16::MAX as usize) as u16,
        ));
    }

    pub fn refresh(&mut self) -> io::Result<()> {
        match read_text(&self.path) {
            Ok(text) if text == self.baseline => self.external = false,
            Ok(text) if !self.dirty => self.reload(text),
            Ok(_) => self.external = true,
            Err(err) => {
                self.external = true;
                return Err(err);
            }
        }
        Ok(())
    }

    pub fn disk(&self) -> io::Result<Option<String>> {
        match read_text(&self.path) {
            Ok(text) => Ok(Some(text)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn baseline(&self) -> &str {
        &self.baseline
    }

    /// Check the reviewed revision immediately before the atomic replacement.
    pub fn save(&mut self, expected: Option<&str>) -> io::Result<()> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| io::Error::other("Missing parent directory"))?;
        let text = self
            .area
            .lines()
            .join(if self.crlf { "\r\n" } else { "\n" });
        let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
        tmp.write_all(text.as_bytes())?;
        if let Ok(meta) = self.path.metadata() {
            if meta.permissions().readonly() {
                return Err(io::Error::other("File is read-only"));
            }
            tmp.as_file().set_permissions(meta.permissions())?;
        }
        if self.disk()?.as_deref() != expected {
            self.external = true;
            return Err(io::Error::other("Disk changed again. Review before saving"));
        }
        if expected.is_none() {
            tmp.persist_noclobber(&self.path)?;
        } else {
            tmp.persist(&self.path)?;
        }
        self.baseline = text;
        self.dirty = false;
        self.external = false;
        Ok(())
    }
}

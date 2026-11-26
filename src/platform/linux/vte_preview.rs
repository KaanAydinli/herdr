//! Local transport between the Herdr client and an opted-in VTE host.
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::sync::OnceLock;

use crate::kitty_graphics::vte::{composite, Layer};

pub(crate) const CLEAR: &[u8] = b"\x1b]666;vte.ext.herdr.preview\x1b\\";

pub(crate) fn enabled() -> bool {
    std::env::var_os("HERDR_VTE_PREVIEW").as_deref() == Some("1".as_ref())
        && ["TMUX", "STY", "SSH_TTY", "HERDR_PANE_ID"]
            .iter()
            .all(|k| std::env::var_os(k).is_none())
}

// The widget updates this four-byte record on font or display scale changes.
// Keep one fd per client. Geometry polling reads it alongside the terminal ioctl.
fn metrics_file() -> Option<&'static File> {
    static FILE: OnceLock<Option<File>> = OnceLock::new();
    FILE.get_or_init(|| {
        if !enabled() {
            return None;
        }
        let name = std::env::var("HERDR_VTE_METRICS").ok()?;
        let suffix = name.strip_prefix("herdr-cell-")?;
        if !(6..=32).contains(&suffix.len())
            || !suffix
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_')
        {
            return None;
        }
        let root = std::path::PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?);
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(root.join(name))
            .ok()?;
        let meta = file.metadata().ok()?;
        // SAFETY: geteuid has no preconditions.
        (meta.is_file()
            && meta.len() == 4
            && meta.mode() & 0o077 == 0
            && meta.uid() == unsafe { libc::geteuid() })
        .then_some(file)
    })
    .as_ref()
}

pub(crate) fn raster_cell() -> Option<(u32, u32)> {
    let mut b = [0; 4];
    metrics_file()?.read_exact_at(&mut b, 0).ok()?;
    let w = u16::from_le_bytes([b[0], b[1]]);
    let h = u16::from_le_bytes([b[2], b[3]]);
    ((1..=256).contains(&w) && (1..=512).contains(&h)).then_some((w.into(), h.into()))
}

#[derive(Default)]
pub(crate) struct Presenter {
    current: Option<tempfile::NamedTempFile>,
}

impl Presenter {
    pub(crate) fn write(&mut self, out: &mut impl Write, layers: &[Layer]) -> io::Result<()> {
        let new_file = (|| {
            let (x, y, img) = composite(layers)?;
            let mut file = tempfile::Builder::new()
                .prefix("herdr-preview-")
                .rand_bytes(16)
                .tempfile_in(std::env::var_os("XDG_RUNTIME_DIR")?)
                .ok()?;
            let cell = layers.first()?.cell;
            let with_grid = metrics_file().is_some();
            file.write_all(if with_grid { b"HDP2" } else { b"HDP1" })
                .ok()?;
            for n in [x, y, img.width(), img.height()] {
                file.write_all(&n.to_le_bytes()).ok()?;
            }
            if with_grid {
                file.write_all(&cell.width_px.to_le_bytes()).ok()?;
                file.write_all(&cell.height_px.to_le_bytes()).ok()?;
            }
            file.write_all(img.as_raw()).ok()?;
            Some(file)
        })();
        if let Some(name) = new_file
            .as_ref()
            .and_then(|f| f.path().file_name()?.to_str())
        {
            write!(out, "\x1b]666;vte.ext.herdr.preview={name}\x1b\\")?;
        } else {
            out.write_all(CLEAR)?;
        }
        self.current = new_file;
        Ok(())
    }
}

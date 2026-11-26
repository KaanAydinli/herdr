//! Pixel previews travel through the pane's existing Kitty graphics channel.
use std::io::{self, Write};

use image::{
    codecs::png::PngEncoder, imageops::FilterType, metadata::Orientation, DynamicImage,
    GenericImage, ImageEncoder, RgbaImage,
};
use ratatui::layout::Rect;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CellPixels {
    width: u32,
    height: u32,
}

impl CellPixels {
    pub fn read() -> Self {
        if let Ok(size) = crossterm::terminal::window_size() {
            if size.columns > 0
                && size.rows > 0
                && size.width >= size.columns
                && size.height >= size.rows
            {
                let width = u32::from(size.width / size.columns);
                let height = u32::from(size.height / size.rows);
                if width <= 256 && height <= 512 {
                    return Self { width, height };
                }
            }
        }
        Self {
            width: 8,
            height: 16,
        }
    }

    pub fn bounds(self, area: Rect) -> (u32, u32) {
        // Bound decoded uploads even for unusually large or misreported terminal sizes.
        let w = (self.width * u32::from(area.width)).max(1);
        let h = (self.height * u32::from(area.height)).max(1);
        let scale = (4096.0 / f64::from(w.max(h)))
            .min((5_000_000.0 / (f64::from(w) * f64::from(h))).sqrt())
            .min(1.0);
        (
            ((f64::from(w) * scale) as u32).max(1),
            ((f64::from(h) * scale) as u32).max(1),
        )
    }
}

pub(super) struct Bitmap {
    png: Vec<u8>,
    width: u32,
    height: u32,
}

impl Bitmap {
    pub fn from_image(
        image: DynamicImage,
        orientation: Orientation,
        area: Rect,
        cell: CellPixels,
    ) -> io::Result<Self> {
        let bounds = cell.bounds(area);
        let (w, h) = if matches!(
            orientation,
            Orientation::Rotate90
                | Orientation::Rotate270
                | Orientation::Rotate90FlipH
                | Orientation::Rotate270FlipH
        ) {
            (bounds.1, bounds.0)
        } else {
            bounds
        };
        let mut image = if image.width() > w || image.height() > h {
            image.resize(w, h, FilterType::Lanczos3)
        } else {
            image
        };
        image.apply_orientation(orientation);
        // The multiplexer places rectangles in whole cells. Pad the edge to keep
        // a 1:1 image (including small icons and screenshots) pixel-aligned.
        // Keeping the same parity as the panel lets an integral-cell placement
        // put the original pixels at the exact center, including odd-sized grids.
        let centered_extent = |pixels: u32, pitch: u32, available: u16| {
            let cells = pixels.div_ceil(pitch);
            let spare = u32::from(available).saturating_sub(cells);
            (cells + spare % 2) * pitch
        };
        let width = centered_extent(image.width(), cell.width, area.width);
        let height = centered_extent(image.height(), cell.height, area.height);
        if width != image.width() || height != image.height() {
            let mut padded = RgbaImage::new(width, height);
            padded
                .copy_from(
                    &image,
                    (width - image.width()) / 2,
                    (height - image.height()) / 2,
                )
                .map_err(io::Error::other)?;
            image = DynamicImage::ImageRgba8(padded);
        }
        let mut png = Vec::new();
        if image.color().has_alpha() {
            let rgba = image.into_rgba8();
            PngEncoder::new(&mut png)
                .write_image(&rgba, width, height, image::ExtendedColorType::Rgba8)
                .map_err(io::Error::other)?;
        } else {
            let rgb = image.into_rgb8();
            PngEncoder::new(&mut png)
                .write_image(&rgb, width, height, image::ExtendedColorType::Rgb8)
                .map_err(io::Error::other)?;
        }
        Ok(Self { png, width, height })
    }

    pub fn from_pdf_page(png: Vec<u8>) -> io::Result<Self> {
        let (width, height) =
            image::ImageReader::with_format(io::Cursor::new(&png), image::ImageFormat::Png)
                .into_dimensions()
                .map_err(io::Error::other)?;
        Ok(Self { png, width, height })
    }

    fn placement(&self, area: Rect, cell: CellPixels) -> Rect {
        let w = u32::from(area.width) * cell.width;
        let h = u32::from(area.height) * cell.height;
        let scale = (f64::from(w) / f64::from(self.width))
            .min(f64::from(h) / f64::from(self.height))
            .min(1.0);
        let cols = (f64::from(self.width) * scale / f64::from(cell.width))
            .round()
            .clamp(1.0, f64::from(area.width)) as u16;
        let rows = (f64::from(self.height) * scale / f64::from(cell.height))
            .round()
            .clamp(1.0, f64::from(area.height)) as u16;
        Rect::new(
            area.x + (area.width - cols) / 2,
            area.y + (area.height - rows) / 2,
            cols,
            rows,
        )
    }
}

#[derive(Default)]
pub(super) struct Display {
    placed: Option<(u64, Rect)>,
}

impl Display {
    pub fn sync(
        &mut self,
        out: &mut impl Write,
        image: Option<(u64, &Bitmap)>,
        area: Rect,
        cell: CellPixels,
    ) -> io::Result<()> {
        let next = image
            .filter(|_| !area.is_empty())
            .map(|(id, img)| (id, img.placement(area, cell)));
        if self.placed == next {
            return Ok(());
        }
        let mut out = io::BufWriter::new(out);
        if self.placed.is_some() {
            // Only delete our own image in this pane's alternate screen.
            out.write_all(b"\x1b_Ga=d,d=I,i=1,q=2;\x1b\\")?;
        }
        if let (Some((_, rect)), Some((_, img))) = (next, image) {
            write!(out, "\x1b7\x1b[{};{}H", rect.y + 1, rect.x + 1)?;
            let control = format!(
                "a=T,f=100,i=1,p=1,c={},r={},C=1,q=2",
                rect.width, rect.height
            );
            crate::kitty_graphics::write_kitty_data(&mut out, &control, &img.png)?;
            out.write_all(b"\x1b8")?;
        }
        out.flush()?;
        self.placed = next;
        Ok(())
    }
}

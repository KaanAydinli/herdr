//! Lossless local VTE presentation. Geometry comes from the same clipped scene
//! used for native Kitty placement. Pixel work only happens at the output boundary.
use std::io::Cursor;
use std::sync::Arc;

use image::{imageops, ImageFormat, ImageReader, RgbImage, RgbaImage};

use super::{clipped_placement, HostPlacement};

const MAX_PIXELS: u64 = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Bounds {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

#[derive(Clone, Debug)]
pub(crate) struct Layer {
    pub(crate) cell: super::HostCellSize,
    source: Bounds,
    dest: Bounds,
    size: (u32, u32),
    format: u32,
    pub(super) z: i32,
    data: Arc<[u8]>,
}

impl PartialEq for Layer {
    fn eq(&self, rhs: &Self) -> bool {
        self.cell == rhs.cell
            && self.source == rhs.source
            && self.dest == rhs.dest
            && self.size == rhs.size
            && self.format == rhs.format
            && self.z == rhs.z
            && Arc::ptr_eq(&self.data, &rhs.data)
    }
}

impl Layer {
    pub(super) fn from_host(host: &HostPlacement) -> Option<Self> {
        let (p, format) = clipped_placement(host)?;
        let cell = host.cell_size;
        let dest = Bounds {
            x: u32::from(p.x)
                .checked_mul(cell.width_px)?
                .checked_add(p.x_offset)?,
            y: u32::from(p.y)
                .checked_mul(cell.height_px)?
                .checked_add(p.y_offset)?,
            w: p.cols.checked_mul(cell.width_px)?.checked_sub(p.x_offset)?,
            h: p.rows
                .checked_mul(cell.height_px)?
                .checked_sub(p.y_offset)?,
        };
        let size = (host.placement.image_width, host.placement.image_height);
        if !fits(size.0, size.1) || !fits(dest.w, dest.h) {
            return None;
        }
        Some(Self {
            cell,
            source: Bounds {
                x: p.source_x,
                y: p.source_y,
                w: p.source_width,
                h: p.source_height,
            },
            dest,
            size,
            format,
            z: host.placement.z,
            data: host.raw_data.clone()?,
        })
    }

    fn decode(&self) -> Option<RgbaImage> {
        let (w, h) = self.size;
        let n = (u64::from(w) * u64::from(h)) as usize;
        match self.format {
            32 if self.data.len() == n * 4 => RgbaImage::from_raw(w, h, self.data.to_vec()),
            24 if self.data.len() == n * 3 => Some(
                image::DynamicImage::ImageRgb8(RgbImage::from_raw(w, h, self.data.to_vec())?)
                    .into_rgba8(),
            ),
            100 => {
                let reader =
                    || ImageReader::with_format(Cursor::new(&*self.data), ImageFormat::Png);
                if reader().into_dimensions().ok()? != self.size {
                    return None;
                }
                let mut png = reader();
                let mut limits = image::Limits::default();
                limits.max_alloc = Some(MAX_PIXELS * 8);
                limits.max_image_width = Some(w);
                limits.max_image_height = Some(h);
                png.limits(limits);
                Some(png.decode().ok()?.into_rgba8())
            }
            _ => None,
        }
    }
}

fn fits(w: u32, h: u32) -> bool {
    w > 0 && h > 0 && u64::from(w) * u64::from(h) <= MAX_PIXELS
}

pub(crate) fn composite(layers: &[Layer]) -> Option<(u32, u32, RgbaImage)> {
    let x = layers.iter().map(|l| l.dest.x).min()?;
    let y = layers.iter().map(|l| l.dest.y).min()?;
    let w = layers
        .iter()
        .map(|l| (l.dest.x - x).saturating_add(l.dest.w))
        .max()?;
    let h = layers
        .iter()
        .map(|l| (l.dest.y - y).saturating_add(l.dest.h))
        .max()?;
    if !fits(w, h) {
        return None;
    }
    if let [layer] = layers {
        if layer.source == (Bounds { x: 0, y: 0, w, h }) && layer.size == (w, h) {
            return Some((x, y, layer.decode()?));
        }
    }
    let mut canvas = RgbaImage::new(w, h);
    let mut decoded: Option<(&Layer, RgbaImage)> = None;
    let mut budget = MAX_PIXELS * 8;
    for layer in layers {
        let d = layer.dest;
        let s = layer.source;
        if decoded.as_ref().is_none_or(|(last, _)| {
            !Arc::ptr_eq(&last.data, &layer.data)
                || last.size != layer.size
                || last.format != layer.format
        }) {
            budget = budget.checked_sub(u64::from(layer.size.0) * u64::from(layer.size.1))?;
            decoded = Some((layer, layer.decode()?));
        }
        budget = budget.checked_sub(u64::from(d.w) * u64::from(d.h))?;
        let img = &decoded.as_ref()?.1;
        if s.x.checked_add(s.w)? > img.width() || s.y.checked_add(s.h)? > img.height() {
            return None;
        }
        let view = imageops::crop_imm(img, s.x, s.y, s.w, s.h);
        let (dx, dy) = (i64::from(d.x - x), i64::from(d.y - y));
        if (d.w, d.h) == (s.w, s.h) {
            imageops::overlay(&mut canvas, &*view, dx, dy);
        } else {
            let scaled = imageops::resize(&*view, d.w, d.h, imageops::FilterType::Lanczos3);
            imageops::overlay(&mut canvas, &scaled, dx, dy);
        }
    }
    Some((x, y, canvas))
}

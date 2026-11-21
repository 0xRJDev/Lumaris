// Image information box (key I, right-click menu > Image info): file, size, format, dates
// and the camera details stored in the image (EXIF).
use std::path::Path;

use resvg::tiny_skia::{FillRule, Paint, Pixmap, Stroke, Transform};

use crate::decode::Decoded;
use crate::ui::{color, draw_text, rounded_rect, style, text_width};

pub struct InfoBox {
    lines: Vec<(String, String)>,
    pixmap: Option<Pixmap>,
    scale: f32,
    /// centred in the window (help) instead of the top left corner (image info)
    centered: bool,
    /// red, green, blue histograms (256 levels)
    histogram: Option<Box<[[u32; 256]; 3]>>,
}

/// Histogram of the image (every pixel up to ~1 MP, sampled above)
fn histogram(img: &Decoded) -> Box<[[u32; 256]; 3]> {
    let mut h = Box::new([[0u32; 256]; 3]);
    let pixels = img.rgba.len() / 4;
    let step = (pixels / 1_000_000).max(1);
    for px in img.rgba.chunks_exact(4).step_by(step) {
        if px[3] == 0 {
            continue; // transparent
        }
        h[0][px[0] as usize] += 1;
        h[1][px[1] as usize] += 1;
        h[2][px[2] as usize] += 1;
    }
    h
}

fn human_size(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1024.0 * 1024.0 * 1024.0 {
        format!("{:.2} GB", b / 1024.0 / 1024.0 / 1024.0)
    } else if b >= 1024.0 * 1024.0 {
        format!("{:.1} MB", b / 1024.0 / 1024.0)
    } else if b >= 1024.0 {
        format!("{:.0} KB", b / 1024.0)
    } else {
        format!("{bytes} bytes")
    }
}

fn exif_lines(path: &Path, out: &mut Vec<(String, String)>) {
    let Ok(file) = std::fs::File::open(path) else { return };
    let Ok(exif) = exif::Reader::new().read_from_container(&mut std::io::BufReader::new(file)) else { return };
    let get = |tag| exif.get_field(tag, exif::In::PRIMARY).map(|f| f.display_value().with_unit(&exif).to_string().trim_matches('"').trim().to_string());
    let camera = match (get(exif::Tag::Make), get(exif::Tag::Model)) {
        (Some(make), Some(model)) if model.starts_with(&make) || make.is_empty() => Some(model),
        (Some(make), Some(model)) => Some(format!("{make} {model}")),
        (None, m) => m,
        (m, None) => m,
    };
    let rows = [
        ("Camera", camera),
        ("Lens", get(exif::Tag::LensModel)),
        ("Taken", get(exif::Tag::DateTimeOriginal)),
        ("Exposure", get(exif::Tag::ExposureTime)),
        ("Aperture", get(exif::Tag::FNumber)),
        ("ISO", get(exif::Tag::PhotographicSensitivity)),
        ("Focal length", get(exif::Tag::FocalLength)),
    ];
    for (label, value) in rows {
        if let Some(v) = value.filter(|v| !v.is_empty()) {
            out.push((label.into(), v));
        }
    }
}

impl InfoBox {
    pub fn new(path: &Path, img: Option<&Decoded>, position: (usize, usize), zoom: f64, scale: f64) -> InfoBox {
        let mut lines = Vec::new();
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        lines.push(("Name".into(), name));
        if let Some(dir) = path.parent() {
            let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
            let shown = match home.as_ref().and_then(|h| dir.strip_prefix(h).ok()) {
                Some(rel) if rel.as_os_str().is_empty() => "~".to_string(),
                Some(rel) => format!("~/{}", rel.display()),
                None => dir.display().to_string(),
            };
            lines.push(("Folder".into(), shown));
        }
        lines.push(("Position".into(), format!("{} of {}", position.0 + 1, position.1)));
        if let Some(img) = img {
            let mp = img.width as f64 * img.height as f64 / 1e6;
            let mut size = format!("{} × {} pixels", img.width, img.height);
            if mp >= 0.1 {
                size.push_str(&format!(" ({mp:.1} MP)"));
            }
            lines.push(("Dimensions".into(), size));
            if img.frames.len() > 1 {
                let total: f64 = img.frames.iter().map(|f| f.delay.as_secs_f64()).sum();
                lines.push(("Animation".into(), format!("{} frames, {total:.1} s", img.frames.len())));
            }
        }
        let ext = path.extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_default();
        if !ext.is_empty() {
            lines.push(("Format".into(), ext));
        }
        if let Ok(meta) = std::fs::metadata(path) {
            lines.push(("File size".into(), human_size(meta.len())));
            if let Ok(t) = meta.modified() {
                let t: chrono::DateTime<chrono::Local> = t.into();
                lines.push(("Modified".into(), t.format("%Y-%m-%d %H:%M").to_string()));
            }
        }
        lines.push(("Zoom".into(), format!("{:.0}%", zoom * 100.0)));
        exif_lines(path, &mut lines);
        InfoBox { lines, pixmap: None, scale: scale as f32, centered: false, histogram: img.map(histogram) }
    }

    /// A box with the given lines (label, text), centred in the window
    pub fn with_lines(lines: Vec<(String, String)>, scale: f64) -> InfoBox {
        InfoBox { lines, pixmap: None, scale: scale as f32, centered: true, histogram: None }
    }

    /// The box as a premultiplied RGBA image (pixels, width, height, x, y)
    pub fn layer(&mut self, window: (u32, u32)) -> Option<(&[u8], u32, u32, i32, i32)> {
        if self.pixmap.is_none() {
            self.pixmap = self.paint();
        }
        let margin = (12.0 * self.scale) as i32;
        let centered = self.centered;
        self.pixmap.as_ref().map(|p| {
            let (x, y) = if centered {
                (((window.0 as i32 - p.width() as i32) / 2).max(0), ((window.1 as i32 - p.height() as i32) / 2).max(0))
            } else {
                (margin, margin)
            };
            (p.data(), p.width(), p.height(), x, y)
        })
    }

    fn paint(&self) -> Option<Pixmap> {
        let st = style();
        let font = st.font.as_ref()?;
        let u = |v: f32| (v * self.scale).round();
        let px = (st.point_size * 96.0 / 72.0 * self.scale).round();
        let line_h = u(st.point_size * 2.2);
        let pad = u(12.0);
        let label_w = self.lines.iter().map(|(l, _)| text_width(font, l, px)).fold(0.0, f32::max);
        let value_w = self.lines.iter().map(|(_, v)| text_width(font, v, px)).fold(0.0, f32::max).min(u(520.0));
        let graph_h = if self.histogram.is_some() { u(90.0) } else { 0.0 };
        let w = (pad * 2.0 + label_w + u(16.0) + value_w).max(u(256.0) + pad * 2.0).ceil();
        let h = (pad * 2.0 + line_h * self.lines.len() as f32 + graph_h).ceil();
        let mut pm = Pixmap::new(w as u32, h as u32)?;
        let mut paint = Paint { anti_alias: true, ..Default::default() };
        if let Some(path) = rounded_rect(0.5, 0.5, w - 1.0, h - 1.0, u(6.0)) {
            paint.set_color(color(st.background, 0.92));
            pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
            paint.set_color(color(st.text, 0.18));
            pm.stroke_path(&path, &paint, &Stroke { width: 1.0, ..Default::default() }, Transform::identity(), None);
        }
        let (asc, desc) = font.horizontal_line_metrics(px).map(|m| (m.ascent, m.descent)).unwrap_or((px * 0.8, -px * 0.2));
        for (i, (label, value)) in self.lines.iter().enumerate() {
            let top = pad + line_h * i as f32;
            let baseline = (top + (line_h - (asc - desc)) / 2.0 + asc).round();
            draw_text(&mut pm, font, label, px, pad, baseline, st.text, 0.6);
            // long values are cut at the box width
            let mut v = value.clone();
            while text_width(font, &v, px) > value_w && v.chars().count() > 2 {
                v.pop();
                v.pop();
                v.push('…');
            }
            draw_text(&mut pm, font, &v, px, pad + label_w + u(16.0), baseline, st.text, 1.0);
        }
        if let Some(hist) = &self.histogram {
            // histogram: the three channels as filled curves, added up
            let (gx, gy, gw, gh) = (pad, pad + line_h * self.lines.len() as f32 + u(8.0), w - pad * 2.0, graph_h - u(12.0));
            paint.set_color(color(st.text, 0.06));
            if let Some(r) = resvg::tiny_skia::Rect::from_xywh(gx, gy, gw, gh) {
                pm.fill_rect(r, &paint, Transform::identity(), None);
            }
            // scale: ignore the extreme levels (pure black/white areas would flatten the rest)
            let mut peak = 1u32;
            for ch in hist.iter() {
                for v in &ch[1..255] {
                    peak = peak.max(*v);
                }
            }
            for (k, rgb) in [[255u8, 70, 70], [70, 220, 70], [80, 120, 255]].iter().enumerate() {
                let mut pb = resvg::tiny_skia::PathBuilder::new();
                pb.move_to(gx, gy + gh);
                for (level, v) in hist[k].iter().enumerate() {
                    let x = gx + gw * level as f32 / 255.0;
                    let y = gy + gh - gh * ((*v as f32 / peak as f32).min(1.0)).sqrt();
                    pb.line_to(x, y);
                }
                pb.line_to(gx + gw, gy + gh);
                pb.close();
                if let Some(path) = pb.finish() {
                    paint.set_color(color(*rgb, 0.45));
                    pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
                }
            }
        }
        Some(pm)
    }
}

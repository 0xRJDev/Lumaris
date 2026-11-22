// JPEG decoding straight to RGBA (no RGB-to-RGBA pass). Files with a restart marker at the end
// of every row (or every few rows) of blocks are cut into horizontal bands at those markers;
// the bands are complete JPEG streams of their own and are decoded on all cores at once.
use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

/// Images smaller than this are decoded on one thread (starting threads would cost more).
const PARALLEL_MIN_PIXELS: usize = 2_000_000;

fn options() -> DecoderOptions {
    DecoderOptions::default()
        .jpeg_set_out_colorspace(ColorSpace::RGBA)
        .set_max_width(1 << 16)
        .set_max_height(1 << 16)
        .set_strict_mode(false)
}

/// RGBA pixels of a colour (YCbCr or RGB) JPEG. None: another kind of JPEG (greyscale, CMYK)
/// or an error; the caller then uses the general decoder.
pub fn decode(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let mut header = JpegDecoder::new_with_options(ZCursor::new(bytes), options());
    header.decode_headers().ok()?;
    if !matches!(header.input_colorspace()?, ColorSpace::YCbCr | ColorSpace::RGB) {
        return None;
    }
    let (w, h) = header.dimensions()?;
    let mut out = vec![0u8; w * h * 4];
    if w * h < PARALLEL_MIN_PIXELS || !decode_bands(bytes, w, h, &mut out) {
        let mut d = JpegDecoder::new_with_options(ZCursor::new(bytes), options());
        d.decode_into(&mut out).ok()?;
    }
    Some((w as u32, h as u32, out))
}

/// Where a baseline JPEG can be cut: header, height field, entropy coded data and restart markers.
struct Layout {
    /// offset of the frame height (2 bytes, big endian) in the SOF segment
    height_at: usize,
    /// start of the entropy coded data (end of the SOS segment)
    data_start: usize,
    /// offset of the end of image marker (or the end of the file)
    data_end: usize,
    /// offsets of the restart markers in the entropy coded data
    restarts: Vec<usize>,
    /// pixel rows per restart interval
    rows_per_interval: usize,
    /// chroma has fewer rows than luma (4:2:0): its upsampling looks at the rows around
    subsampled: bool,
}

fn be16(b: &[u8], at: usize) -> Option<usize> {
    Some(u16::from_be_bytes([*b.get(at)?, *b.get(at + 1)?]) as usize)
}

fn layout(b: &[u8], width: usize, height: usize) -> Option<Layout> {
    if b.get(..2)? != [0xFF, 0xD8] {
        return None;
    }
    let (mut at, mut height_at, mut interval, mut block_w, mut block_h, mut subsampled) = (2, None, 0, 0, 0, false);
    let data_start = loop {
        // a marker, maybe preceded by fill bytes
        while *b.get(at)? == 0xFF && *b.get(at + 1)? == 0xFF {
            at += 1;
        }
        if *b.get(at)? != 0xFF {
            return None;
        }
        let marker = *b.get(at + 1)?;
        let len = be16(b, at + 2)?;
        let seg = at + 4; // segment contents
        match marker {
            // baseline or extended sequential, Huffman coded: the only kinds that can be cut
            0xC0 | 0xC1 => {
                let comps = *b.get(seg + 5)? as usize;
                if comps != 3 || be16(b, seg + 1)? != height || be16(b, seg + 3)? != width {
                    return None;
                }
                let (mut hmax, mut vmax) = (1, 1);
                for c in 0..comps {
                    let s = *b.get(seg + 6 + c * 3 + 1)? as usize;
                    hmax = hmax.max(s >> 4);
                    vmax = vmax.max(s & 15);
                }
                (block_w, block_h, height_at, subsampled) = (8 * hmax, 8 * vmax, Some(seg + 1), vmax > 1);
            }
            0xC2..=0xCF if marker != 0xC4 && marker != 0xC8 && marker != 0xCC => return None, // progressive, lossless, arithmetic
            0xDD => interval = be16(b, seg)?,
            0xDA => {
                // all three components in this one scan
                if *b.get(seg)? != 3 {
                    return None;
                }
                break at + 2 + len;
            }
            0xD9 => return None,
            _ => {}
        }
        at += 2 + len;
    };
    let height_at = height_at?;
    let blocks_per_row = width.div_ceil(block_w);
    if interval == 0 || block_w == 0 || interval % blocks_per_row != 0 {
        return None; // no restart markers, or not at the end of a row of blocks
    }
    let rows_per_interval = interval / blocks_per_row * block_h;
    // find the restart markers (0xFF 0x00 is a stuffed data byte)
    let mut restarts = Vec::new();
    let mut i = data_start;
    let data_end = loop {
        let Some(off) = b.get(i..)?.iter().position(|&x| x == 0xFF) else { break b.len() };
        i += off;
        match b.get(i + 1) {
            None => break b.len(),
            Some(0x00) => i += 2,
            Some(0xFF) => i += 1,
            Some(0xD0..=0xD7) => {
                restarts.push(i);
                i += 2;
            }
            Some(0xD9) => break i,
            Some(_) => return None, // another scan or table: not a single sequential scan
        }
    };
    if restarts.len() + 1 != height.div_ceil(rows_per_interval) {
        return None;
    }
    Some(Layout { height_at, data_start, data_end, restarts, rows_per_interval, subsampled })
}

/// Decodes the image in bands on all cores into `out` (width x height RGBA). False: the file
/// cannot be cut, or a band failed; `out` is then to be decoded as a whole.
fn decode_bands(b: &[u8], width: usize, height: usize, out: &mut [u8]) -> bool {
    let Some(l) = layout(b, width, height) else { return false };
    let intervals = l.restarts.len() + 1;
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).min(intervals);
    if threads < 2 {
        return false;
    }
    let per_band = intervals.div_ceil(threads);
    // with subsampled chroma, each band is decoded with one interval more above and below and
    // those rows are dropped: the rows at its edges then get the same colours as in the whole image
    let margin = usize::from(l.subsampled);
    let rpi = l.rows_per_interval;
    // (stream, rows decoded, first row to keep, rows to keep)
    let mut bands: Vec<(Vec<u8>, usize, usize, usize)> = Vec::new();
    for first in (0..intervals).step_by(per_band) {
        let last = (first + per_band).min(intervals);
        let (from, to) = (first.saturating_sub(margin), (last + margin).min(intervals));
        let decoded = (to * rpi).min(height) - from * rpi;
        let keep = (last * rpi).min(height) - first * rpi;
        let start = if from == 0 { l.data_start } else { l.restarts[from - 1] + 2 };
        let end = if to == intervals { l.data_end } else { l.restarts[to - 1] };
        let mut s = Vec::with_capacity(l.data_start + end - start + 2);
        s.extend_from_slice(&b[..l.height_at]);
        s.extend_from_slice(&(decoded as u16).to_be_bytes());
        s.extend_from_slice(&b[l.height_at + 2..l.data_start]);
        s.extend_from_slice(&b[start..end]);
        s.extend_from_slice(&[0xFF, 0xD9]);
        bands.push((s, decoded, (first - from) * rpi, keep));
    }
    let row = width * 4;
    std::thread::scope(|scope| {
        let mut rest = out;
        let mut jobs = Vec::new();
        for (stream, decoded, skip, keep) in &bands {
            let (part, tail) = std::mem::take(&mut rest).split_at_mut(keep * row);
            rest = tail;
            jobs.push(scope.spawn(move || {
                let mut d = JpegDecoder::new_with_options(ZCursor::new(stream.as_slice()), options());
                if decoded == keep {
                    return d.decode_into(part).is_ok() && d.dimensions() == Some((width, *decoded));
                }
                let mut tmp = vec![0u8; decoded * row];
                if d.decode_into(&mut tmp).is_err() || d.dimensions() != Some((width, *decoded)) {
                    return false;
                }
                part.copy_from_slice(&tmp[skip * row..(skip + keep) * row]);
                true
            }));
        }
        jobs.into_iter().all(|j| j.join().unwrap_or(false))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn whole(bytes: &[u8]) -> (usize, usize, Vec<u8>) {
        let mut d = JpegDecoder::new_with_options(ZCursor::new(bytes), options());
        let rgba = d.decode().unwrap();
        let (w, h) = d.dimensions().unwrap();
        (w, h, rgba)
    }

    /// Restart markers at every row of blocks, no chroma subsampling: bands decode exactly
    /// like the whole image.
    #[test]
    fn bands_444() {
        let bytes = include_bytes!("../tests/data/restart-444.jpg");
        let (w, h, expected) = whole(bytes);
        let mut out = vec![0u8; w * h * 4];
        assert!(decode_bands(bytes, w, h, &mut out));
        assert!(out == expected);
    }

    /// Every two rows of 4:2:0 blocks (bands overlap for the chroma upsampling): exact too.
    #[test]
    fn bands_420() {
        let bytes = include_bytes!("../tests/data/restart-420.jpg");
        let (w, h, expected) = whole(bytes);
        let mut out = vec![0u8; w * h * 4];
        assert!(decode_bands(bytes, w, h, &mut out));
        assert!(out == expected);
    }

    #[test]
    fn without_restart_markers() {
        let img = image::RgbImage::from_fn(300, 200, |x, y| image::Rgb([x as u8, y as u8, (x ^ y) as u8]));
        let mut bytes = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 90).encode_image(&img).unwrap();
        assert!(layout(&bytes, 300, 200).is_none());
        let (w, h, rgba) = decode(&bytes).unwrap();
        assert_eq!((w, h, rgba.len()), (300, 200, 300 * 200 * 4));
        assert!(rgba.chunks(4).all(|p| p[3] == 255));
    }
}

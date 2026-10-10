//! Image decoding. The fast path avoids decoding full-size JPEGs:
//! camera JPEGs embed MPF previews (Sigma BF: 2836x2006 at the end of the file),
//! and raw files embed JPEG previews in their TIFF structure.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::library::Kind;

pub struct Decoded {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

#[derive(Default, Clone)]
pub struct Meta {
    /// e.g. "1/250s  f/2.8  ISO 400  45mm"
    pub exposure: String,
    /// Full-resolution size after orientation, for 1:1 zoom.
    pub dims: Option<(u32, u32)>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Size {
    Thumb,
    Preview,
    Full,
}

const THUMB_EDGE: u32 = 240;
const PREVIEW_MAX_EDGE: u32 = 3200;

pub fn load(path: &Path, kind: Kind, size: Size) -> Result<(Decoded, Option<Meta>), String> {
    match kind {
        Kind::Video => Err("video".into()),
        Kind::Jpeg => load_jpeg(path, size),
        Kind::Raw => load_raw(path, size),
        Kind::Image => {
            let img = image::open(path).map_err(|e| e.to_string())?;
            let img = fit(img.to_rgba8(), size);
            Ok((to_decoded(img), None))
        }
    }
}

fn load_jpeg(path: &Path, size: Size) -> Result<(Decoded, Option<Meta>), String> {
    let mut f = File::open(path).map_err(|e| e.to_string())?;
    // Header holds EXIF (with thumbnail) and the MPF index. 64 KB covers it.
    let mut head = vec![0u8; 64 * 1024];
    let n = read_up_to(&mut f, &mut head)?;
    head.truncate(n);

    let exif = parse_exif(&head);
    let orientation = exif.as_ref().map(exif_orientation).unwrap_or(1);
    let mut meta = exif.as_ref().map(exif_meta).unwrap_or_default();
    meta.dims = sof_dims(&head).map(|(w, h)| if orientation >= 5 { (h, w) } else { (w, h) });
    let meta = Some(meta);

    let bytes: Vec<u8> = match size {
        // The EXIF thumbnail is already in the header, so it costs no extra card reads.
        Size::Thumb => match exif.as_ref().and_then(exif_thumbnail) {
            Some(t) => {
                let img = image::load_from_memory_with_format(&t, image::ImageFormat::Jpeg)
                    .map_err(|e| e.to_string())?
                    .to_rgba8();
                let img = match sof_dims(&head) {
                    Some(d) => crop_letterbox(img, d),
                    None => img,
                };
                return Ok((to_decoded(orient(img, orientation)), meta));
            }
            None => match pick_mpf(&head, size) {
                Some(p) => read_range(&mut f, p.0, p.1)?,
                None => read_all(&mut f)?,
            },
        },
        Size::Preview => match pick_mpf(&head, size) {
            Some(p) => read_range(&mut f, p.0, p.1)?,
            None => read_all(&mut f)?,
        },
        Size::Full => read_all(&mut f)?,
    };

    let img = image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg)
        .map_err(|e| e.to_string())?
        .to_rgba8();
    let img = orient(fit(img, size), orientation);
    Ok((to_decoded(img), meta))
}

fn load_raw(path: &Path, size: Size) -> Result<(Decoded, Option<Meta>), String> {
    let mut f = File::open(path).map_err(|e| e.to_string())?;
    let previews = tiff_jpegs(&mut f).unwrap_or_default();
    // Smallest preview that is big enough, else the largest.
    let want = if size == Size::Thumb { 0 } else { 900_000 };
    let pick = previews
        .iter()
        .filter(|p| p.1 >= want)
        .min_by_key(|p| p.1)
        .or_else(|| previews.iter().max_by_key(|p| p.1))
        .ok_or("no embedded preview")?;
    let bytes = read_range(&mut f, pick.0, pick.1)?;
    let exif = parse_exif_tiff(&mut f);
    let orientation = exif.as_ref().map(exif_orientation).unwrap_or(1);
    let meta = exif.as_ref().map(exif_meta);
    let img = image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg)
        .map_err(|e| e.to_string())?
        .to_rgba8();
    Ok((to_decoded(orient(fit(img, size), orientation)), meta))
}

fn to_decoded(img: image::RgbaImage) -> Decoded {
    Decoded { width: img.width() as usize, height: img.height() as usize, rgba: img.into_raw() }
}

fn fit(img: image::RgbaImage, size: Size) -> image::RgbaImage {
    let max = match size {
        Size::Thumb => THUMB_EDGE,
        Size::Preview => PREVIEW_MAX_EDGE,
        Size::Full => return img,
    };
    let (w, h) = img.dimensions();
    if w.max(h) <= max {
        return img;
    }
    let s = max as f32 / w.max(h) as f32;
    image::imageops::thumbnail(&img, (w as f32 * s) as u32, (h as f32 * s) as u32)
}

/// Camera EXIF thumbnails are 4:3 with black bars when the sensor is not 4:3.
/// Crops them to the primary image's aspect ratio.
fn crop_letterbox(img: image::RgbaImage, (w, h): (u32, u32)) -> image::RgbaImage {
    let (tw, th) = img.dimensions();
    let want = w as f32 / h as f32;
    let have = tw as f32 / th as f32;
    if (want / have - 1.0).abs() < 0.02 {
        return img;
    }
    let (cw, ch) = if have > want { ((th as f32 * want) as u32, th) } else { (tw, (tw as f32 / want) as u32) };
    image::imageops::crop_imm(&img, (tw - cw) / 2, (th - ch) / 2, cw, ch).to_image()
}

fn orient(img: image::RgbaImage, o: u32) -> image::RgbaImage {
    use image::imageops::*;
    match o {
        2 => flip_horizontal(&img),
        3 => rotate180(&img),
        4 => flip_vertical(&img),
        5 => flip_horizontal(&rotate90(&img)),
        6 => rotate90(&img),
        7 => flip_horizontal(&rotate270(&img)),
        8 => rotate270(&img),
        _ => img,
    }
}

// ---------- IO helpers ----------

fn read_up_to(f: &mut File, buf: &mut [u8]) -> Result<usize, String> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(n)
}

fn read_range(f: &mut File, off: u64, len: u64) -> Result<Vec<u8>, String> {
    f.seek(SeekFrom::Start(off)).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; len as usize];
    f.read_exact(&mut buf).map_err(|e| e.to_string())?;
    Ok(buf)
}

fn read_all(f: &mut File) -> Result<Vec<u8>, String> {
    f.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).map_err(|e| e.to_string())?;
    Ok(buf)
}

// ---------- JPEG / MPF ----------

/// Walks JPEG markers up to SOS and returns (offset, payload) of each APPn segment.
fn app_segments(d: &[u8]) -> Vec<(usize, u8, &[u8])> {
    let mut out = vec![];
    if d.len() < 4 || d[0] != 0xFF || d[1] != 0xD8 {
        return out;
    }
    let mut i = 2;
    while i + 4 <= d.len() && d[i] == 0xFF {
        let m = d[i + 1];
        if m == 0xDA || m == 0xD9 {
            break;
        }
        let len = u16::from_be_bytes([d[i + 2], d[i + 3]]) as usize;
        let start = i + 4;
        let end = (i + 2 + len).min(d.len());
        if start <= end {
            out.push((start, m, &d[start..end]));
        }
        i += 2 + len;
    }
    out
}

/// Primary image size from the first SOF marker.
fn sof_dims(d: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2;
    while i + 9 <= d.len() && d[i] == 0xFF {
        let m = d[i + 1];
        if matches!(m, 0xC0..=0xCF) && !matches!(m, 0xC4 | 0xC8 | 0xCC) {
            let h = u16::from_be_bytes([d[i + 5], d[i + 6]]) as u32;
            let w = u16::from_be_bytes([d[i + 7], d[i + 8]]) as u32;
            return Some((w, h));
        }
        if m == 0xDA {
            return None;
        }
        i += 2 + u16::from_be_bytes([d[i + 2], d[i + 3]]) as usize;
    }
    None
}

/// Picks an MPF preview for `size` and returns its (file offset, length).
fn pick_mpf(head: &[u8], size: Size) -> Option<(u64, u64)> {
    let (start, _, _) = app_segments(head)
        .into_iter()
        .find(|(_, m, s)| *m == 0xE2 && s.starts_with(b"MPF\0"))?;
    let base = start + 4;
    let t = &head[base..];
    let le = t.get(0..2)? == b"II";
    let r16 = |o: usize| -> Option<u32> {
        let b = t.get(o..o + 2)?;
        Some(if le { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) } as u32)
    };
    let r32 = |o: usize| -> Option<u32> {
        let b: [u8; 4] = t.get(o..o + 4)?.try_into().ok()?;
        Some(if le { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) })
    };
    let ifd = r32(4)? as usize;
    let count = r16(ifd)? as usize;
    let mut entries = None;
    for k in 0..count {
        let e = ifd + 2 + 12 * k;
        if r16(e)? == 0xB002 {
            entries = Some((r32(e + 4)? as usize, r32(e + 8)? as usize));
        }
    }
    let (n_bytes, off) = entries?;
    let mut previews: Vec<(u64, u64)> = vec![];
    for j in 0..n_bytes / 16 {
        let e = off + 16 * j;
        let attr = r32(e)?;
        let len = r32(e + 4)? as u64;
        let img_off = r32(e + 8)? as u64;
        // Only CIPA "large thumbnail" types (0x010001-0x010003) are previews. Sigma BF also
        // stores an ISO 21496 HDR gain map here (type 0x050000); showing it looks dark and oversaturated.
        if img_off == 0 || !(0x010001..=0x010003).contains(&(attr & 0x00FF_FFFF)) {
            continue;
        }
        previews.push(((base as u64) + img_off, len));
    }
    // Byte size tracks pixel count closely enough to choose without decoding.
    match size {
        Size::Thumb => previews.into_iter().min_by_key(|p| p.1),
        Size::Preview => previews.into_iter().max_by_key(|p| p.1),
        Size::Full => None,
    }
}

// ---------- EXIF ----------

fn parse_exif(head: &[u8]) -> Option<exif::Exif> {
    let (_, _, seg) = app_segments(head)
        .into_iter()
        .find(|(_, m, s)| *m == 0xE1 && s.starts_with(b"Exif\0\0"))?;
    exif::Reader::new().read_raw(seg[6..].to_vec()).ok()
}

fn parse_exif_tiff(f: &mut File) -> Option<exif::Exif> {
    let mut head = vec![0u8; 256 * 1024];
    f.seek(SeekFrom::Start(0)).ok()?;
    let n = read_up_to(f, &mut head).ok()?;
    head.truncate(n);
    exif::Reader::new().read_raw(head).ok()
}

fn exif_orientation(e: &exif::Exif) -> u32 {
    e.get_field(exif::Tag::Orientation, exif::In::PRIMARY)
        .and_then(|f| f.value.get_uint(0))
        .unwrap_or(1)
}

fn exif_thumbnail(e: &exif::Exif) -> Option<Vec<u8>> {
    let off = e.get_field(exif::Tag::JPEGInterchangeFormat, exif::In::THUMBNAIL)?.value.get_uint(0)? as usize;
    let len = e.get_field(exif::Tag::JPEGInterchangeFormatLength, exif::In::THUMBNAIL)?.value.get_uint(0)? as usize;
    e.buf().get(off..off + len).map(|s| s.to_vec())
}

fn exif_meta(e: &exif::Exif) -> Meta {
    let field = |t| e.get_field(t, exif::In::PRIMARY);
    let rational = |t| match field(t).map(|f| &f.value) {
        Some(exif::Value::Rational(v)) if !v.is_empty() && v[0].denom != 0 => Some(v[0].to_f64()),
        _ => None,
    };
    let mut parts = vec![];
    if let Some(s) = rational(exif::Tag::ExposureTime) {
        parts.push(if s >= 0.5 { format!("{s:.1}s") } else { format!("1/{:.0}s", 1.0 / s) });
    }
    if let Some(n) = rational(exif::Tag::FNumber) {
        parts.push(format!("f/{n:.1}").replace(".0", ""));
    }
    if let Some(iso) = field(exif::Tag::PhotographicSensitivity).and_then(|f| f.value.get_uint(0)) {
        parts.push(format!("ISO {iso}"));
    }
    if let Some(mm) = rational(exif::Tag::FocalLength) {
        parts.push(format!("{mm:.0}mm"));
    }
    Meta { exposure: parts.join("   "), dims: None }
}

// ---------- TIFF (raw) embedded JPEGs ----------

/// Returns (offset, length) of every JPEG stream referenced by IFD0, its chain, and SubIFDs.
fn tiff_jpegs(f: &mut File) -> Option<Vec<(u64, u64)>> {
    let mut hdr = [0u8; 8];
    f.seek(SeekFrom::Start(0)).ok()?;
    f.read_exact(&mut hdr).ok()?;
    let le = match &hdr[0..2] {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_ = |b: &[u8]| if le { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) };
    let u32_ = |b: &[u8]| {
        let a = [b[0], b[1], b[2], b[3]];
        if le { u32::from_le_bytes(a) } else { u32::from_be_bytes(a) }
    };
    let mut out = vec![];
    let mut queue = vec![u32_(&hdr[4..8]) as u64];
    let mut seen = 0;
    while let Some(off) = queue.pop() {
        seen += 1;
        if off == 0 || seen > 32 {
            continue;
        }
        f.seek(SeekFrom::Start(off)).ok()?;
        let mut nb = [0u8; 2];
        f.read_exact(&mut nb).ok()?;
        let n = u16_(&nb) as usize;
        let mut buf = vec![0u8; n * 12 + 4];
        f.read_exact(&mut buf).ok()?;
        let (mut jpeg_off, mut jpeg_len, mut strip_off, mut strip_len, mut compression) = (0, 0, 0, 0, 0);
        for k in 0..n {
            let e = &buf[k * 12..k * 12 + 12];
            let tag = u16_(&e[0..2]);
            let typ = u16_(&e[2..4]);
            let cnt = u32_(&e[4..8]);
            let val = if typ == 3 { u16_(&e[8..10]) as u32 } else { u32_(&e[8..12]) };
            match tag {
                0x0103 => compression = val,
                0x0111 if cnt == 1 => strip_off = val,
                0x0117 if cnt == 1 => strip_len = val,
                0x0201 => jpeg_off = val,
                0x0202 => jpeg_len = val,
                0x014A => {
                    if cnt == 1 {
                        queue.push(val as u64);
                    } else {
                        let mut sb = vec![0u8; cnt as usize * 4];
                        let pos = f.stream_position().ok()?;
                        f.seek(SeekFrom::Start(val as u64)).ok()?;
                        if f.read_exact(&mut sb).is_ok() {
                            queue.extend(sb.chunks(4).map(|c| u32_(c) as u64));
                        }
                        f.seek(SeekFrom::Start(pos)).ok()?;
                    }
                }
                _ => {}
            }
        }
        if jpeg_off > 0 && jpeg_len > 0 {
            out.push((jpeg_off as u64, jpeg_len as u64));
        }
        // Compression 6/7 with one strip is a baseline JPEG preview (lossless raw data
        // also uses 7 but spans many tiles, so the single-strip rule skips it).
        if (compression == 6 || compression == 7) && strip_off > 0 && strip_len > 0 {
            out.push((strip_off as u64, strip_len as u64));
        }
        queue.push(u32_(&buf[n * 12..n * 12 + 4]) as u64);
    }
    // Only keep streams that actually start with a JPEG SOI marker.
    out.retain(|&(o, _)| {
        let mut b = [0u8; 2];
        f.seek(SeekFrom::Start(o)).is_ok() && f.read_exact(&mut b).is_ok() && b == [0xFF, 0xD8]
    });
    Some(out)
}

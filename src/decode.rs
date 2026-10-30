//! Reads the flattened ("composite") image that Photoshop stores at the end of
//! every PSD/PSB file, and turns it into 8-bit RGB pixels.
//!
//! Reading the composite (rather than re-compositing every layer) is fast, uses
//! little memory, and matches what Photoshop itself shows. It supports:
//!   * PSD and PSB (large document) files
//!   * 1, 8, 16 and 32 bits per channel
//!   * RGB, CMYK, Grayscale, Duotone, Bitmap, Indexed, Lab and Multichannel
//!   * raw and RLE-compressed image data
//!
//! If a layered file was saved with "Maximize Compatibility" turned off, the
//! stored composite may be blank. In that case we fall back to compositing the
//! layers ourselves (RGB 8-bit files only) and report a warning.

use anyhow::{bail, Context, Result};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

pub struct Image {
    pub width: u32,
    pub height: u32,
    /// Tightly packed RGB, 3 bytes per pixel.
    pub rgb: Vec<u8>,
    pub warnings: Vec<String>,
}

const MODE_BITMAP: u16 = 0;
const MODE_GRAY: u16 = 1;
const MODE_INDEXED: u16 = 2;
const MODE_RGB: u16 = 3;
const MODE_CMYK: u16 = 4;
const MODE_MULTICHANNEL: u16 = 7;
const MODE_DUOTONE: u16 = 8;
const MODE_LAB: u16 = 9;

struct Reader<R: Read + Seek> {
    r: R,
}

impl<R: Read + Seek> Reader<R> {
    fn bytes<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut b = [0u8; N];
        self.r.read_exact(&mut b).context("file ended unexpectedly")?;
        Ok(b)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes::<1>()?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.bytes()?))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.bytes()?))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(self.bytes()?))
    }
    fn skip(&mut self, n: u64) -> Result<()> {
        self.r.seek(SeekFrom::Current(n as i64))?;
        Ok(())
    }
    fn vec(&mut self, n: usize) -> Result<Vec<u8>> {
        let mut v = vec![0u8; n];
        self.r.read_exact(&mut v).context("file ended unexpectedly")?;
        Ok(v)
    }
}

pub fn decode(path: &Path) -> Result<Image> {
    let file = File::open(path).context("could not open file")?;
    let mut rd = Reader { r: BufReader::with_capacity(1 << 20, file) };

    // ---- File header ----
    if &rd.bytes::<4>()? != b"8BPS" {
        bail!("not a Photoshop file (bad signature)");
    }
    let version = rd.u16()?;
    let is_psb = match version {
        1 => false,
        2 => true,
        v => bail!("unsupported PSD version {v}"),
    };
    rd.skip(6)?;
    let channels = rd.u16()? as usize;
    let height = rd.u32()?;
    let width = rd.u32()?;
    let depth = rd.u16()?;
    let mode = rd.u16()?;
    if width == 0 || height == 0 {
        bail!("image has zero size");
    }
    if ![1, 8, 16, 32].contains(&depth) {
        bail!("unsupported bit depth {depth}");
    }

    // ---- Colour mode data (palette for indexed images) ----
    let cm_len = rd.u32()? as usize;
    let palette = if mode == MODE_INDEXED && cm_len >= 768 {
        let p = rd.vec(768)?;
        rd.skip((cm_len - 768) as u64)?;
        Some(p)
    } else {
        rd.skip(cm_len as u64)?;
        None
    };

    // ---- Image resources: look for "has real merged data" (id 1057) ----
    let res_len = rd.u32()? as u64;
    let res_end = rd.r.stream_position()? + res_len;
    let mut has_real_merged = true;
    while rd.r.stream_position()? + 12 <= res_end {
        if &rd.bytes::<4>()? != b"8BIM" {
            break;
        }
        let id = rd.u16()?;
        let name_len = rd.u8()? as u64;
        // Pascal string padded so (1 + len) is even.
        rd.skip(if (name_len + 1) % 2 == 1 { name_len + 1 } else { name_len })?;
        let size = rd.u32()? as u64;
        let padded = size + (size & 1);
        if id == 1057 && size >= 5 {
            let _ver = rd.u32()?;
            has_real_merged = rd.u8()? != 0;
            rd.skip(padded - 5)?;
        } else {
            rd.skip(padded)?;
        }
    }
    rd.r.seek(SeekFrom::Start(res_end))?;

    // ---- Layer and mask section: skip ----
    let layer_len = if is_psb { rd.u64()? } else { rd.u32()? as u64 };
    rd.skip(layer_len)?;

    // ---- Composite image data ----
    let compression = rd.u16()?;
    let colour_channels = match mode {
        MODE_BITMAP | MODE_GRAY | MODE_INDEXED | MODE_DUOTONE => 1,
        MODE_RGB | MODE_LAB => 3,
        MODE_CMYK => 4,
        MODE_MULTICHANNEL => channels.min(3),
        m => bail!("unsupported colour mode {m}"),
    }
    .min(channels);
    if colour_channels == 0 {
        bail!("file has no colour channels");
    }

    let w = width as usize;
    let h = height as usize;
    let row_bytes = (w * depth as usize).div_ceil(8);
    let mut planes: Vec<Vec<u8>> = Vec::with_capacity(colour_channels);

    match compression {
        0 => {
            for _ in 0..colour_channels {
                let mut plane = Vec::with_capacity(w * h);
                for _ in 0..h {
                    let row = rd.vec(row_bytes)?;
                    to_8bit(&row, depth, w, &mut plane)?;
                }
                planes.push(plane);
            }
        }
        1 => {
            // Byte counts for every row of every channel come first.
            let total_rows = channels * h;
            let mut counts = Vec::with_capacity(total_rows);
            for _ in 0..total_rows {
                counts.push(if is_psb { rd.u32()? as usize } else { rd.u16()? as usize });
            }
            let mut row = vec![0u8; row_bytes];
            for c in 0..colour_channels {
                let mut plane = Vec::with_capacity(w * h);
                for y in 0..h {
                    let packed = rd.vec(counts[c * h + y])?;
                    unpack_bits(&packed, &mut row);
                    to_8bit(&row, depth, w, &mut plane)?;
                }
                planes.push(plane);
            }
        }
        2 | 3 => bail!("ZIP-compressed composite data is not supported"),
        c => bail!("unknown compression type {c}"),
    }

    let mut rgb = vec![0u8; w * h * 3];
    match mode {
        MODE_RGB => {
            let (r, g, b) = (&planes[0], plane_or(&planes, 1), plane_or(&planes, 2));
            for i in 0..w * h {
                rgb[i * 3] = r[i];
                rgb[i * 3 + 1] = g[i];
                rgb[i * 3 + 2] = b[i];
            }
        }
        MODE_CMYK => {
            // Photoshop stores CMYK inverted (255 = no ink).
            let k_plane = planes.get(3);
            for i in 0..w * h {
                let k = k_plane.map_or(255, |p| p[i]) as u32;
                for (j, p) in planes.iter().take(3).enumerate() {
                    rgb[i * 3 + j] = ((p[i] as u32 * k + 127) / 255) as u8;
                }
            }
        }
        MODE_LAB => {
            let (l, a, b) = (&planes[0], plane_or(&planes, 1), plane_or(&planes, 2));
            for i in 0..w * h {
                let px = lab_to_srgb(l[i], a[i], b[i]);
                rgb[i * 3..i * 3 + 3].copy_from_slice(&px);
            }
        }
        MODE_INDEXED => {
            let pal = palette.as_deref();
            for (i, &v) in planes[0].iter().enumerate() {
                let v = v as usize;
                let px = match pal {
                    Some(p) => [p[v], p[256 + v], p[512 + v]],
                    None => [v as u8; 3],
                };
                rgb[i * 3..i * 3 + 3].copy_from_slice(&px);
            }
        }
        _ => {
            // Grayscale, duotone, bitmap, multichannel: show as grayscale.
            for (i, &v) in planes[0].iter().enumerate() {
                rgb[i * 3..i * 3 + 3].fill(v);
            }
        }
    }

    let mut image = Image { width, height, rgb, warnings: Vec::new() };

    if !has_real_merged {
        match composite_layers(path, w, h) {
            Ok(layers_rgb) => {
                image.rgb = layers_rgb;
                image.warnings.push(
                    "saved without \"Maximize Compatibility\"; layers were combined by the converter, so check it looks right".into(),
                );
            }
            Err(e) => image.warnings.push(format!(
                "saved without \"Maximize Compatibility\"; the JPEG may be blank or incomplete ({e}). Re-save it in Photoshop with Maximize Compatibility on."
            )),
        }
    }

    Ok(image)
}

fn plane_or(planes: &[Vec<u8>], i: usize) -> &Vec<u8> {
    planes.get(i).unwrap_or(&planes[0])
}

/// Fallback: composite visible layers (RGB 8-bit only) onto white.
fn composite_layers(path: &Path, w: usize, h: usize) -> Result<Vec<u8>> {
    let bytes = std::fs::read(path)?;
    // The layer library panics on some features (e.g. ZIP-compressed layers),
    // so contain that and report it as an ordinary error.
    std::panic::catch_unwind(|| composite_layers_inner(&bytes, w, h))
        .unwrap_or_else(|p| Err(anyhow::anyhow!("{}", crate::convert::panic_text(&p))))
}

fn composite_layers_inner(bytes: &[u8], w: usize, h: usize) -> Result<Vec<u8>> {
    let psd = psd::Psd::from_bytes(bytes).map_err(|e| anyhow::anyhow!("{e}"))?;
    if psd.layers().is_empty() {
        bail!("no layers found");
    }
    let rgba = psd
        .flatten_layers_rgba(&|(_, layer)| layer.visible())
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    if rgba.len() != w * h * 4 {
        bail!("layer data size mismatch");
    }
    let mut rgb = vec![0u8; w * h * 3];
    for i in 0..w * h {
        let a = rgba[i * 4 + 3] as u32;
        for c in 0..3 {
            let v = rgba[i * 4 + c] as u32;
            rgb[i * 3 + c] = ((v * a + 255 * (255 - a) + 127) / 255) as u8;
        }
    }
    Ok(rgb)
}

/// PackBits RLE decoding for one row.
fn unpack_bits(src: &[u8], dst: &mut [u8]) {
    let (mut i, mut o) = (0, 0);
    while i < src.len() && o < dst.len() {
        let n = src[i] as i8;
        i += 1;
        if n >= 0 {
            let count = (n as usize + 1).min(dst.len() - o).min(src.len() - i);
            dst[o..o + count].copy_from_slice(&src[i..i + count]);
            i += n as usize + 1;
            o += count;
        } else if n != -128 {
            let count = ((1 - n as isize) as usize).min(dst.len() - o);
            if i < src.len() {
                dst[o..o + count].fill(src[i]);
            }
            i += 1;
            o += count;
        }
    }
    dst[o..].fill(0);
}

/// Convert one row of channel samples at any supported depth to 8-bit values.
fn to_8bit(row: &[u8], depth: u16, w: usize, out: &mut Vec<u8>) -> Result<()> {
    match depth {
        8 => out.extend_from_slice(&row[..w]),
        16 => out.extend(row.chunks_exact(2).take(w).map(|c| {
            let v = u16::from_be_bytes([c[0], c[1]]) as u32;
            ((v * 255 + 32767) / 65535) as u8
        })),
        32 => out.extend(row.chunks_exact(4).take(w).map(|c| {
            // 32-bit Photoshop data is linear light; apply sRGB-ish gamma.
            let v = f32::from_be_bytes([c[0], c[1], c[2], c[3]]).clamp(0.0, 1.0);
            (v.powf(1.0 / 2.2) * 255.0 + 0.5) as u8
        })),
        1 => out.extend((0..w).map(|x| {
            // Bitmap mode: a set bit means black.
            if row[x / 8] & (0x80 >> (x % 8)) != 0 { 0 } else { 255 }
        })),
        d => bail!("unsupported bit depth {d}"),
    }
    Ok(())
}

fn lab_to_srgb(l: u8, a: u8, b: u8) -> [u8; 3] {
    let l = l as f32 * 100.0 / 255.0;
    let a = a as f32 - 128.0;
    let b = b as f32 - 128.0;
    let fy = (l + 16.0) / 116.0;
    let fx = fy + a / 500.0;
    let fz = fy - b / 200.0;
    let finv = |t: f32| if t > 6.0 / 29.0 { t * t * t } else { 3.0 * (6.0f32 / 29.0).powi(2) * (t - 4.0 / 29.0) };
    // D65 white point
    let x = 0.950_47 * finv(fx);
    let y = finv(fy);
    let z = 1.088_83 * finv(fz);
    let r = 3.2406 * x - 1.5372 * y - 0.4986 * z;
    let g = -0.9689 * x + 1.8758 * y + 0.0415 * z;
    let bl = 0.0557 * x - 0.2040 * y + 1.0570 * z;
    let gamma = |c: f32| {
        let c = c.clamp(0.0, 1.0);
        let v = if c <= 0.003_130_8 { 12.92 * c } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
        (v * 255.0 + 0.5) as u8
    };
    [gamma(r), gamma(g), gamma(bl)]
}

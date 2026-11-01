//! Camera raw files (Sony .ARW and other formats the rawler library knows).
//!
//! The raw sensor data is fully developed (demosaic, white balance, colour
//! calibration to sRGB) at full resolution, then rotated to match how the
//! camera was held. Raw data has no "look" applied, so it comes out flat and
//! dark; to match what the camera itself would have produced, the brightness,
//! contrast and saturation are matched to the preview JPEG the camera stores in
//! every raw file (or a standard tone curve is used when there isn't one). If the camera model isn't supported for full development,
//! the full-size preview the camera stored inside the file is used instead and a
//! warning is reported.

use crate::decode::Image;
use anyhow::{anyhow, Result};
use rawler::decoders::{Orientation, RawDecodeParams};
use rawler::imgop::develop::RawDevelop;
use rawler::rawsource::RawSource;
use std::path::Path;

pub fn decode(path: &Path) -> Result<Image> {
    match develop(path) {
        Ok(img) => Ok(img),
        Err(full_err) => {
            // Fall back to the camera's own embedded JPEG preview.
            let preview = rawler::analyze::extract_preview_pixels(path, &RawDecodeParams::default())
                .map_err(|e| anyhow!("could not read this raw file ({full_err}; preview: {e})"))?
                .to_rgb8();
            let (width, height) = preview.dimensions();
            Ok(Image {
                width,
                height,
                rgb: preview.into_raw(),
                warnings: vec![format!(
                    "this camera's raw format couldn't be fully developed ({full_err}), so the preview image stored in the file was used ({width}×{height})"
                )],
            })
        }
    }
}

fn develop(path: &Path) -> Result<Image> {
    let source = RawSource::new(path)?;
    let decoder = rawler::get_decoder(&source).map_err(|e| anyhow!("{e}"))?;
    let raw = decoder.raw_image(&source, &RawDecodeParams::default(), false).map_err(|e| anyhow!("{e}"))?;
    let orientation = raw.orientation;
    let developed = RawDevelop::default()
        .develop_intermediate(&raw)
        .map_err(|e| anyhow!("{e}"))?
        .to_dynamic_image()
        .ok_or_else(|| anyhow!("could not develop the raw image"))?
        .to_rgb8();
    let (w, h) = developed.dimensions();
    let mut rgb = developed.into_raw();
    let preview = rawler::analyze::extract_preview_pixels(path, &RawDecodeParams::default())
        .ok()
        .map(|p| p.to_rgb8())
        .filter(|p| p.width() >= 320 && p.height() >= 200);
    match preview {
        Some(p) => match_look(&mut rgb, p.as_raw()),
        None => standard_look(&mut rgb),
    }
    let (width, height, rgb) = orient(w, h, rgb, orientation);
    Ok(Image { width, height, rgb, warnings: Vec::new() })
}

fn luma(p: &[u8]) -> usize {
    (p[0] as usize * 2 + p[1] as usize * 5 + p[2] as usize) / 8
}

/// Average "colourfulness" of an image, sampling at most ~1M pixels.
fn mean_saturation(rgb: &[u8], lut: Option<&[u8; 256]>) -> f64 {
    let step = (rgb.len() / 3 / 1_000_000).max(1) * 3;
    let (mut sum, mut n) = (0.0, 0usize);
    for px in rgb.chunks_exact(3).step_by(step / 3) {
        let px = match lut {
            Some(l) => [l[px[0] as usize], l[px[1] as usize], l[px[2] as usize]],
            None => [px[0], px[1], px[2]],
        };
        let mx = *px.iter().max().unwrap() as f64;
        let mn = *px.iter().min().unwrap() as f64;
        if mx > 8.0 {
            sum += (mx - mn) / mx;
            n += 1;
        }
    }
    if n == 0 { 0.0 } else { sum / n as f64 }
}

fn cdf(rgb: &[u8]) -> [f64; 256] {
    let step = (rgb.len() / 3 / 1_000_000).max(1);
    let mut hist = [0f64; 256];
    for px in rgb.chunks_exact(3).step_by(step) {
        hist[luma(px)] += 1.0;
    }
    let total: f64 = hist.iter().sum::<f64>().max(1.0);
    let mut acc = 0.0;
    let mut out = [0f64; 256];
    for i in 0..256 {
        acc += hist[i];
        out[i] = acc / total;
    }
    out
}

/// Match brightness/contrast (by histogram) and saturation to the camera's preview.
fn match_look(rgb: &mut [u8], preview: &[u8]) {
    let (ours, theirs) = (cdf(rgb), cdf(preview));
    let mut lut = [0u8; 256];
    let mut j = 0;
    for i in 0..256 {
        while j < 255 && theirs[j] < ours[i] {
            j += 1;
        }
        lut[i] = j as u8;
    }
    // Smooth the curve a little so it can't create banding, keep it rising.
    let mut smooth = [0u8; 256];
    for i in 0..256usize {
        let (a, b) = (i.saturating_sub(4), (i + 4).min(255));
        let avg = lut[a..=b].iter().map(|&v| v as u32).sum::<u32>() / (b - a + 1) as u32;
        smooth[i] = avg as u8;
    }
    smooth[0] = lut[0].min(smooth[0]);
    for i in 1..256 {
        smooth[i] = smooth[i].max(smooth[i - 1]);
    }
    let sat = {
        let ours = mean_saturation(rgb, Some(&smooth));
        let theirs = mean_saturation(preview, None);
        if ours > 0.01 { (theirs / ours).clamp(0.85, 1.6) } else { 1.0 }
    };
    apply(rgb, &smooth, sat);
}

/// No usable preview: lift exposure a little and add a gentle S-curve.
fn standard_look(rgb: &mut [u8]) {
    let mut lut = [0u8; 256];
    for (i, v) in lut.iter_mut().enumerate() {
        let x = i as f64 / 255.0;
        let lifted = 1.0 - (1.0 - x).powf(1.35); // brighten
        // Blend in an S-curve for a bit more contrast.
        let s = 0.7 * lifted + 0.3 * (0.5 - 0.5 * (std::f64::consts::PI * lifted).cos());
        *v = (s.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    }
    apply(rgb, &lut, 1.15);
}

fn apply(rgb: &mut [u8], lut: &[u8; 256], saturation: f64) {
    use rayon::prelude::*;
    rgb.par_chunks_exact_mut(3).for_each(|px| {
        let c = [lut[px[0] as usize] as f64, lut[px[1] as usize] as f64, lut[px[2] as usize] as f64];
        let y = (c[0] * 2.0 + c[1] * 5.0 + c[2]) / 8.0;
        for k in 0..3 {
            px[k] = (y + (c[k] - y) * saturation).clamp(0.0, 255.0).round() as u8;
        }
    });
}

/// Rotate/flip an RGB buffer according to the camera's orientation tag.
fn orient(w: u32, h: u32, rgb: Vec<u8>, o: Orientation) -> (u32, u32, Vec<u8>) {
    let (w, h) = (w as usize, h as usize);
    // Maps a destination (x, y) to the source pixel.
    let (dw, dh, map): (usize, usize, Box<dyn Fn(usize, usize) -> (usize, usize)>) = match o {
        Orientation::Normal | Orientation::Unknown => return (w as u32, h as u32, rgb),
        Orientation::HorizontalFlip => (w, h, Box::new(move |x, y| (w - 1 - x, y))),
        Orientation::Rotate180 => (w, h, Box::new(move |x, y| (w - 1 - x, h - 1 - y))),
        Orientation::VerticalFlip => (w, h, Box::new(move |x, y| (x, h - 1 - y))),
        Orientation::Transpose => (h, w, Box::new(move |x, y| (y, x))),
        Orientation::Rotate90 => (h, w, Box::new(move |x, y| (y, h - 1 - x))),
        Orientation::Transverse => (h, w, Box::new(move |x, y| (w - 1 - y, h - 1 - x))),
        Orientation::Rotate270 => (h, w, Box::new(move |x, y| (w - 1 - y, x))),
    };
    let mut out = vec![0u8; dw * dh * 3];
    for y in 0..dh {
        for x in 0..dw {
            let (sx, sy) = map(x, y);
            let s = (sy * w + sx) * 3;
            let d = (y * dw + x) * 3;
            out[d..d + 3].copy_from_slice(&rgb[s..s + 3]);
        }
    }
    (dw as u32, dh as u32, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2×1 image: red, blue.
    fn img() -> Vec<u8> {
        vec![255, 0, 0, 0, 0, 255]
    }

    #[test]
    fn rotate90_is_clockwise() {
        // Tag value 6 ("rotate 90 CW to view"): red ends up on top.
        let (w, h, out) = orient(2, 1, img(), Orientation::Rotate90);
        assert_eq!((w, h), (1, 2));
        assert_eq!(out, vec![255, 0, 0, 0, 0, 255]);
    }

    #[test]
    fn rotate270_is_counter_clockwise() {
        let (w, h, out) = orient(2, 1, img(), Orientation::Rotate270);
        assert_eq!((w, h), (1, 2));
        assert_eq!(out, vec![0, 0, 255, 255, 0, 0]);
    }
}

//! Finding PSD files and converting them in parallel.

use anyhow::{Context, Result};
use rayon::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use walkdir::WalkDir;

#[derive(Clone, Debug)]
pub struct Options {
    /// JPEG quality, 1–100.
    pub quality: u8,
    /// Leave JPEGs that already exist alone (handy for resuming a big batch).
    pub skip_existing: bool,
    /// Number of files converted at once. 0 = one per CPU core.
    pub threads: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self { quality: 92, skip_existing: true, threads: 0 }
    }
}

pub struct Job {
    pub src: PathBuf,
    pub out: PathBuf,
}

pub enum Event<'a> {
    Converted { job: &'a Job, warnings: &'a [String] },
    Skipped { job: &'a Job },
    Failed { job: &'a Job, error: String },
}

/// Where the converted copy of `source` will go: `<dest>/<source folder name>`.
pub fn output_root(source: &Path, dest: &Path) -> PathBuf {
    let name = source
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_else(|| "Converted".into());
    dest.join(name)
}

fn is_psd(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    // Skip macOS "._" metadata files that appear on external drives.
    if name.starts_with("._") {
        return false;
    }
    matches!(
        path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(),
        Some("psd") | Some("psb")
    )
}

/// Walk `source` and every subfolder, returning one job per PSD/PSB file.
pub fn find_jobs(source: &Path, out_root: &Path) -> Vec<Job> {
    let mut jobs: Vec<Job> = WalkDir::new(source)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        // Don't descend into the output folder if it lives inside the source.
        .filter(|e| e.file_type().is_file() && !e.path().starts_with(out_root) && is_psd(e.path()))
        .map(|e| {
            let rel = e.path().strip_prefix(source).unwrap_or(e.path());
            let out = out_root.join(rel).with_extension("jpg");
            Job { src: e.path().to_path_buf(), out }
        })
        .collect();
    jobs.sort_by(|a, b| a.src.cmp(&b.src));

    // "photo.psd" and "photo.psb" in the same folder would both become photo.jpg.
    let mut seen = std::collections::HashSet::new();
    for job in &mut jobs {
        if !seen.insert(job.out.clone()) {
            let stem = job.out.file_stem().unwrap_or_default().to_string_lossy().into_owned();
            let ext = job.src.extension().unwrap_or_default().to_string_lossy().to_lowercase();
            job.out.set_file_name(format!("{stem} ({ext}).jpg"));
        }
    }
    jobs
}

fn convert_one(job: &Job, quality: u8) -> Result<Vec<String>> {
    let img = crate::decode::decode(&job.src)?;
    if img.width > u16::MAX as u32 || img.height > u16::MAX as u32 {
        anyhow::bail!(
            "image is {}×{} pixels; JPEG can't be larger than 65535 pixels on a side",
            img.width,
            img.height
        );
    }
    if let Some(parent) = job.out.parent() {
        fs::create_dir_all(parent).context("could not create output folder")?;
    }
    // Write to a temporary file first so a cancelled run never leaves half a JPEG.
    let tmp = job.out.with_extension("jpg.part");
    let encoder = jpeg_encoder::Encoder::new_file(&tmp, quality).context("could not create output file")?;
    encoder
        .encode(&img.rgb, img.width as u16, img.height as u16, jpeg_encoder::ColorType::Rgb)
        .context("could not write JPEG")?;
    fs::rename(&tmp, &job.out).context("could not finalise output file")?;
    Ok(img.warnings)
}

/// Convert every job, calling `on_event` as each one finishes. Stops starting
/// new files once `cancel` is set.
pub fn run<F>(jobs: &[Job], opts: &Options, cancel: &AtomicBool, on_event: F) -> Result<()>
where
    F: Fn(Event) + Sync,
{
    let pool = rayon::ThreadPoolBuilder::new().num_threads(opts.threads).build()?;
    pool.install(|| {
        jobs.par_iter().for_each(|job| {
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            if opts.skip_existing && job.out.exists() {
                on_event(Event::Skipped { job });
                return;
            }
            // A badly damaged file must never take the whole batch down with it.
            let result = std::panic::catch_unwind(|| convert_one(job, opts.quality))
                .unwrap_or_else(|p| Err(anyhow::anyhow!("unexpected problem reading this file ({})", panic_text(&p))));
            match result {
                Ok(w) => on_event(Event::Converted { job, warnings: &w }),
                Err(e) => {
                    let _ = fs::remove_file(job.out.with_extension("jpg.part"));
                    on_event(Event::Failed { job, error: format!("{e:#}") })
                }
            }
        });
    });
    Ok(())
}

pub fn panic_text(p: &Box<dyn std::any::Any + Send>) -> String {
    p.downcast_ref::<String>()
        .cloned()
        .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_else(|| "unknown error".into())
}

/// Save a plain-text list of problems next to the converted files.
pub fn write_report(out_root: &Path, lines: &[String]) -> Option<PathBuf> {
    if lines.is_empty() {
        return None;
    }
    let path = out_root.join("conversion-problems.txt");
    let _ = fs::create_dir_all(out_root);
    fs::write(&path, lines.join("\n") + "\n").ok()?;
    Some(path)
}

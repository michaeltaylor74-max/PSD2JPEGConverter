//! Finding files and converting them in parallel.

use anyhow::{Context, Result};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
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
    /// Put every JPEG straight into one folder instead of copying the subfolders.
    pub flatten: bool,
    /// Search every level of subfolders (otherwise only the chosen folder).
    pub include_subfolders: bool,
    /// Also convert camera raw files (Sony .ARW and others).
    pub convert_raw: bool,
    /// Also copy JPEGs that are already in the folder.
    pub copy_jpegs: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            quality: 92,
            skip_existing: true,
            threads: 0,
            flatten: false,
            include_subfolders: true,
            convert_raw: false,
            copy_jpegs: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    /// An existing JPEG, copied as is.
    Jpeg,
    /// Photoshop PSD / PSB.
    Photoshop,
    /// Camera raw (ARW etc.).
    Raw,
}

pub struct Job {
    pub src: PathBuf,
    pub out: PathBuf,
    pub kind: Kind,
}

pub enum Event<'a> {
    Converted { job: &'a Job, warnings: &'a [String] },
    Skipped { job: &'a Job },
    Failed { job: &'a Job, error: String },
}

/// Where the converted copy of `source` will go: `<dest>/<source folder name>`.
pub fn output_root(source: &Path, dest: &Path) -> PathBuf {
    let name = source.file_name().map(|n| n.to_os_string()).unwrap_or_else(|| "Converted".into());
    dest.join(name)
}

const RAW_EXTS: &[&str] =
    &["arw", "srf", "sr2", "cr2", "cr3", "nef", "nrw", "raf", "orf", "rw2", "dng", "pef", "srw", "3fr", "iiq"];

/// Work out what kind of file this is, if it's one we handle.
pub fn classify(path: &Path, opts: &Options) -> Option<Kind> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    // Skip macOS "._" metadata files that appear on external drives.
    if name.starts_with("._") {
        return None;
    }
    let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some("psd") | Some("psb") => Some(Kind::Photoshop),
        Some(e) if RAW_EXTS.contains(&e) => opts.convert_raw.then_some(Kind::Raw),
        Some("jpg") | Some("jpeg") | Some("jpe") => opts.copy_jpegs.then_some(Kind::Jpeg),
        // Older Mac files often have no extension, or a name like "Logo v1.2"
        // that only looks like one, so look inside those.
        Some(e) if e.len() <= 5 && e.chars().all(|c| c.is_ascii_alphabetic()) => None,
        _ => has_psd_signature(path).then_some(Kind::Photoshop),
    }
}

fn has_psd_signature(path: &Path) -> bool {
    use std::io::Read;
    let mut sig = [0u8; 4];
    fs::File::open(path).and_then(|mut f| f.read_exact(&mut sig)).is_ok() && &sig == b"8BPS"
}

/// Name for the output file without ".jpg": "photo.psd" -> "photo", but
/// "Logo v1.2" (no real extension) keeps its full name.
fn base_name(path: &Path) -> String {
    let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_default();
    let known = ["psd", "psb", "jpg", "jpeg", "jpe"].contains(&ext.as_str()) || RAW_EXTS.contains(&ext.as_str());
    let name = if known { path.file_stem() } else { path.file_name() };
    name.unwrap_or_default().to_string_lossy().into_owned()
}

/// Everything found in a folder, plus any folders that couldn't be read.
#[derive(Default)]
pub struct ScanResult {
    pub files: Vec<(PathBuf, Kind)>,
    pub problems: Vec<String>,
}

impl ScanResult {
    pub fn count(&self, kind: Kind) -> usize {
        self.files.iter().filter(|(_, k)| *k == kind).count()
    }
}

/// Search `source`. With `include_subfolders` it goes through every level of
/// subfolders, including folders that are shortcuts (symbolic links /
/// junctions) to somewhere else. Folders that can't be opened are reported
/// rather than silently skipped. `on_found` is called after each file found;
/// setting `stop` ends the search early.
pub fn scan(
    source: &Path,
    exclude: Option<&Path>,
    opts: &Options,
    stop: &AtomicBool,
    on_found: impl Fn(Kind),
) -> ScanResult {
    let mut result = ScanResult::default();
    let mut walker = WalkDir::new(source).follow_links(true);
    if !opts.include_subfolders {
        walker = walker.max_depth(1);
    }
    let mut it = walker.into_iter();
    while let Some(entry) = it.next() {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                let place = err.path().map(|p| p.display().to_string()).unwrap_or_default();
                if err.loop_ancestor().is_some() {
                    // A shortcut back to a folder already being searched: its
                    // files are found anyway, so there's nothing to report.
                } else {
                    let why = err.io_error().map(|e| e.to_string()).unwrap_or_else(|| err.to_string());
                    result.problems.push(format!("COULD NOT OPEN {place}: {why}"));
                }
                continue;
            }
        };
        // Don't search inside the output folder if it lives inside the source.
        if exclude.is_some_and(|ex| entry.path().starts_with(ex)) {
            if entry.file_type().is_dir() {
                it.skip_current_dir();
            }
            continue;
        }
        if entry.file_type().is_file() {
            if let Some(kind) = classify(entry.path(), opts) {
                result.files.push((entry.into_path(), kind));
                on_found(kind);
            }
        }
    }
    result
}

/// Decide where each file found by [`scan`] goes.
///
/// With `flatten` off, the subfolder layout is copied under `out_root`.
/// With it on, every JPEG goes straight into `out_root`; when files in
/// different folders share a name, the folders are added to the name, e.g.
/// `2024 - Holiday - beach.jpg`.
///
/// When two files would still end up with the same name (for example a camera's
/// `DSC001.ARW` and `DSC001.JPG` side by side), the copied JPEG keeps its name
/// and the converted one gets its type added: `DSC001 (ARW).jpg`.
pub fn find_jobs(source: &Path, files: &[(PathBuf, Kind)], out_root: &Path, flatten: bool) -> Vec<Job> {
    let mut found: Vec<(Kind, PathBuf, PathBuf)> = files
        .iter()
        .map(|(p, k)| (*k, p.clone(), p.strip_prefix(source).unwrap_or(p).to_path_buf()))
        .collect();
    // Copies first (so they keep their names), then by path.
    found.sort_by(|a, b| (a.0 != Kind::Jpeg, &a.1).cmp(&(b.0 != Kind::Jpeg, &b.1)));

    let folders_of = |rel: &Path| -> Vec<String> {
        rel.parent()
            .map(|p| p.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect())
            .unwrap_or_default()
    };

    // In one-folder mode, which names turn up in more than one folder?
    let mut name_folders: HashMap<String, HashSet<PathBuf>> = HashMap::new();
    if flatten {
        for (_, _, rel) in &found {
            name_folders
                .entry(base_name(rel).to_lowercase())
                .or_default()
                .insert(rel.parent().unwrap_or(Path::new("")).to_path_buf());
        }
    }

    let mut jobs: Vec<Job> = found
        .into_iter()
        .map(|(kind, src, rel)| {
            let stem = base_name(&rel);
            // Copied JPEGs keep their exact file name; converted files get ".jpg".
            let file = if kind == Kind::Jpeg {
                rel.file_name().unwrap_or_default().to_string_lossy().into_owned()
            } else {
                format!("{stem}.jpg")
            };
            let out = if flatten {
                let folders = folders_of(&rel);
                let clash = name_folders[&stem.to_lowercase()].len() > 1;
                if clash && !folders.is_empty() {
                    out_root.join(format!("{} - {file}", folders.join(" - ")))
                } else {
                    out_root.join(file)
                }
            } else {
                out_root.join(rel.parent().unwrap_or(Path::new(""))).join(file)
            };
            Job { src, out, kind }
        })
        .collect();

    let lower = |p: &Path| p.to_string_lossy().to_lowercase();
    let mut seen = HashSet::new();
    for job in &mut jobs {
        if seen.insert(lower(&job.out)) {
            continue;
        }
        let stem = job.out.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        let ext = job.out.extension().unwrap_or_default().to_string_lossy().into_owned();
        let tag = job.src.extension().unwrap_or_default().to_string_lossy().to_uppercase();
        let tag = if tag.is_empty() { "PSD".to_string() } else { tag };
        job.out.set_file_name(format!("{stem} ({tag}).{ext}"));
        let mut n = 1;
        while !seen.insert(lower(&job.out)) {
            n += 1;
            job.out.set_file_name(format!("{stem} ({tag} {n}).{ext}"));
        }
    }
    jobs
}

fn convert_one(job: &Job, quality: u8) -> Result<Vec<String>> {
    if let Some(parent) = job.out.parent() {
        fs::create_dir_all(parent).context("could not create output folder")?;
    }
    // Write to a temporary file first so a cancelled run never leaves half a file.
    let tmp = part_path(&job.out);

    if job.kind == Kind::Jpeg {
        fs::copy(&job.src, &tmp).context("could not copy file")?;
        fs::rename(&tmp, &job.out).context("could not finalise output file")?;
        return Ok(Vec::new());
    }

    let img = match job.kind {
        Kind::Raw => crate::raw::decode(&job.src)?,
        _ => crate::decode::decode(&job.src)?,
    };
    if img.width > u16::MAX as u32 || img.height > u16::MAX as u32 {
        anyhow::bail!(
            "image is {}×{} pixels; JPEG can't be larger than 65535 pixels on a side",
            img.width,
            img.height
        );
    }
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
                    let _ = fs::remove_file(part_path(&job.out));
                    on_event(Event::Failed { job, error: format!("{e:#}") })
                }
            }
        });
    });
    Ok(())
}

fn part_path(out: &Path) -> PathBuf {
    let mut name = out.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    out.with_file_name(name)
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

//! The desktop window.

use crate::convert::{self, Event, Options};
use eframe::egui;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

const MAX_LOG_LINES: usize = 2000;

pub fn run() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("PSD to JPEG Converter")
            .with_inner_size([640.0, 560.0])
            .with_min_inner_size([480.0, 420.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        "PSD to JPEG Converter",
        options,
        Box::new(|_cc| Ok(Box::new(App::default()))),
    )
}

#[derive(Default)]
struct Progress {
    total: AtomicUsize,
    processed: AtomicUsize,
    converted: AtomicUsize,
    skipped: AtomicUsize,
    failed: AtomicUsize,
    warned: AtomicUsize,
    cancel: AtomicBool,
    finished: AtomicBool,
    status: Mutex<String>,
    log: Mutex<Vec<String>>,
    report: Mutex<Option<PathBuf>>,
}

impl Progress {
    fn log(&self, line: String) {
        let mut log = self.log.lock().unwrap();
        log.push(line);
        if log.len() > MAX_LOG_LINES {
            let excess = log.len() - MAX_LOG_LINES;
            log.drain(..excess);
        }
    }
}

struct App {
    source: Option<PathBuf>,
    dest: Option<PathBuf>,
    options: Options,
    progress: Option<Arc<Progress>>,
    out_root: Option<PathBuf>,
}

impl Default for App {
    fn default() -> Self {
        Self { source: None, dest: None, options: Options::default(), progress: None, out_root: None }
    }
}

impl App {
    fn running(&self) -> bool {
        self.progress.as_ref().is_some_and(|p| !p.finished.load(Ordering::Relaxed))
    }

    fn start(&mut self, ctx: &egui::Context) {
        let (Some(source), Some(dest)) = (self.source.clone(), self.dest.clone()) else { return };
        let out_root = convert::output_root(&source, &dest);
        self.out_root = Some(out_root.clone());
        let progress = Arc::new(Progress::default());
        self.progress = Some(progress.clone());
        let opts = self.options.clone();
        let ctx = ctx.clone();

        std::thread::spawn(move || {
            *progress.status.lock().unwrap() = "Searching for PSD files…".into();
            ctx.request_repaint();
            let jobs = convert::find_jobs(&source, &out_root);
            progress.total.store(jobs.len(), Ordering::Relaxed);
            *progress.status.lock().unwrap() = if jobs.is_empty() {
                "No PSD or PSB files were found in that folder.".into()
            } else {
                format!("Converting {} files…", jobs.len())
            };
            ctx.request_repaint();

            let problems = Mutex::new(Vec::new());
            let rel = |p: &Path| p.strip_prefix(&source).unwrap_or(p).display().to_string();
            let result = convert::run(&jobs, &opts, &progress.cancel, |ev| {
                match ev {
                    Event::Converted { job, warnings } => {
                        progress.converted.fetch_add(1, Ordering::Relaxed);
                        progress.log(format!("✔ {}", rel(&job.src)));
                        if !warnings.is_empty() {
                            progress.warned.fetch_add(1, Ordering::Relaxed);
                        }
                        for w in warnings {
                            let line = format!("WARNING {}: {w}", rel(&job.src));
                            progress.log(format!("⚠ {line}"));
                            problems.lock().unwrap().push(line);
                        }
                    }
                    Event::Skipped { job } => {
                        progress.skipped.fetch_add(1, Ordering::Relaxed);
                        progress.log(format!("↷ {} (already converted)", rel(&job.src)));
                    }
                    Event::Failed { job, error } => {
                        progress.failed.fetch_add(1, Ordering::Relaxed);
                        let line = format!("FAILED {}: {error}", rel(&job.src));
                        progress.log(format!("✖ {line}"));
                        problems.lock().unwrap().push(line);
                    }
                }
                progress.processed.fetch_add(1, Ordering::Relaxed);
                ctx.request_repaint();
            });

            *progress.report.lock().unwrap() = convert::write_report(&out_root, &problems.into_inner().unwrap());
            let mut status = progress.status.lock().unwrap();
            if let Err(e) = result {
                *status = format!("Error: {e}");
            } else if progress.cancel.load(Ordering::Relaxed) {
                *status = "Stopped. Run again with \"Skip files already converted\" ticked to carry on.".into();
            } else if !jobs.is_empty() {
                *status = "Finished.".into();
            }
            drop(status);
            progress.finished.store(true, Ordering::Relaxed);
            ctx.request_repaint();
        });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let running = self.running();

        // Dropping a folder onto the window picks it as the source.
        if !running {
            let dropped = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).find(|p| p.is_dir()));
            if let Some(dir) = dropped {
                self.source = Some(dir);
            }
        }

        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(4.0);
            ui.heading("PSD to JPEG Converter");
            ui.label("Converts every Photoshop file in a folder and all its subfolders, keeping the same folder layout.");
            ui.add_space(10.0);

            ui.add_enabled_ui(!running, |ui| {
                egui::Grid::new("folders").num_columns(3).spacing([10.0, 8.0]).show(ui, |ui| {
                    ui.strong("1. Folder to convert");
                    if ui.button("Choose…").clicked() {
                        if let Some(p) = rfd::FileDialog::new().set_title("Folder to convert").pick_folder() {
                            self.source = Some(p);
                        }
                    }
                    ui.label(path_text(&self.source, "None chosen (or drop a folder here)"));
                    ui.end_row();

                    ui.strong("2. Save the copy in");
                    if ui.button("Choose…").clicked() {
                        if let Some(p) = rfd::FileDialog::new().set_title("Where to save the JPEG copy").pick_folder() {
                            self.dest = Some(p);
                        }
                    }
                    ui.label(path_text(&self.dest, "None chosen"));
                    ui.end_row();
                });

                if let (Some(s), Some(d)) = (&self.source, &self.dest) {
                    ui.add_space(4.0);
                    ui.weak(format!("JPEGs will be saved in: {}", convert::output_root(s, d).display()));
                }

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.label("JPEG quality");
                    ui.add(egui::Slider::new(&mut self.options.quality, 50..=100));
                });
                ui.checkbox(
                    &mut self.options.skip_existing,
                    "Skip files already converted (lets you resume a big batch)",
                );
            });

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if running {
                    if ui.button("Stop").clicked() {
                        if let Some(p) = &self.progress {
                            p.cancel.store(true, Ordering::Relaxed);
                            *p.status.lock().unwrap() = "Stopping after the files in progress…".into();
                        }
                    }
                } else {
                    let ready = self.source.is_some() && self.dest.is_some();
                    let button = egui::Button::new(egui::RichText::new("Convert").strong()).min_size([120.0, 30.0].into());
                    if ui.add_enabled(ready, button).clicked() {
                        self.start(&ctx);
                    }
                    if let (Some(out), Some(p)) = (&self.out_root, &self.progress) {
                        if out.is_dir() && ui.button("Open output folder").clicked() {
                            open_path(out);
                        }
                        if let Some(report) = p.report.lock().unwrap().clone() {
                            if ui.button("Open problem list").clicked() {
                                open_path(&report);
                            }
                        }
                    }
                }
            });

            if let Some(p) = &self.progress {
                ui.add_space(10.0);
                let total = p.total.load(Ordering::Relaxed);
                let processed = p.processed.load(Ordering::Relaxed);
                let frac = if total == 0 { 0.0 } else { processed as f32 / total as f32 };
                ui.add(egui::ProgressBar::new(frac).text(format!("{processed} / {total}")));
                ui.label(p.status.lock().unwrap().as_str());
                ui.label(format!(
                    "Converted {}   ·   Already done {}   ·   Failed {}   ·   With warnings {}",
                    p.converted.load(Ordering::Relaxed),
                    p.skipped.load(Ordering::Relaxed),
                    p.failed.load(Ordering::Relaxed),
                    p.warned.load(Ordering::Relaxed),
                ));
                ui.add_space(6.0);
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
                        for line in p.log.lock().unwrap().iter() {
                            let text = egui::RichText::new(line).monospace();
                            if line.starts_with('✖') {
                                ui.label(text.color(ui.visuals().error_fg_color));
                            } else if line.starts_with('⚠') {
                                ui.label(text.color(ui.visuals().warn_fg_color));
                            } else {
                                ui.label(text);
                            }
                        }
                    });
                });
            }
        });
    }
}

fn path_text(p: &Option<PathBuf>, empty: &str) -> String {
    p.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| empty.to_string())
}

fn open_path(path: &Path) {
    #[cfg(target_os = "windows")]
    let cmd = "explorer";
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let cmd = "xdg-open";
    let _ = std::process::Command::new(cmd).arg(path).spawn();
}

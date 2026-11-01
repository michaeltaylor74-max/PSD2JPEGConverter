//! The desktop window.

use crate::convert::{self, Event, Kind, Options};
use eframe::egui::{
    self, Align, Color32, CornerRadius, FontFamily, FontId, Layout, Margin, RichText, Sense, Stroke, Vec2,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const MAX_LOG_LINES: usize = 2000;
const ICON_PNG: &[u8] = include_bytes!("../assets/icon.png");

// ---- Colours ----
const BG: Color32 = Color32::from_rgb(0x14, 0x16, 0x1b);
const CARD: Color32 = Color32::from_rgb(0x1d, 0x20, 0x27);
const CARD_HI: Color32 = Color32::from_rgb(0x24, 0x28, 0x31);
const BORDER: Color32 = Color32::from_rgb(0x2c, 0x31, 0x3b);
const TEXT: Color32 = Color32::from_rgb(0xe8, 0xea, 0xee);
const MUTED: Color32 = Color32::from_rgb(0x8f, 0x97, 0xa5);
const ACCENT: Color32 = Color32::from_rgb(0x4c, 0x7d, 0xff);
const ACCENT_HI: Color32 = Color32::from_rgb(0x6a, 0x93, 0xff);
const ACCENT_BG: Color32 = Color32::from_rgb(0x1f, 0x2b, 0x4a);
const GREEN: Color32 = Color32::from_rgb(0x3e, 0xc2, 0x8a);
const AMBER: Color32 = Color32::from_rgb(0xf0, 0xb0, 0x40);
const RED: Color32 = Color32::from_rgb(0xf0, 0x60, 0x60);

pub fn run() -> eframe::Result {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("PSD to JPEG Converter")
        .with_inner_size([720.0, 840.0])
        .with_min_inner_size([560.0, 560.0])
        .with_drag_and_drop(true);
    if let Ok(icon) = eframe::icon_data::from_png_bytes(ICON_PNG) {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native("PSD to JPEG Converter", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}

fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("semibold".into()))
}

fn setup_style(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "inter".into(),
        Arc::new(egui::FontData::from_static(include_bytes!("../assets/Inter-Regular.otf"))),
    );
    fonts.font_data.insert(
        "inter-semibold".into(),
        Arc::new(egui::FontData::from_static(include_bytes!("../assets/Inter-SemiBold.otf"))),
    );
    let fallbacks = fonts.families[&FontFamily::Proportional].clone();
    fonts.families.get_mut(&FontFamily::Proportional).unwrap().insert(0, "inter".into());
    let mut bold = vec!["inter-semibold".to_string()];
    bold.extend(fallbacks);
    fonts.families.insert(FontFamily::Name("semibold".into()), bold);
    ctx.set_fonts(fonts);

    let mut style = (*ctx.global_style()).clone();
    use egui::TextStyle::*;
    style.text_styles = [
        (Heading, FontId::new(22.0, FontFamily::Proportional)),
        (Body, FontId::new(14.0, FontFamily::Proportional)),
        (Button, FontId::new(14.0, FontFamily::Proportional)),
        (Small, FontId::new(12.0, FontFamily::Proportional)),
        (Monospace, FontId::new(12.5, FontFamily::Monospace)),
    ]
    .into();
    style.spacing.item_spacing = Vec2::new(8.0, 8.0);
    style.spacing.button_padding = Vec2::new(14.0, 7.0);
    style.spacing.slider_width = 260.0;
    style.spacing.interact_size.y = 26.0;

    let v = &mut style.visuals;
    *v = egui::Visuals::dark();
    v.override_text_color = Some(TEXT);
    v.panel_fill = BG;
    v.window_fill = CARD;
    v.extreme_bg_color = Color32::from_rgb(0x10, 0x12, 0x16);
    v.faint_bg_color = CARD_HI;
    v.selection.bg_fill = ACCENT;
    v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    v.hyperlink_color = ACCENT_HI;
    v.warn_fg_color = AMBER;
    v.error_fg_color = RED;
    let r = CornerRadius::same(7);
    for (w, fill) in [
        (&mut v.widgets.inactive, CARD_HI),
        (&mut v.widgets.hovered, Color32::from_rgb(0x2d, 0x32, 0x3d)),
        (&mut v.widgets.active, Color32::from_rgb(0x34, 0x3a, 0x47)),
        (&mut v.widgets.open, CARD_HI),
    ] {
        w.bg_fill = fill;
        w.weak_bg_fill = fill;
        w.corner_radius = r;
        w.bg_stroke = Stroke::new(1.0, BORDER);
        w.fg_stroke = Stroke::new(1.5, TEXT);
    }
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0x44, 0x4b, 0x58));
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.noninteractive.corner_radius = r;
    ctx.set_global_style(style);
}

// ---------------------------------------------------------------------------

#[derive(Default)]
struct Progress {
    total: AtomicUsize,
    processed: AtomicUsize,
    converted: AtomicUsize,
    skipped: AtomicUsize,
    failed: AtomicUsize,
    warned: AtomicUsize,
    unreadable: AtomicUsize,
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

/// Background search of the chosen folder, so the window can show what it found.
struct Scan {
    path: PathBuf,
    /// The options that affect what's found (subfolders, raw, JPEGs).
    key: (bool, bool, bool),
    psd: AtomicUsize,
    raw: AtomicUsize,
    jpeg: AtomicUsize,
    unreadable: AtomicUsize,
    done: AtomicBool,
    stop: AtomicBool,
}

fn scan_key(o: &Options) -> (bool, bool, bool) {
    (o.include_subfolders, o.convert_raw, o.copy_jpegs)
}

struct App {
    source: Option<PathBuf>,
    dest: Option<PathBuf>,
    options: Options,
    progress: Option<Arc<Progress>>,
    out_root: Option<PathBuf>,
    scan: Option<Arc<Scan>>,
    logo: Option<egui::TextureHandle>,
    show_log: bool,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_style(&cc.egui_ctx);
        let logo = eframe::icon_data::from_png_bytes(ICON_PNG).ok().map(|icon| {
            let img = egui::ColorImage::from_rgba_unmultiplied([icon.width as usize, icon.height as usize], &icon.rgba);
            cc.egui_ctx.load_texture("logo", img, egui::TextureOptions::LINEAR)
        });
        #[allow(unused_mut)]
        let mut app = Self {
            source: None,
            dest: None,
            options: Options::default(),
            progress: None,
            out_root: None,
            scan: None,
            logo,
            show_log: false,
        };
        // Development builds only: preset folders so screenshots can be taken.
        #[cfg(debug_assertions)]
        if let (Ok(s), Ok(d)) = (std::env::var("DEMO_SOURCE"), std::env::var("DEMO_DEST")) {
            app.set_source(s.into(), &cc.egui_ctx);
            app.dest = Some(d.into());
            app.options.flatten = std::env::var("DEMO_FLATTEN").is_ok();
            app.options.convert_raw = std::env::var("DEMO_RAW").is_ok();
            app.options.copy_jpegs = std::env::var("DEMO_JPEG").is_ok();
            app.rescan(&cc.egui_ctx);
            app.show_log = std::env::var("DEMO_LOG").is_ok();
            if std::env::var("DEMO_RUN").is_ok() {
                app.start(&cc.egui_ctx);
            }
        }
        app
    }

    fn running(&self) -> bool {
        self.progress.as_ref().is_some_and(|p| !p.finished.load(Ordering::Relaxed))
    }

    fn set_source(&mut self, dir: PathBuf, ctx: &egui::Context) {
        self.source = Some(dir);
        self.progress = None;
        self.rescan(ctx);
    }

    /// Count what's in the chosen folder (again), e.g. after an option changes.
    fn rescan(&mut self, ctx: &egui::Context) {
        if let Some(old) = &self.scan {
            old.stop.store(true, Ordering::Relaxed);
        }
        let Some(dir) = self.source.clone() else { return };
        let scan = Arc::new(Scan {
            path: dir.clone(),
            key: scan_key(&self.options),
            psd: AtomicUsize::new(0),
            raw: AtomicUsize::new(0),
            jpeg: AtomicUsize::new(0),
            unreadable: AtomicUsize::new(0),
            done: AtomicBool::new(false),
            stop: AtomicBool::new(false),
        });
        self.scan = Some(scan.clone());
        let opts = self.options.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = convert::scan(&dir, None, &opts, &scan.stop, |kind| {
                let counter = match kind {
                    Kind::Photoshop => &scan.psd,
                    Kind::Raw => &scan.raw,
                    Kind::Jpeg => &scan.jpeg,
                };
                counter.fetch_add(1, Ordering::Relaxed);
            });
            scan.unreadable.store(result.problems.len(), Ordering::Relaxed);
            scan.done.store(true, Ordering::Relaxed);
            ctx.request_repaint();
        });
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
            *progress.status.lock().unwrap() = "Searching for Photoshop files…".into();
            ctx.request_repaint();
            let found = convert::scan(&source, Some(&out_root), &opts, &progress.cancel, |_| {});
            for p in &found.problems {
                progress.log(format!("⚠  {p}"));
            }
            progress.unreadable.store(found.problems.len(), Ordering::Relaxed);
            let jobs = convert::find_jobs(&source, &found.files, &out_root, opts.flatten);
            progress.total.store(jobs.len(), Ordering::Relaxed);
            *progress.status.lock().unwrap() = if jobs.is_empty() {
                "Nothing to convert was found in that folder.".into()
            } else {
                format!("Converting {} files…", fmt_num(jobs.len()))
            };
            ctx.request_repaint();

            let problems = Mutex::new(found.problems);
            let rel = |p: &Path| p.strip_prefix(&source).unwrap_or(p).display().to_string();
            let result = convert::run(&jobs, &opts, &progress.cancel, |ev| {
                match ev {
                    Event::Converted { job, warnings } => {
                        progress.converted.fetch_add(1, Ordering::Relaxed);
                        let verb = if job.kind == Kind::Jpeg { "copied" } else { "converted" };
                        progress.log(format!("✔  {}  ({verb})", rel(&job.src)));
                        if !warnings.is_empty() {
                            progress.warned.fetch_add(1, Ordering::Relaxed);
                        }
                        for w in warnings {
                            let line = format!("WARNING {}: {w}", rel(&job.src));
                            progress.log(format!("⚠  {line}"));
                            problems.lock().unwrap().push(line);
                        }
                    }
                    Event::Skipped { job } => {
                        progress.skipped.fetch_add(1, Ordering::Relaxed);
                        progress.log(format!("↷  {} (already converted)", rel(&job.src)));
                    }
                    Event::Failed { job, error } => {
                        progress.failed.fetch_add(1, Ordering::Relaxed);
                        let line = format!("FAILED {}: {error}", rel(&job.src));
                        progress.log(format!("✖  {line}"));
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
                *status = "Stopped. Convert again with \"Skip files already converted\" on to carry on.".into();
            } else if !jobs.is_empty() {
                let bad = progress.unreadable.load(Ordering::Relaxed);
                *status = if bad > 0 {
                    format!("Done, but {bad} folder(s) couldn't be opened. See the problem list.")
                } else {
                    "All done.".into()
                };
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

        let hovering_files = ctx.input(|i| !i.raw.hovered_files.is_empty());
        if !running {
            let dropped = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).find(|p| p.is_dir()));
            if let Some(dir) = dropped {
                self.set_source(dir, &ctx);
            }
        }
        if !running && self.source.is_some() && self.scan.as_ref().is_some_and(|s| s.key != scan_key(&self.options)) {
            self.rescan(&ctx);
        }
        if self.scan.as_ref().is_some_and(|s| !s.done.load(Ordering::Relaxed)) {
            ctx.request_repaint_after(Duration::from_millis(150));
        }

        // The Convert button and progress stay pinned to the bottom of the window.
        egui::Panel::bottom("actions")
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(Color32::from_rgb(0x18, 0x1a, 0x20))
                    .stroke(Stroke::new(1.0, BORDER))
                    .inner_margin(Margin { left: 24, right: 24, top: 16, bottom: 18 }),
            )
            .show(ui, |ui| self.action_area(ui, running));

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG).inner_margin(Margin { left: 24, right: 24, top: 20, bottom: 12 }))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    self.header(ui);
                    ui.add_space(14.0);
                    ui.add_enabled_ui(!running, |ui| {
                        self.source_card(ui, hovering_files);
                        ui.add_space(12.0);
                        self.dest_card(ui);
                        ui.add_space(12.0);
                        self.options_card(ui);
                    });
                });
            });
    }
}

impl App {
    fn header(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if let Some(logo) = &self.logo {
                ui.add(egui::Image::new(logo).fit_to_exact_size(Vec2::splat(44.0)));
            }
            ui.add_space(4.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label(RichText::new("PSD to JPEG Converter").font(semibold(21.0)));
                ui.label(RichText::new("Batch convert Photoshop and camera RAW files, including every subfolder").color(MUTED));
            });
        });
    }

    fn source_card(&mut self, ui: &mut egui::Ui, hovering: bool) {
        let ctx = ui.ctx().clone();
        let mut browse = false;
        card(ui, "FOLDER TO CONVERT", |ui| {
            let (fill, stroke) = if hovering {
                (ACCENT_BG, Stroke::new(1.5, ACCENT))
            } else {
                (CARD_HI, Stroke::new(1.0, BORDER))
            };
            egui::Frame::new()
                .fill(fill)
                .stroke(stroke)
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::symmetric(14, 12))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    browse = row_with_button(ui, folder_icon, |ui| match &self.source {
                        Some(p) => {
                            ui.add(egui::Label::new(RichText::new(folder_name(p)).font(semibold(15.0))).truncate());
                            ui.add(egui::Label::new(RichText::new(p.display().to_string()).small().color(MUTED)).truncate());
                            if let Some(scan) = self.scan.as_ref().filter(|s| &s.path == p) {
                                scan_summary(ui, scan, &self.options);
                            }
                        }
                        None => {
                            let msg = if hovering { "Drop the folder here" } else { "Drag a folder here, or click Browse" };
                            ui.label(RichText::new(msg).font(semibold(15.0)));
                            ui.label(RichText::new("Every subfolder, at every level, is searched").small().color(MUTED));
                        }
                    });
                });
        });
        if browse {
            if let Some(p) = rfd::FileDialog::new().set_title("Folder to convert").pick_folder() {
                self.set_source(p, &ctx);
            }
        }
    }

    fn dest_card(&mut self, ui: &mut egui::Ui) {
        let mut browse = false;
        card(ui, "SAVE JPEGS TO", |ui| {
            browse = row_with_button(ui, save_icon, |ui| {
                match &self.dest {
                    Some(p) => {
                        ui.add(egui::Label::new(RichText::new(folder_name(p)).font(semibold(15.0))).truncate());
                        ui.add(egui::Label::new(RichText::new(p.display().to_string()).small().color(MUTED)).truncate());
                    }
                    None => {
                        ui.label(RichText::new("Choose where the converted files should go").font(semibold(15.0)));
                        ui.label(RichText::new("Your original files are never changed").small().color(MUTED));
                    }
                }
                if let (Some(s), Some(d)) = (&self.source, &self.dest) {
                    let out = convert::output_root(s, d);
                    ui.add(
                        egui::Label::new(
                            RichText::new(format!("A new folder named \"{}\" will be created here", folder_name(&out)))
                                .small()
                                .color(ACCENT_HI),
                        )
                        .truncate(),
                    );
                }
            });
        });
        if browse {
            if let Some(p) = rfd::FileDialog::new().set_title("Where to save the JPEGs").pick_folder() {
                self.dest = Some(p);
            }
        }
    }

    fn options_card(&mut self, ui: &mut egui::Ui) {
        card(ui, "OPTIONS", |ui| {
            ui.label(RichText::new("What to include").font(semibold(14.0)));
            ui.add_space(-2.0);
            option_check(
                ui,
                &mut self.options.include_subfolders,
                "Search all subfolders",
                "Goes through every folder inside, however deep. Untick to use only the files directly in the chosen folder.",
            );
            option_check(
                ui,
                &mut self.options.convert_raw,
                "Also convert camera RAW files",
                "Sony .ARW, plus Canon .CR2/.CR3, Nikon .NEF, Fujifilm .RAF, .DNG and other raw formats.",
            );
            option_check(
                ui,
                &mut self.options.copy_jpegs,
                "Also copy JPEGs already in the folder",
                "Existing .jpg files are copied across unchanged, so everything ends up together.",
            );

            ui.add_space(10.0);
            ui.label(RichText::new("Folder layout").font(semibold(14.0)));
            ui.columns(2, |cols| {
                choice_tile(
                    &mut cols[0],
                    &mut self.options.flatten,
                    false,
                    "Keep folder structure",
                    "Subfolders are recreated, so every JPEG sits where its original was.",
                );
                choice_tile(
                    &mut cols[1],
                    &mut self.options.flatten,
                    true,
                    "All in one folder",
                    "Every JPEG goes into a single folder. Matching names get their folder added.",
                );
            });

            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("JPEG quality").font(semibold(14.0)));
                ui.add_space(8.0);
                ui.add(egui::Slider::new(&mut self.options.quality, 50..=100).show_value(false).trailing_fill(true));
                let desc = match self.options.quality {
                    98..=100 => "Maximum",
                    90..=97 => "High",
                    80..=89 => "Good",
                    _ => "Smaller files",
                };
                ui.label(RichText::new(format!("{}  ·  {desc}", self.options.quality)).color(MUTED));
            });

            ui.add_space(4.0);
            ui.checkbox(&mut self.options.skip_existing, "Skip files already converted  (lets you resume a big batch)");
        });
    }

    fn action_area(&mut self, ui: &mut egui::Ui, running: bool) {
        let ctx = ui.ctx().clone();
        let ready = self.source.is_some() && self.dest.is_some();

        if !running {
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 46.0), Sense::click());
            let fill = match (ready, resp.hovered()) {
                (false, _) => Color32::from_rgb(0x2a, 0x33, 0x4d),
                (true, true) => ACCENT_HI,
                (true, false) => ACCENT,
            };
            ui.painter().rect_filled(rect, CornerRadius::same(9), fill);
            let text_colour = if ready { Color32::WHITE } else { Color32::from_rgb(0x7a, 0x84, 0x9c) };
            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "Convert to JPEG", semibold(16.0), text_colour);
            if ready && resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if ready && resp.clicked() {
                self.start(&ctx);
            }
            if !ready {
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("Choose a folder to convert and where to save the JPEGs").small().color(MUTED));
                });
            }
        }

        let Some(p) = self.progress.clone() else { return };
        ui.add_space(4.0);
        card(ui, if running { "CONVERTING" } else { "RESULTS" }, |ui| {
            let total = p.total.load(Ordering::Relaxed);
            let processed = p.processed.load(Ordering::Relaxed);
            let frac = if total == 0 { if running { 0.0 } else { 1.0 } } else { processed as f32 / total as f32 };
            let failed = p.failed.load(Ordering::Relaxed);
            let bar_colour = if !running && failed == 0 && total > 0 { GREEN } else { ACCENT };

            ui.horizontal(|ui| {
                ui.label(RichText::new(p.status.lock().unwrap().as_str()).font(semibold(14.0)));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!("{} of {}  ·  {:.0}%", fmt_num(processed), fmt_num(total), frac * 100.0))
                            .color(MUTED),
                    );
                });
            });
            ui.add(
                egui::ProgressBar::new(frac)
                    .fill(bar_colour)
                    .desired_height(10.0)
                    .corner_radius(CornerRadius::same(5))
                    .animate(running),
            );
            ui.add_space(6.0);

            ui.columns(4, |cols| {
                stat(&mut cols[0], "Converted", p.converted.load(Ordering::Relaxed), GREEN);
                stat(&mut cols[1], "Already done", p.skipped.load(Ordering::Relaxed), ACCENT_HI);
                stat(&mut cols[2], "Warnings", p.warned.load(Ordering::Relaxed), AMBER);
                stat(&mut cols[3], "Failed", failed, RED);
            });
            ui.add_space(6.0);

            ui.horizontal(|ui| {
                if running {
                    if ui.button("■  Stop").clicked() {
                        p.cancel.store(true, Ordering::Relaxed);
                        *p.status.lock().unwrap() = "Stopping after the files in progress…".into();
                    }
                } else if let Some(out) = &self.out_root {
                    if out.is_dir() && ui.button("Open output folder").clicked() {
                        open_path(out);
                    }
                    if let Some(report) = p.report.lock().unwrap().clone() {
                        if ui.button("⚠  Open problem list").clicked() {
                            open_path(&report);
                        }
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let label = if self.show_log { "Hide details" } else { "Show details" };
                    if ui.add(egui::Button::new(RichText::new(label).color(MUTED)).frame(false)).clicked() {
                        self.show_log = !self.show_log;
                    }
                });
            });

            if self.show_log {
                egui::Frame::new()
                    .fill(Color32::from_rgb(0x10, 0x12, 0x16))
                    .corner_radius(CornerRadius::same(7))
                    .inner_margin(Margin::same(10))
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .max_height(150.0)
                            .stick_to_bottom(true)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                for line in p.log.lock().unwrap().iter() {
                                    let colour = match line.chars().next() {
                                        Some('✖') => RED,
                                        Some('⚠') => AMBER,
                                        Some('↷') => MUTED,
                                        _ => TEXT,
                                    };
                                    ui.label(RichText::new(line).monospace().color(colour));
                                }
                            });
                    });
            }
        });
    }
}

// ---- Small widgets ----

/// Icon on the left, text in the middle (truncated to fit), Browse button on
/// the right. Returns true when Browse was clicked.
fn row_with_button(
    ui: &mut egui::Ui,
    icon: fn(&mut egui::Ui),
    text: impl FnOnce(&mut egui::Ui),
) -> bool {
    let mut clicked = false;
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            clicked = ui.button("Browse…").clicked();
            ui.add_space(8.0);
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                icon(ui);
                ui.add_space(6.0);
                ui.vertical(|ui| {
                    ui.set_max_width(ui.available_width());
                    ui.spacing_mut().item_spacing.y = 3.0;
                    text(ui);
                });
            });
        });
    });
    clicked
}

fn folder_icon(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(30.0, 26.0), Sense::hover());
    let p = ui.painter();
    let tab = egui::Rect::from_min_size(rect.min + Vec2::new(1.0, 2.0), Vec2::new(12.0, 6.0));
    p.rect_filled(tab, CornerRadius::same(2), Color32::from_rgb(0x3d, 0x63, 0xd6));
    let body = egui::Rect::from_min_max(rect.min + Vec2::new(1.0, 6.0), rect.max - Vec2::new(1.0, 1.0));
    p.rect_filled(body, CornerRadius::same(3), ACCENT);
    let front = egui::Rect::from_min_max(body.min + Vec2::new(0.0, 4.0), body.max);
    p.rect_filled(front, CornerRadius::same(3), ACCENT_HI);
}

fn save_icon(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(30.0, 26.0), Sense::hover());
    let p = ui.painter();
    let tray = egui::Rect::from_min_max(egui::pos2(rect.left() + 2.0, rect.bottom() - 9.0), rect.max - Vec2::new(2.0, 1.0));
    p.rect_filled(tray, CornerRadius::same(3), ACCENT);
    let c = rect.center().x;
    let stroke = Stroke::new(3.0, ACCENT_HI);
    p.line_segment([egui::pos2(c, rect.top() + 1.0), egui::pos2(c, rect.bottom() - 11.0)], stroke);
    p.line_segment([egui::pos2(c - 6.0, rect.bottom() - 17.0), egui::pos2(c, rect.bottom() - 11.0)], stroke);
    p.line_segment([egui::pos2(c + 6.0, rect.bottom() - 17.0), egui::pos2(c, rect.bottom() - 11.0)], stroke);
}

fn card(ui: &mut egui::Ui, title: &str, add_contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(CARD)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(title).font(semibold(11.5)).color(MUTED).extra_letter_spacing(1.0));
            ui.add_space(2.0);
            add_contents(ui);
        });
}

fn choice_tile(ui: &mut egui::Ui, value: &mut bool, this: bool, title: &str, desc: &str) {
    let selected = *value == this;
    let enabled = ui.is_enabled();
    let resp = egui::Frame::new()
        .fill(if selected { ACCENT_BG } else { CARD_HI })
        .stroke(Stroke::new(if selected { 1.5 } else { 1.0 }, if selected { ACCENT } else { BORDER }))
        .corner_radius(CornerRadius::same(9))
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height(64.0);
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
                let c = rect.center();
                ui.painter().circle_stroke(c, 7.0, Stroke::new(1.5, if selected { ACCENT_HI } else { MUTED }));
                if selected {
                    ui.painter().circle_filled(c, 4.0, ACCENT_HI);
                }
                ui.label(RichText::new(title).font(semibold(14.0)));
            });
            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                ui.add(egui::Label::new(RichText::new(desc).small().color(MUTED)).wrap());
            });
        })
        .response
        .interact(Sense::click());
    if enabled && resp.clicked() {
        *value = this;
    }
    if enabled && resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
}

/// A tick box with a bold label and a grey explanation underneath.
fn option_check(ui: &mut egui::Ui, value: &mut bool, label: &str, desc: &str) {
    ui.add_space(2.0);
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 1.0;
        ui.checkbox(value, RichText::new(label).font(semibold(13.5)));
        ui.horizontal(|ui| {
            ui.add_space(26.0);
            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                ui.add(egui::Label::new(RichText::new(desc).small().color(MUTED)).wrap());
            });
        });
    });
}

/// "1,240 Photoshop · 320 RAW · 85 JPEG found" under the chosen folder.
fn scan_summary(ui: &mut egui::Ui, scan: &Scan, opts: &Options) {
    let done = scan.done.load(Ordering::Relaxed);
    let (psd, raw, jpeg) = (
        scan.psd.load(Ordering::Relaxed),
        scan.raw.load(Ordering::Relaxed),
        scan.jpeg.load(Ordering::Relaxed),
    );
    let mut parts = vec![format!("{} Photoshop", fmt_num(psd))];
    if opts.convert_raw {
        parts.push(format!("{} RAW", fmt_num(raw)));
    }
    if opts.copy_jpegs {
        parts.push(format!("{} JPEG", fmt_num(jpeg)));
    }
    let total = psd + raw + jpeg;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if !done {
            ui.label(RichText::new(format!("Searching… {} so far", parts.join("  ·  "))).small().color(MUTED));
        } else if total == 0 {
            ui.label(RichText::new("Nothing to convert found here").small().color(AMBER));
        } else {
            ui.label(RichText::new(format!("{} files found", parts.join("  ·  "))).small().color(GREEN));
        }
        let bad = scan.unreadable.load(Ordering::Relaxed);
        if done && bad > 0 {
            let s = if bad == 1 { "" } else { "s" };
            ui.label(RichText::new(format!("·  {bad} folder{s} couldn't be opened")).small().color(AMBER))
                .on_hover_text("They're listed in the problem list after converting.");
        }
    });
}

fn stat(ui: &mut egui::Ui, label: &str, n: usize, colour: Color32) {
    egui::Frame::new()
        .fill(CARD_HI)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 1.0;
            ui.label(RichText::new(fmt_num(n)).font(semibold(19.0)).color(if n == 0 { MUTED } else { colour }));
            ui.label(RichText::new(label).small().color(MUTED));
        });
}

// ---- Helpers ----

fn folder_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string())
}

fn fmt_num(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
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

// Hide the console window on Windows when launched by double-click.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod convert;
mod decode;
mod gui;

use clap::Parser;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

/// Batch convert Photoshop PSD/PSB files to JPEG, including every subfolder.
///
/// Run with no arguments to open the window. With arguments, it runs in the
/// terminal: the converted copy goes to <DEST>/<name of SOURCE folder>/...
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Folder to search for PSD/PSB files (subfolders included)
    source: PathBuf,
    /// Folder where the converted copy will be created
    dest: PathBuf,
    /// JPEG quality, 1-100
    #[arg(short, long, default_value_t = 92, value_parser = clap::value_parser!(u8).range(1..=100))]
    quality: u8,
    /// Re-convert files even if the JPEG already exists
    #[arg(long)]
    overwrite: bool,
    /// Put every JPEG into one folder instead of copying the subfolders
    #[arg(long)]
    flatten: bool,
    /// Files to convert at once (0 = one per CPU core)
    #[arg(short, long, default_value_t = 0)]
    threads: usize,
}

fn main() {
    // Problems inside individual files are caught and reported per file, so
    // keep the default panic message out of the terminal.
    std::panic::set_hook(Box::new(|_| {}));
    if std::env::args_os().len() > 1 {
        attach_console();
        std::process::exit(run_cli(Cli::parse()));
    }
    if let Err(e) = gui::run() {
        eprintln!("Could not open the window: {e}");
        std::process::exit(1);
    }
}

fn run_cli(cli: Cli) -> i32 {
    if !cli.source.is_dir() {
        eprintln!("Source folder not found: {}", cli.source.display());
        return 2;
    }
    let out_root = convert::output_root(&cli.source, &cli.dest);
    let jobs = convert::find_jobs(&cli.source, &out_root, cli.flatten);
    if jobs.is_empty() {
        println!("No PSD or PSB files found in {}", cli.source.display());
        return 0;
    }
    println!("Found {} files. Saving to {}", jobs.len(), out_root.display());

    let opts = convert::Options { quality: cli.quality, skip_existing: !cli.overwrite, threads: cli.threads, flatten: cli.flatten };
    let done = AtomicUsize::new(0);
    let failed = AtomicUsize::new(0);
    let problems = Mutex::new(Vec::new());
    let total = jobs.len();
    let cancel = AtomicBool::new(false);

    let result = convert::run(&jobs, &opts, &cancel, |ev| {
        let n = done.fetch_add(1, Ordering::Relaxed) + 1;
        let rel = |p: &std::path::Path| p.strip_prefix(&cli.source).unwrap_or(p).display().to_string();
        match ev {
            convert::Event::Converted { job, warnings } => {
                println!("[{n}/{total}] {}", rel(&job.src));
                for w in warnings {
                    let line = format!("WARNING {}: {w}", rel(&job.src));
                    println!("    {line}");
                    problems.lock().unwrap().push(line);
                }
            }
            convert::Event::Skipped { job } => println!("[{n}/{total}] {} (already done)", rel(&job.src)),
            convert::Event::Failed { job, error } => {
                failed.fetch_add(1, Ordering::Relaxed);
                let line = format!("FAILED {}: {error}", rel(&job.src));
                eprintln!("[{n}/{total}] {line}");
                problems.lock().unwrap().push(line);
            }
        }
    });
    if let Err(e) = result {
        eprintln!("Error: {e}");
        return 1;
    }
    let failed = failed.load(Ordering::Relaxed);
    if let Some(p) = convert::write_report(&out_root, &problems.into_inner().unwrap()) {
        println!("Problems listed in {}", p.display());
    }
    println!("Finished: {} of {total} files OK.", total - failed);
    if failed > 0 { 1 } else { 0 }
}

/// On Windows the app is a GUI program, so reconnect to the terminal for CLI use.
#[cfg(windows)]
fn attach_console() {
    use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}
#[cfg(not(windows))]
fn attach_console() {}

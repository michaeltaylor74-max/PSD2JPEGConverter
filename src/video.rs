//! Camcorder video (AVCHD .MTS etc.) re-saved so DaVinci Resolve can use it.
//!
//! Camcorders usually record the sound as Dolby Digital (AC-3), which Resolve
//! can't decode, so the clips import silently. The picture is copied across
//! untouched and only the sound is turned into uncompressed PCM, inside a .mov.
//! The work is done by ffmpeg, which needs to be installed.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Video files from camcorders: AVCHD (.MTS/.M2TS) and older JVC/Panasonic (.MOD/.TOD).
pub const VIDEO_EXTS: &[&str] = &["mts", "m2ts", "mod", "tod"];

#[cfg(windows)]
const EXE: &str = "ffmpeg.exe";
#[cfg(not(windows))]
const EXE: &str = "ffmpeg";

/// Find ffmpeg: next to this program, on the PATH, or where the usual
/// installers put it (so it's found straight after installing, without a restart).
pub fn find_ffmpeg() -> Option<PathBuf> {
    let mut places: Vec<PathBuf> = Vec::new();
    if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) {
        places.push(dir.join(EXE));
    }
    if let Some(path) = std::env::var_os("PATH") {
        places.extend(std::env::split_paths(&path).map(|d| d.join(EXE)));
    }
    #[cfg(windows)]
    {
        if let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
            let winget = local.join("Microsoft").join("WinGet");
            places.push(winget.join("Links").join(EXE));
            // winget install Gyan.FFmpeg → Packages\Gyan.FFmpeg_…\ffmpeg-x.y-full_build\bin\ffmpeg.exe
            if let Ok(pkgs) = std::fs::read_dir(winget.join("Packages")) {
                for pkg in pkgs.flatten().filter(|e| e.file_name().to_string_lossy().to_lowercase().contains("ffmpeg")) {
                    if let Ok(inner) = std::fs::read_dir(pkg.path()) {
                        places.extend(inner.flatten().map(|e| e.path().join("bin").join(EXE)));
                    }
                }
            }
        }
        for root in ["C:\\ffmpeg", "C:\\Program Files\\ffmpeg", "C:\\ProgramData\\chocolatey"] {
            places.push(Path::new(root).join("bin").join(EXE));
        }
    }
    #[cfg(not(windows))]
    places.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].iter().map(|d| Path::new(d).join(EXE)));

    places.into_iter().find(|p| p.is_file())
}

/// Windows only: open a terminal window that installs ffmpeg with winget.
#[cfg(windows)]
pub fn install_ffmpeg() {
    let _ = Command::new("cmd")
        .args([
            "/C",
            "start",
            "Installing ffmpeg",
            "cmd",
            "/K",
            "winget install --id Gyan.FFmpeg -e --accept-source-agreements --accept-package-agreements && echo. && echo ffmpeg is installed. You can close this window and go back to the converter.",
        ])
        .spawn();
}

pub fn missing_message() -> &'static str {
    if cfg!(windows) {
        "ffmpeg is needed to convert video but isn't installed. Use the \"Install ffmpeg\" button, or open Command Prompt and run: winget install Gyan.FFmpeg"
    } else if cfg!(target_os = "macos") {
        "ffmpeg is needed to convert video but isn't installed. Install it with Homebrew: brew install ffmpeg"
    } else {
        "ffmpeg is needed to convert video but isn't installed. Install the ffmpeg package for your system."
    }
}

/// Copy the picture as is, convert the sound to PCM, and save as a QuickTime
/// .mov at `out` (any file name; the format is set explicitly).
pub fn rewrap_for_resolve(src: &Path, out: &Path) -> Result<Vec<String>> {
    let ffmpeg = find_ffmpeg().ok_or_else(|| anyhow::anyhow!(missing_message()))?;
    let mut cmd = Command::new(&ffmpeg);
    cmd.args(["-hide_banner", "-nostdin", "-loglevel", "error", "-y", "-fflags", "+genpts", "-i"])
        .arg(src)
        .args(["-map", "0:v:0", "-map", "0:a?", "-c:v", "copy", "-c:a", "pcm_s16le", "-f", "mov"])
        .arg(out)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let output = cmd.output().context("could not start ffmpeg")?;
    let messages: Vec<String> = String::from_utf8_lossy(&output.stderr)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect();
    if !output.status.success() {
        let tail = messages.iter().rev().take(3).rev().cloned().collect::<Vec<_>>().join(" | ");
        bail!("ffmpeg couldn't convert this video: {}", if tail.is_empty() { "no details given".into() } else { tail });
    }
    if std::fs::metadata(out).map(|m| m.len()).unwrap_or(0) == 0 {
        bail!("ffmpeg finished but produced an empty file");
    }
    // Minor stream glitches are normal in camcorder files; report but don't fail.
    Ok(if messages.is_empty() { Vec::new() } else { vec![format!("ffmpeg said: {}", messages.iter().take(3).cloned().collect::<Vec<_>>().join(" | "))] })
}

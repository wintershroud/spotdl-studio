use crate::engine::Engines;
use regex::Regex;
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::LazyLock,
};
use tauri::{AppHandle, Emitter};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, BufReader},
    process::{Child, Command},
    sync::oneshot,
};

pub const FORMATS: [&str; 6] = ["mp3", "m4a", "flac", "opus", "ogg", "wav"];
pub const BITRATES: [&str; 5] = ["320k", "256k", "192k", "128k", "auto"];

pub struct Job {
    pub query: String,
    pub output_dir: PathBuf,
    pub format: String,
    pub bitrate: String,
    pub naming: String,
}

#[derive(Serialize, Clone, Default)]
pub struct Summary {
    pub code: i32,
    pub saved: u32,
    pub skipped: u32,
    pub failed: u32,
    pub cancelled: bool,
}

/// Several links pasted together become separate spotDL queries; anything
/// else is one search phrase.
pub fn split_queries(input: &str) -> Vec<String> {
    let parts: Vec<&str> = input.split_whitespace().collect();
    if parts.len() > 1 && parts.iter().all(|p| p.starts_with("http")) {
        parts.into_iter().map(str::to_owned).collect()
    } else {
        vec![input.trim().to_owned()]
    }
}

pub fn spotdl_args(job: &Job, engines: &Engines, errors_file: &Path) -> Vec<String> {
    let name = if job.naming == "title" {
        "{title}.{output-ext}"
    } else {
        "{artists} - {title}.{output-ext}"
    };
    let mut args: Vec<String> = vec!["download".into()];
    args.extend(split_queries(&job.query));
    args.extend([
        "--output".into(),
        job.output_dir.join(name).to_string_lossy().into_owned(),
        "--format".into(),
        job.format.clone(),
        "--bitrate".into(),
        job.bitrate.clone(),
        "--ffmpeg".into(),
        engines.ffmpeg.to_string_lossy().into_owned(),
        "--simple-tui".into(),
        "--log-level".into(),
        "INFO".into(),
        "--save-errors".into(),
        errors_file.to_string_lossy().into_owned(),
    ]);
    args
}

fn emit_log(app: &AppHandle, text: impl Into<String>) {
    let _ = app.emit("log", text.into());
}

static ANSI: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\x1b\[[0-9;?]*[A-Za-z]").unwrap());

#[derive(PartialEq, Debug)]
enum Outcome {
    Saved,
    Skipped,
}

fn classify(line: &str) -> Option<Outcome> {
    if line.starts_with("Downloaded \"") {
        Some(Outcome::Saved)
    } else if line.starts_with("Skipping ") && line.contains("file already exists") {
        Some(Outcome::Skipped)
    } else {
        None
    }
}

async fn pump(
    app: AppHandle,
    stream: impl AsyncRead + Unpin,
) -> (u32, u32) {
    let (mut saved, mut skipped) = (0, 0);
    let mut lines = BufReader::new(stream).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        // --simple-tui redraws progress with carriage returns; keep the last frame.
        let line = ANSI.replace_all(&line, "");
        let line = line.rsplit('\r').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        match classify(line) {
            Some(Outcome::Saved) => saved += 1,
            Some(Outcome::Skipped) => skipped += 1,
            None => {}
        }
        emit_log(&app, line);
    }
    (saved, skipped)
}

pub async fn run(app: AppHandle, job: Job, engines: Engines, cancel: oneshot::Receiver<()>) -> Summary {
    let data = crate::paths::data_dir();
    let home = data.join("spotdl-home");
    let _ = std::fs::create_dir_all(&home);
    let errors_file = data.join("last-errors.txt");
    let _ = std::fs::remove_file(&errors_file);
    let args = spotdl_args(&job, &engines, &errors_file);

    let mut cmd = Command::new(&engines.spotdl);
    cmd.args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        // spotDL keeps its cache and config in ~/.spotdl; point "home" at the
        // data folder so nothing lands in the real home folder.
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        // Rich wraps at 80 columns when not on a terminal, which splits lines.
        .env("COLUMNS", "1000")
        .env("PYTHONUTF8", "1")
        .env("PYTHONIOENCODING", "utf-8");
    if let Some(bin) = &engines.bin_dir {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let joined = std::env::join_paths(std::iter::once(bin.clone()).chain(std::env::split_paths(&path)))
            .unwrap_or(path);
        cmd.env("PATH", joined);
    }
    #[cfg(unix)]
    cmd.process_group(0);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            emit_log(&app, format!("Could not start spotDL: {e}"));
            return Summary { code: 1, ..Default::default() };
        }
    };

    let out = tokio::spawn(pump(app.clone(), child.stdout.take().unwrap()));
    let err = tokio::spawn(pump(app.clone(), child.stderr.take().unwrap()));

    let (code, cancelled) = tokio::select! {
        status = child.wait() => (status.ok().and_then(|s| s.code()).unwrap_or(1), false),
        _ = cancel => {
            kill_tree(&mut child).await;
            (1, true)
        }
    };

    let (a, b) = (out.await.unwrap_or_default(), err.await.unwrap_or_default());
    Summary {
        code,
        saved: a.0 + b.0,
        skipped: a.1 + b.1,
        failed: count_errors(&std::fs::read_to_string(&errors_file).unwrap_or_default()),
        cancelled,
    }
}

/// spotDL's --save-errors file is a timestamp line followed by one line per failed song.
fn count_errors(text: &str) -> u32 {
    static STAMP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{4}-\d{2}-\d{2}-\d{2}-\d{2}-\d{2}$").unwrap());
    text.lines().map(str::trim).filter(|l| !l.is_empty() && !STAMP.is_match(l)).count() as u32
}

/// spotDL is a PyInstaller bundle that starts its own child process, which in
/// turn runs ffmpeg, so killing only the direct child would leave work running.
async fn kill_tree(child: &mut Child) {
    if let Some(pid) = child.id() {
        #[cfg(unix)]
        let _ = Command::new("kill").args(["-TERM", "--", &format!("-{pid}")]).status().await;
        #[cfg(windows)]
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .creation_flags(0x0800_0000)
            .status()
            .await;
    }
    if tokio::time::timeout(std::time::Duration::from_secs(3), child.wait()).await.is_err() {
        let _ = child.kill().await;
    }
}

pub fn validate(job: &Job) -> Result<(), String> {
    if job.query.trim().is_empty() {
        return Err("Paste a Spotify link or type a song name.".into());
    }
    if job.query.trim_start().starts_with('-') {
        return Err("A search can't start with a dash.".into());
    }
    if !FORMATS.contains(&job.format.as_str()) {
        return Err(format!("Unknown format {}.", job.format));
    }
    if !BITRATES.contains(&job.bitrate.as_str()) {
        return Err(format!("Unknown bitrate {}.", job.bitrate));
    }
    if !Path::new(&job.output_dir).is_absolute() {
        return Err("Choose a full save folder path.".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(query: &str) -> Job {
        Job {
            query: query.into(),
            output_dir: std::env::temp_dir(),
            format: "mp3".into(),
            bitrate: "320k".into(),
            naming: "artist_title".into(),
        }
    }

    #[test]
    fn multiple_links_split_but_searches_stay_whole() {
        assert_eq!(split_queries("https://a https://b\nhttps://c").len(), 3);
        assert_eq!(split_queries("  Daft Punk - One More Time "), vec!["Daft Punk - One More Time"]);
        assert_eq!(split_queries("https://a and more").len(), 1);
    }

    #[test]
    fn args_carry_template_and_engine_paths() {
        let engines = Engines {
            spotdl: "spotdl".into(),
            ffmpeg: "/x/ffmpeg".into(),
            bin_dir: None,
        };
        let args = spotdl_args(&job("song"), &engines, Path::new("/x/errors.txt"));
        assert_eq!(&args[..2], ["download", "song"]);
        let out = &args[args.iter().position(|a| a == "--output").unwrap() + 1];
        assert!(out.ends_with("{artists} - {title}.{output-ext}"));
        assert!(args.windows(2).any(|w| w == ["--ffmpeg", "/x/ffmpeg"]));
    }

    #[test]
    fn rejects_option_looking_queries_and_bad_choices() {
        assert!(validate(&job("--help")).is_err());
        assert!(validate(&job("")).is_err());
        let mut j = job("ok");
        j.format = "exe".into();
        assert!(validate(&j).is_err());
        assert!(validate(&job("ok")).is_ok());
    }

    #[test]
    fn classifies_spotdl_result_lines() {
        assert_eq!(classify("Downloaded \"A - B\": https://music.youtube.com/watch?v=x"), Some(Outcome::Saved));
        assert_eq!(
            classify("Skipping A - B (file already exists) (duplicate)"),
            Some(Outcome::Skipped)
        );
        assert_eq!(classify("Processing query: x"), None);
    }

    #[test]
    fn counts_saved_error_lines_without_timestamp() {
        let text = "2026-11-18-17-30-01\nhttps://open.spotify.com/track/x - LookupError: No results found for song: A - B\n\n";
        assert_eq!(count_errors(text), 1);
        assert_eq!(count_errors(""), 0);
    }
}

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use axum::{
    extract::State,
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        Html, Json,
    },
    routing::{get, post},
    Router,
};
use lofty::{
    config::WriteOptions,
    picture::{MimeType, Picture, PictureType},
    prelude::*,
    probe::Probe,
    tag::Tag,
};
use regex::Regex;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::{
    convert::Infallible,
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tao::{
    dpi::LogicalSize,
    event::{Event as TaoEvent, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    window::WindowBuilder,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
    sync::{broadcast, Mutex},
    task::JoinHandle,
};
use tokio_stream::{wrappers::BroadcastStream, StreamExt};
use wry::WebViewBuilder;

const HTML_TEMPLATE: &str = include_str!("../index.html");

#[derive(Clone)]
struct AppState {
    client: Client,
    log_tx: broadcast::Sender<String>,
    active_task: Arc<Mutex<Option<JoinHandle<()>>>>,
}

#[derive(Serialize, Deserialize, Default)]
struct AppConfig {
    output_dir: String,
}

#[derive(Deserialize)]
struct SaveConfigReq {
    output_dir: String,
}

#[derive(Deserialize)]
struct DownloadReq {
    query: String,
    output_dir: String,
    format: String,
    bitrate: String,
    naming: String,
}

#[derive(Serialize)]
struct LogEvent {
    #[serde(rename = "type")]
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<i32>,
}

#[derive(Debug, Clone)]
struct TrackMeta {
    title: String,
    artist: String,
    album: String,
    artwork_url: Option<String>,
}

#[derive(Deserialize)]
struct OEmbedResp {
    title: String,
}

#[derive(Deserialize)]
struct ItunesResp {
    results: Vec<ItunesTrack>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ItunesTrack {
    track_name: Option<String>,
    artist_name: Option<String>,
    collection_name: Option<String>,
    artwork_url100: Option<String>,
}

fn config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".spotdl_studio_config.json")
}

fn spotdl_dir() -> PathBuf {
    let dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".spotdl");
    let _ = fs::create_dir_all(&dir);
    dir
}

fn load_config() -> AppConfig {
    fs::read_to_string(config_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_config(cfg: &AppConfig) {
    if let Ok(json) = serde_json::to_string_pretty(cfg) {
        let _ = fs::write(config_path(), json);
    }
}

fn expand_tilde(path_str: &str) -> PathBuf {
    if let Some(stripped) = path_str.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(stripped);
        }
    }
    PathBuf::from(path_str)
}

fn effective_output_dir(raw: &str) -> PathBuf {
    let trimmed = raw.trim();
    if !trimmed.is_empty() {
        return expand_tilde(trimmed);
    }
    let saved = load_config().output_dir;
    if !saved.trim().is_empty() {
        return expand_tilde(saved.trim());
    }
    dirs::audio_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join("Music"))
        .join("spotDL")
}

fn find_binary(name: &str) -> Option<PathBuf> {
    let ext = if cfg!(windows) { ".exe" } else { "" };

    // 1. Check same folder as the application binary
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            let beside = exe_dir.join(format!("{name}{ext}"));
            if beside.is_file() {
                return Some(beside);
            }
        }
    }

    // 2. Check ~/.spotdl/
    let local = spotdl_dir().join(format!("{name}{ext}"));
    if local.exists() && fs::metadata(&local).map(|m| m.len() > 100_000).unwrap_or(false) {
        return Some(local);
    }

    // 3. Check system PATH
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(format!("{name}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn emit_log(tx: &broadcast::Sender<String>, line: impl Into<String>) {
    let payload = LogEvent {
        kind: "log".into(),
        text: Some(line.into()),
        code: None,
    };
    if let Ok(json) = serde_json::to_string(&payload) {
        let _ = tx.send(json);
    }
}

fn emit_done(tx: &broadcast::Sender<String>, code: i32) {
    let payload = LogEvent {
        kind: "done".into(),
        text: None,
        code: Some(code),
    };
    if let Ok(json) = serde_json::to_string(&payload) {
        let _ = tx.send(json);
    }
}

async fn resolve_metadata(client: &Client, input: &str, tx: &broadcast::Sender<String>) -> TrackMeta {
    let mut query_text = input.trim().to_string();

    if query_text.contains("open.spotify.com") {
        let clean_url = query_text.split('?').next().unwrap_or(&query_text);
        let oembed_url = format!(
            "https://open.spotify.com/oembed?url={}",
            urlencoding::encode(clean_url)
        );
        if let Ok(resp) = client.get(&oembed_url).timeout(Duration::from_secs(5)).send().await {
            if let Ok(data) = resp.json::<OEmbedResp>().await {
                if !data.title.trim().is_empty() {
                    query_text = data.title.trim().to_string();
                    emit_log(tx, format!("> Matched Spotify track: {query_text}"));
                }
            }
        }
    }

    let search_term = if query_text.starts_with("http") {
        String::new()
    } else {
        query_text.clone()
    };

    if !search_term.is_empty() {
        let itunes_url = format!(
            "https://itunes.apple.com/search?term={}&entity=song&limit=1",
            urlencoding::encode(&search_term)
        );
        if let Ok(resp) = client.get(&itunes_url).timeout(Duration::from_secs(5)).send().await {
            if let Ok(data) = resp.json::<ItunesResp>().await {
                if let Some(track) = data.results.into_iter().next() {
                    let title = track.track_name.unwrap_or_else(|| search_term.clone());
                    let artist = track.artist_name.unwrap_or_default();
                    let album = track.collection_name.unwrap_or_default();
                    let artwork_url = track.artwork_url100.map(|u| u.replace("100x100bb", "600x600bb"));
                    emit_log(tx, format!("> Verified metadata: {artist} - {title}"));
                    return TrackMeta {
                        title,
                        artist,
                        album,
                        artwork_url,
                    };
                }
            }
        }
    }

    TrackMeta {
        title: query_text,
        artist: String::new(),
        album: String::new(),
        artwork_url: None,
    }
}

fn build_ytmusic_targets(raw_query: &str, meta: &TrackMeta) -> (Vec<String>, Vec<String>) {
    let trimmed = raw_query.trim();
    if trimmed.starts_with("http") && !trimmed.contains("open.spotify.com") {
        return (vec![trimmed.to_string()], vec![]);
    }

    let search_phrase = if !meta.artist.is_empty() {
        format!("{} - {}", meta.artist, meta.title)
    } else {
        meta.title.clone()
    };

    let lower = search_phrase.to_lowercase();
    let banned = [
        "sped up",
        "speed up",
        "slowed",
        "reverb",
        "nightcore",
        "instrumental",
        "karaoke",
        "acapella",
        "8d",
        "bass boosted",
    ];

    let active_bans: Vec<String> = banned
        .iter()
        .filter(|w| !lower.contains(**w))
        .map(|w| format!("title!*='{w}'"))
        .collect();

    let mut flags = vec![
        "--playlist-items".into(),
        "1:5".into(),
        "--max-downloads".into(),
        "1".into(),
    ];

    if !active_bans.is_empty() {
        flags.push("--match-filter".into());
        flags.push(active_bans.join(" & "));
    }

    let encoded = urlencoding::encode(&search_phrase);
    let ytm_songs_url = format!("https://music.youtube.com/search?q={encoded}#songs");
    let fallback_search = if lower.contains("audio") || lower.contains("official") {
        format!("ytsearch5:{search_phrase}")
    } else {
        format!("ytsearch5:{search_phrase} \"Topic\"")
    };

    (vec![ytm_songs_url, fallback_search], flags)
}

async fn embed_metadata(client: &Client, file_path: &Path, meta: &TrackMeta) -> Result<(), String> {
    let mut tagged_file = Probe::open(file_path)
        .map_err(|e| e.to_string())?
        .read()
        .map_err(|e| e.to_string())?;

    let tag = match tagged_file.primary_tag_mut() {
        Some(t) => t,
        None => {
            let tag_type = tagged_file.primary_tag_type();
            tagged_file.insert_tag(Tag::new(tag_type));
            tagged_file.primary_tag_mut().ok_or("Unable to initialize tag")?
        }
    };

    tag.set_title(meta.title.clone());
    if !meta.artist.is_empty() {
        tag.set_artist(meta.artist.clone());
    }
    if !meta.album.is_empty() {
        tag.set_album(meta.album.clone());
    }

    if let Some(ref art_url) = meta.artwork_url {
        if let Ok(resp) = client.get(art_url).timeout(Duration::from_secs(6)).send().await {
            if let Ok(bytes) = resp.bytes().await {
                let pic = Picture::new_unchecked(
                    PictureType::CoverFront,
                    Some(MimeType::Jpeg),
                    None,
                    bytes.to_vec(),
                );
                tag.push_picture(pic);
            }
        }
    }

    tagged_file
        .save_to_path(file_path, WriteOptions::default())
        .map_err(|e| e.to_string())
}

async fn index_handler() -> Html<String> {
    let saved = load_config().output_dir;
    Html(HTML_TEMPLATE.replace("__DEFAULT_OUTPUT__", &saved))
}

async fn check_engine_handler() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "ffmpeg": find_binary("ffmpeg").is_some(),
        "yt_dlp": find_binary("yt-dlp").is_some()
    }))
}

async fn save_config_handler(Json(req): Json<SaveConfigReq>) -> Json<serde_json::Value> {
    let trimmed = req.output_dir.trim().to_string();
    save_config(&AppConfig {
        output_dir: trimmed.clone(),
    });
    if !trimmed.is_empty() {
        let _ = fs::create_dir_all(expand_tilde(&trimmed));
    }
    Json(serde_json::json!({ "ok": true }))
}

async fn browse_folder_handler() -> Json<serde_json::Value> {
    let start = effective_output_dir("");
    let _ = fs::create_dir_all(&start);

    #[cfg(target_os = "macos")]
    {
        let safe_dir = start.to_string_lossy().replace('"', "\\\"");
        let script = format!(
            "POSIX path of (choose folder with prompt \"Select Save Location\" default location POSIX file \"{safe_dir}\")"
        );
        let chosen = Command::new("osascript")
            .args(["-e", &script])
            .output()
            .await
            .ok()
            .filter(|o| o.status.success())
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .trim()
                    .trim_end_matches('/')
                    .to_string()
            })
            .unwrap_or_default();
        return Json(serde_json::json!({ "path": chosen }));
    }

    #[cfg(not(target_os = "macos"))]
    {
        let chosen = rfd::AsyncFileDialog::new()
            .set_directory(&start)
            .pick_folder()
            .await
            .map(|h| h.path().to_string_lossy().to_string())
            .unwrap_or_default();
        Json(serde_json::json!({ "path": chosen }))
    }
}

async fn logs_handler(
    State(state): State<AppState>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let rx = state.log_tx.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|msg| match msg {
        Ok(text) => Some(Ok(Event::default().data(text))),
        Err(_) => None,
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

async fn download_handler(
    State(state): State<AppState>,
    Json(req): Json<DownloadReq>,
) -> (StatusCode, Json<serde_json::Value>) {
    let mut guard = state.active_task.lock().await;
    if let Some(handle) = guard.as_ref() {
        if !handle.is_finished() {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({ "error": "A download is already in progress." })),
            );
        }
    }

    let Some(yt_dlp_bin) = find_binary("yt-dlp") else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "yt-dlp not found. Place yt-dlp next to the app or in ~/.spotdl/." })),
        );
    };

    let output_dir = effective_output_dir(&req.output_dir);
    let _ = fs::create_dir_all(&output_dir);
    if !req.output_dir.trim().is_empty() {
        save_config(&AppConfig {
            output_dir: req.output_dir.trim().to_string(),
        });
    }

    let tx = state.log_tx.clone();
    let client = state.client.clone();

    let handle = tokio::spawn(async move {
        emit_log(&tx, format!("\n> Starting download: {}", req.query.trim()));
        emit_log(&tx, format!("> Save folder: {}", output_dir.display()));

        let meta = resolve_metadata(&client, &req.query, &tx).await;
        let (targets, extra_flags) = build_ytmusic_targets(&req.query, &meta);

        let filename_tpl = if req.naming == "artist_title" && !meta.artist.is_empty() {
            format!(
                "{} - {}.%(ext)s",
                sanitize_filename(&meta.artist),
                sanitize_filename(&meta.title)
            )
        } else if !meta.title.is_empty() && !meta.title.starts_with("http") {
            format!("{}.%(ext)s", sanitize_filename(&meta.title))
        } else {
            "%(title)s.%(ext)s".to_string()
        };

        let out_template = output_dir.join(&filename_tpl);
        let ffmpeg_bin = find_binary("ffmpeg");

        let mut base_args: Vec<String> = vec![
            "--socket-timeout".into(),
            "10".into(),
            "--no-warnings".into(),
            "--extractor-args".into(),
            "youtube:player_client=android,ios,tv_embedded,web".into(),
        ];
        base_args.extend(extra_flags);

        if let Some(ref ff) = ffmpeg_bin {
            if req.format != "m4a" {
                base_args.extend([
                    "-f".into(),
                    "bestaudio".into(),
                    "--ffmpeg-location".into(),
                    ff.to_string_lossy().to_string(),
                    "-x".into(),
                    "--audio-format".into(),
                    req.format.clone(),
                    "--audio-quality".into(),
                    req.bitrate.clone(),
                ]);
            } else {
                base_args.extend(["-f".into(), "bestaudio[ext=m4a]/bestaudio".into()]);
            }
        } else {
            if req.format != "m4a" {
                emit_log(
                    &tx,
                    "FFmpeg not found — saving native M4A audio (compatible with Apple Music, Spotify Local Files, Windows, and Linux).",
                );
            }
            base_args.extend(["-f".into(), "bestaudio[ext=m4a]/bestaudio".into()]);
        }

        base_args.extend([
            "--print".into(),
            "after_move:filepath".into(),
            "-o".into(),
            out_template.to_string_lossy().to_string(),
        ]);

        let ansi_re = Regex::new(r"\x1b\[[0-9;]*m").unwrap();
        let mut downloaded_file: Option<PathBuf> = None;
        let mut final_exit_code = 1;

        for (idx, target_url) in targets.iter().enumerate() {
            if idx == 0 && target_url.contains("music.youtube.com") {
                emit_log(&tx, "> Searching YouTube Music...");
            } else if idx > 0 {
                emit_log(&tx, "> Retrying via YouTube Music Topic catalog...");
            }

            let mut args = base_args.clone();
            args.push(target_url.clone());

            let mut cmd = Command::new(&yt_dlp_bin);
            cmd.args(&args)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);

            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
            }

            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    emit_log(&tx, format!("Failed to start yt-dlp: {e}"));
                    emit_done(&tx, 1);
                    return;
                }
            };

            if let Some(stderr) = child.stderr.take() {
                let tx_err = tx.clone();
                let re_err = ansi_re.clone();
                tokio::spawn(async move {
                    let mut err_lines = BufReader::new(stderr).lines();
                    while let Ok(Some(line)) = err_lines.next_line().await {
                        let clean = re_err.replace_all(&line, "").trim().to_string();
                        if clean.is_empty()
                            || clean.contains("Deprecated Feature: Support for Python")
                            || clean == "or above"
                        {
                            continue;
                        }
                        emit_log(&tx_err, clean);
                    }
                });
            }

            if let Some(stdout) = child.stdout.take() {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let clean = ansi_re.replace_all(&line, "").trim().to_string();
                    if clean.is_empty() || clean.contains("Maximum number of downloads reached") {
                        continue;
                    }
                    let candidate = PathBuf::from(&clean);
                    if candidate.is_absolute() && candidate.exists() {
                        downloaded_file = Some(candidate);
                    } else {
                        emit_log(&tx, clean);
                    }
                }
            }

            let status = child.wait().await;
            let code = match status {
                Ok(s) if s.success() || s.code() == Some(101) => 0,
                Ok(s) => s.code().unwrap_or(1),
                Err(_) => 1,
            };

            if code == 0 && downloaded_file.is_some() {
                final_exit_code = 0;
                break;
            }
            final_exit_code = code;
        }

        if final_exit_code == 0 {
            if let Some(ref path) = downloaded_file {
                emit_log(&tx, format!("> Saved file: {}", path.display()));
                match embed_metadata(&client, path, &meta).await {
                    Ok(_) => emit_log(&tx, "> Embedded official title, artist, album, and cover art."),
                    Err(e) => emit_log(&tx, format!("> Note: Could not embed cover tags ({e}).")),
                }
            }
        }

        emit_done(&tx, final_exit_code);
    });

    *guard = Some(handle);
    (StatusCode::OK, Json(serde_json::json!({ "ok": true })))
}

async fn cancel_handler(State(state): State<AppState>) -> Json<serde_json::Value> {
    let mut guard = state.active_task.lock().await;
    if let Some(handle) = guard.take() {
        handle.abort();
        emit_log(&state.log_tx, "> Download cancelled.");
    }
    Json(serde_json::json!({ "ok": true }))
}

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect()
}

fn main() {
    let (log_tx, _) = broadcast::channel::<String>(256);
    let client = Client::builder()
        .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)")
        .connect_timeout(Duration::from_secs(4))
        .timeout(Duration::from_secs(6))
        .build()
        .expect("Failed to build HTTP client");

    let state = AppState {
        client,
        log_tx,
        active_task: Arc::new(Mutex::new(None)),
    };

    let (port_tx, port_rx) = std::sync::mpsc::channel::<u16>();

    // Run the Axum backend on a background Tokio thread
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime");
        rt.block_on(async move {
            let app = Router::new()
                .route("/", get(index_handler))
                .route("/check-engine", get(check_engine_handler))
                .route("/save-config", post(save_config_handler))
                .route("/browse-folder", get(browse_folder_handler))
                .route("/logs", get(logs_handler))
                .route("/download", post(download_handler))
                .route("/cancel", post(cancel_handler))
                .with_state(state);

            // Bind to port 0 so OS assigns an open port (avoids port-in-use conflicts)
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("Failed to bind local server");
            let port = listener.local_addr().unwrap().port();
            let _ = port_tx.send(port);

            axum::serve(listener, app).await.unwrap();
        });
    });

    let port = port_rx.recv().expect("Server failed to start");
    let app_url = format!("http://127.0.0.1:{port}");

    // Launch native standalone desktop window (WebKit on macOS, WebView2 on Windows)
    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title("spotDL Studio")
        .with_inner_size(LogicalSize::new(920.0, 740.0))
        .with_min_inner_size(LogicalSize::new(680.0, 620.0))
        .build(&event_loop)
        .expect("Failed to create desktop window");

    let _webview = WebViewBuilder::new()
        .with_url(&app_url)
        .build(&window)
        .expect("Failed to initialize WebView");

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        if let TaoEvent::WindowEvent {
            event: WindowEvent::CloseRequested,
            ..
        } = event
        {
            *control_flow = ControlFlow::Exit;
        }
    });
}
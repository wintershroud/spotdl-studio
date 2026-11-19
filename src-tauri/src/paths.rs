use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::OnceLock,
};

const DATA_FOLDER: &str = "spotDL Studio data";

/// Where settings, unpacked engines and spotDL's own cache live.
///
/// On Windows and Linux this is a folder next to the app, so the app stays
/// portable (a USB stick keeps everything). If that folder can't be written,
/// or on macOS where apps live in /Applications, it falls back to the
/// normal per-user app data folder.
pub fn data_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        portable_dir()
            .filter(|dir| is_writable(dir))
            .unwrap_or_else(|| {
                let dir = dirs::data_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join("spotDL Studio");
                let _ = fs::create_dir_all(&dir);
                dir
            })
    })
}

fn portable_dir() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        return None;
    }
    // An AppImage runs from a read-only mount; $APPIMAGE is the real file.
    let app = std::env::var_os("APPIMAGE")
        .map(PathBuf::from)
        .or_else(|| std::env::current_exe().ok())?;
    Some(app.parent()?.join(DATA_FOLDER))
}

fn is_writable(dir: &Path) -> bool {
    let probe = dir.join(".write-test");
    let ok = fs::create_dir_all(dir).is_ok() && fs::write(&probe, b"").is_ok();
    let _ = fs::remove_file(probe);
    ok
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Settings {
    pub output_dir: String,
    pub format: String,
    pub bitrate: String,
    pub naming: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            output_dir: default_output_dir().to_string_lossy().into_owned(),
            format: "mp3".into(),
            bitrate: "320k".into(),
            naming: "artist_title".into(),
        }
    }
}

fn default_output_dir() -> PathBuf {
    dirs::audio_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join("Music")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("spotDL")
}

fn settings_path() -> PathBuf {
    data_dir().join("settings.json")
}

pub fn load_settings() -> Settings {
    if let Some(s) = fs::read_to_string(settings_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
    {
        return s;
    }
    // Carry over the save folder from the 0.1 app, which kept it in the home folder.
    let mut s = Settings::default();
    if let Some(old) = dirs::home_dir()
        .and_then(|h| fs::read_to_string(h.join(".spotdl_studio_config.json")).ok())
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v["output_dir"].as_str().map(str::to_owned))
        .filter(|d| !d.trim().is_empty())
    {
        s.output_dir = old;
    }
    s
}

pub fn save_settings(s: &Settings) -> Result<(), String> {
    let json = serde_json::to_string_pretty(s).map_err(|e| e.to_string())?;
    fs::write(settings_path(), json).map_err(|e| format!("Could not save settings: {e}"))
}

pub fn expand_tilde(path: &str) -> PathBuf {
    let path = path.trim();
    match (path.strip_prefix("~/").or(path.strip_prefix("~\\")), dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ if path == "~" => dirs::home_dir().unwrap_or_default(),
        _ => PathBuf::from(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tilde_expands_to_home() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(expand_tilde("~/Music"), home.join("Music"));
        assert_eq!(expand_tilde("  /tmp/x "), PathBuf::from("/tmp/x"));
    }

    #[test]
    fn partial_settings_fill_defaults() {
        let s: Settings = serde_json::from_str(r#"{"format":"flac"}"#).unwrap();
        assert_eq!(s.format, "flac");
        assert_eq!(s.bitrate, "320k");
    }
}

//! spotDL, ffmpeg and deno are packed into the app at build time and unpacked
//! once into the data folder on first launch. Builds made without them (plain
//! `cargo run` during development) use whatever is on PATH instead.

use std::path::{Path, PathBuf};

pub const SPOTDL_VERSION: &str = "4.6.0";

#[derive(Clone)]
pub struct Engines {
    pub spotdl: PathBuf,
    pub ffmpeg: PathBuf,
    /// Prepended to spotDL's PATH so its yt-dlp finds deno.
    pub bin_dir: Option<PathBuf>,
}

const EXE: &str = if cfg!(windows) { ".exe" } else { "" };

#[cfg(embedded_engines)]
pub fn prepare(data_dir: &Path) -> Result<Engines, String> {
    use std::{fs, io};

    static SPOTDL: &[u8] = include_bytes!(env!("SPOTDL_STUDIO_SPOTDL"));
    static FFMPEG_GZ: &[u8] = include_bytes!(env!("SPOTDL_STUDIO_FFMPEG"));
    static DENO_ZIP: &[u8] = include_bytes!(env!("SPOTDL_STUDIO_DENO"));
    const ID: &str = env!("SPOTDL_STUDIO_ENGINE_ID");

    let root = data_dir.join("engines");
    let dir = root.join(ID);
    let engines = Engines {
        spotdl: dir.join(format!("spotdl{EXE}")),
        ffmpeg: dir.join(format!("ffmpeg{EXE}")),
        bin_dir: Some(dir.clone()),
    };
    if dir.join(".complete").is_file() {
        return Ok(engines);
    }

    let fail = |what: &str, e: &dyn std::fmt::Display| format!("Could not unpack {what}: {e}");
    let tmp = root.join(format!(".unpacking-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).map_err(|e| fail("engines", &e))?;

    fs::write(tmp.join(format!("spotdl{EXE}")), SPOTDL).map_err(|e| fail("spotDL", &e))?;

    let mut ffmpeg = fs::File::create(tmp.join(format!("ffmpeg{EXE}"))).map_err(|e| fail("ffmpeg", &e))?;
    io::copy(&mut flate2::read::GzDecoder::new(FFMPEG_GZ), &mut ffmpeg).map_err(|e| fail("ffmpeg", &e))?;
    drop(ffmpeg);

    let mut zip = zip::ZipArchive::new(io::Cursor::new(DENO_ZIP)).map_err(|e| fail("deno", &e))?;
    let mut deno = zip.by_name(&format!("deno{EXE}")).map_err(|e| fail("deno", &e))?;
    let mut out = fs::File::create(tmp.join(format!("deno{EXE}"))).map_err(|e| fail("deno", &e))?;
    io::copy(&mut deno, &mut out).map_err(|e| fail("deno", &e))?;
    drop(out);

    #[cfg(unix)]
    for name in ["spotdl", "ffmpeg", "deno"] {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(tmp.join(name), fs::Permissions::from_mode(0o755))
            .map_err(|e| fail(name, &e))?;
    }

    fs::write(tmp.join(".complete"), b"").map_err(|e| fail("engines", &e))?;
    let _ = fs::remove_dir_all(&dir);
    fs::rename(&tmp, &dir).map_err(|e| fail("engines", &e))?;

    // Engines from older app versions are dead weight (a few hundred MB).
    if let Ok(entries) = fs::read_dir(&root) {
        for entry in entries.flatten() {
            if entry.file_name() != ID {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }
    Ok(engines)
}

#[cfg(not(embedded_engines))]
pub fn prepare(_data_dir: &Path) -> Result<Engines, String> {
    let find = |name: &str| -> Option<PathBuf> {
        let file = format!("{name}{EXE}");
        std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|dir| dir.join(&file))
            .find(|p| p.is_file())
    };
    Ok(Engines {
        spotdl: find("spotdl").ok_or("spotdl was not found on PATH, and this build has no engines packed in.")?,
        ffmpeg: find("ffmpeg").ok_or("ffmpeg was not found on PATH, and this build has no engines packed in.")?,
        bin_dir: None,
    })
}

#[cfg(all(test, embedded_engines))]
mod tests {
    use super::*;

    #[test]
    fn unpacks_once_and_clears_old_versions() {
        let data = std::env::temp_dir().join(format!("spotdl-studio-test-{}", std::process::id()));
        let stale = data.join("engines").join("old-version");
        std::fs::create_dir_all(&stale).unwrap();

        let engines = prepare(&data).unwrap();
        let dir = engines.bin_dir.clone().unwrap();
        for name in ["spotdl", "ffmpeg", "deno"] {
            let len = std::fs::metadata(dir.join(format!("{name}{EXE}"))).unwrap().len();
            assert!(len > 1_000_000, "{name} is only {len} bytes");
        }
        assert!(!stale.exists());

        // A second call reuses the unpacked folder.
        let marker = std::fs::metadata(dir.join(".complete")).unwrap().modified().unwrap();
        prepare(&data).unwrap();
        assert_eq!(std::fs::metadata(dir.join(".complete")).unwrap().modified().unwrap(), marker);
        std::fs::remove_dir_all(&data).unwrap();
    }
}

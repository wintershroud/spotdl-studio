# spotDL Studio

A small desktop app for [spotDL](https://github.com/spotDL/spotify-downloader). Paste a Spotify track, album or playlist link (or type "Artist - Song"), pick a folder and a format, and press Download. spotDL finds each song on YouTube Music and saves it with the Spotify title, artist, album and cover art.

## Download

Grab the file for your system from the [latest release](../../releases/tag/latest):

| System | File | How it runs |
| --- | --- | --- |
| Windows 10/11 | `spotDL-Studio-Windows.exe` | One file, no install. Double-click it. |
| macOS 11+ (Apple Silicon) | `spotDL-Studio-macOS.dmg` | Drag the app wherever you like. |
| Linux (x86-64) | `spotDL-Studio-Linux.AppImage` | `chmod +x` it, then run it. |

The builds aren't signed, so the first launch shows a warning. On Windows, click "More info", then "Run anyway". On macOS, right-click the app, choose Open, then Open again.

Intel Macs aren't supported because spotDL only publishes an Apple Silicon build.

## Portable

spotDL, ffmpeg and deno are packed inside the app, so there's nothing else to install. The first launch unpacks them, which takes a few seconds.

On Windows and Linux the app keeps everything in a `spotDL Studio data` folder next to itself: settings, the unpacked engines and spotDL's cache. Copy the app to a USB stick and that folder comes with it. Delete the app and the folder and it's gone. If the app's own folder can't be written to, it uses the normal per-user app data folder instead. On macOS it always uses `~/Library/Application Support/spotDL Studio`.

## What's inside

| Engine | Version | Source |
| --- | --- | --- |
| spotDL | 4.6.0 | [spotDL/spotify-downloader](https://github.com/spotDL/spotify-downloader/releases/tag/v4.6.0) |
| ffmpeg | 6.1.1 | [eugeneware/ffmpeg-static](https://github.com/eugeneware/ffmpeg-static/releases/tag/b6.1.1) |
| deno | 2.9.8 | [denoland/deno](https://github.com/denoland/deno/releases/tag/v2.9.8) (yt-dlp needs it for YouTube) |

`engines.lock` pins each download to a SHA-256 hash, and the build stops if a file doesn't match. To update an engine, change its URL and hash in `engines.lock` and bump `SPOTDL_VERSION` in `src-tauri/src/engine.rs` if spotDL changed.

## Building

You need Rust, Node (for the Tauri CLI) and, on Linux, the WebKitGTK build libraries listed in `.github/workflows/build.yml`.

```sh
bash scripts/fetch-engines.sh windows   # or macos, linux
npx @tauri-apps/cli@2.12.4 build --no-bundle   # Windows: one .exe
npx @tauri-apps/cli@2.12.4 build --bundles app,dmg   # macOS
npx @tauri-apps/cli@2.12.4 build --bundles appimage   # Linux
```

Skip `fetch-engines.sh` during development and the app uses `spotdl` and `ffmpeg` from your PATH instead. `cargo test` in `src-tauri` runs the tests.

Every push to `main` builds all three platforms on GitHub Actions and updates the `latest` release.

## Layout

- `ui/` is the window: plain HTML, CSS and JavaScript, no build step.
- `src-tauri/src/main.rs` holds the commands the window calls.
- `src-tauri/src/download.rs` runs spotDL and streams its output to the log.
- `src-tauri/src/engine.rs` unpacks the packed-in engines.
- `src-tauri/src/paths.rs` handles the data folder and settings.

## Use it for music you're allowed to download

The app downloads audio from YouTube. Whether that's allowed depends on where you live and what you're downloading.

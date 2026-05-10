# VideoSniffer

Rust Windows desktop app prototype for browser video sniffing and multi-threaded downloads.

## Architecture

- Browser extension captures media network responses.
- Extension sends candidate video URLs to `http://127.0.0.1:37651/api/media`.
- Extension preserves key playback request headers such as Cookie, Referer, Origin and User-Agent for the downloader.
- Rust app shows detected media and download queue.
- Detected media, settings and tasks are persisted locally so the app can restore them after restart.
- Network requests use timeout, retry, exponential backoff and clearer source-error classification.
- Failed HLS or range parts are deprioritized and retried later so other parts can continue first.
- Incomplete HLS and range downloads keep a temporary manifest at `.parts/{task_id}/manifest.json` so restart/resume skips completed parts.
- Downloader supports direct MP4/WEBM/unknown file URLs with ranged multi-thread download when the server supports `Accept-Ranges: bytes`.
- HLS media playlists are downloaded by fetching segments concurrently, merging into `.ts`, and remuxing to `.mp4` with `ffmpeg` when available.
- HLS master playlists are analyzed automatically. The app defaults to the highest bitrate variant and lets the user switch quality in the UI before downloading.
- Non-DRM HLS `EXT-X-KEY:METHOD=AES-128` streams are decrypted when the playlist exposes the key URL and IV.
- DASH parsing is intentionally separated for the next step.

## Run

```powershell
cargo run
```

## Load Browser Extension

1. Open Chrome or Edge extensions page.
2. Enable developer mode.
3. Load unpacked extension from `extensions/chrome`.
4. Start the Rust app, then play a video in the browser.

## First Version Scope

- Local sniffer endpoint.
- Desktop UI.
- Save directory setting.
- Detected media list.
- Request header forwarding from the browser extension.
- Download queue.
- Pause/resume controls for active tasks.
- Persisted task history and settings.
- Task detail panel showing temporary manifest progress for completed and pending parts.
- Retry handling for playlist, key, segment, direct and range requests.
- Per-part retry queue for HLS segments and ranged direct downloads.
- Partial-file reuse for completed HLS segments and completed range parts after pause, crash or app restart.
- Multi-thread ranged download for direct media files.
- Non-encrypted HLS media playlist segment download and merge.
- HLS master playlist quality discovery, highest-bitrate default selection, and manual quality switching.
- AES-128 HLS segment decryption using playlist-provided `EXT-X-KEY` metadata.
- Optional HLS TS-to-MP4 remuxing through `ffmpeg -c copy`, with TS fallback when ffmpeg is unavailable.
- Bundled ffmpeg at `third_party/ffmpeg/ffmpeg.exe`; package builds should include it next to the app or under `bin/ffmpeg.exe`.

## Bundled FFmpeg

The app resolves ffmpeg in this order:

1. `VIDEOSNIFFER_FFMPEG` environment variable.
2. `ffmpeg.exe` next to the packaged app.
3. `bin/ffmpeg.exe` next to the packaged app.
4. `third_party/ffmpeg/ffmpeg.exe` in the repo or packaged directory.
5. `ffmpeg` from system `PATH`.

## Next Implementation Step

- Add per-part retry history and last error visibility.

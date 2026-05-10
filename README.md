# VideoSniffer

Rust Windows desktop app prototype for browser video sniffing and multi-threaded downloads.

## Architecture

- Browser extension captures media network responses.
- Extension sends candidate video URLs to `http://127.0.0.1:37651/api/media`.
- Extension preserves key playback request headers such as Cookie, Referer, Origin and User-Agent for the downloader.
- Rust app shows detected media and download queue.
- Downloader supports direct MP4/WEBM/unknown file URLs with ranged multi-thread download when the server supports `Accept-Ranges: bytes`.
- HLS media playlists are downloaded by fetching segments concurrently and merging them into a `.ts` output file.
- HLS master playlists are analyzed automatically. The app defaults to the highest bitrate variant and lets the user switch quality in the UI before downloading.
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
- Multi-thread ranged download for direct media files.
- Non-encrypted HLS media playlist segment download and merge.
- HLS master playlist quality discovery, highest-bitrate default selection, and manual quality switching.

## Next Implementation Step

- Add encrypted HLS key handling where allowed by the source.
- Preserve cookies and richer request headers from extension.
- Add pause/resume and persisted task database.

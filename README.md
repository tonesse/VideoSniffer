# VideoSniffer

Rust Windows desktop app prototype for browser video sniffing and multi-threaded downloads.

## Architecture

- Browser extension captures media network responses.
- Extension sends candidate video URLs to `http://127.0.0.1:37651/api/media`.
- Rust app shows detected media and download queue.
- Downloader currently supports direct MP4/WEBM/unknown file URLs with ranged multi-thread download when the server supports `Accept-Ranges: bytes`.
- HLS/DASH parsing is intentionally separated for the next step.

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
- Download queue.
- Multi-thread ranged download for direct media files.

## Next Implementation Step

- Add HLS `.m3u8` parsing.
- Download and merge `.ts` / `.m4s` segments.
- Preserve cookies and richer request headers from extension.
- Add pause/resume and persisted task database.

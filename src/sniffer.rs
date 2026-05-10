use crate::state::{MediaCandidate, MediaItem, SharedState};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderValue, Method, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use std::{net::SocketAddr, time::Duration};
use tokio::net::TcpListener;

pub fn spawn_sniffer_server(state: SharedState, port: u16) {
    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(err) => {
                eprintln!("failed to start sniffer runtime: {err}");
                return;
            }
        };

        runtime.block_on(async move {
            if let Err(err) = run_server(state, port).await {
                eprintln!("sniffer server stopped: {err}");
            }
        });
    });
}

async fn run_server(state: SharedState, port: u16) -> anyhow::Result<()> {
    let app = Router::new()
        .route("/health", get(health))
        .route("/api/media", post(receive_media).options(cors_preflight))
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health() -> impl IntoResponse {
    with_cors((StatusCode::OK, "ok"))
}

async fn cors_preflight() -> impl IntoResponse {
    with_cors(StatusCode::NO_CONTENT)
}

async fn receive_media(
    State(state): State<SharedState>,
    Json(candidate): Json<MediaCandidate>,
) -> impl IntoResponse {
    if !looks_like_media(&candidate) {
        return with_cors((StatusCode::NO_CONTENT, "ignored"));
    }

    state.write(|app| {
        if let Some(item) = app
            .detected
            .iter_mut()
            .find(|item| item.url == candidate.url)
        {
            if !candidate.request_headers.is_empty() {
                item.headers = candidate.request_headers.clone();
            }
            if candidate.page_url.is_some() {
                item.page_url = candidate.page_url.clone();
            }
            if let Some(title) = candidate
                .title
                .as_ref()
                .filter(|title| !title.trim().is_empty())
            {
                item.title = title.clone();
            }
            return;
        }

        app.detected.push_front(MediaItem::from(candidate));
        while app.detected.len() > 200 {
            app.detected.pop_back();
        }
    });

    with_cors((StatusCode::CREATED, "accepted"))
}

fn looks_like_media(candidate: &MediaCandidate) -> bool {
    let url = candidate.url.to_ascii_lowercase();
    let mime = candidate
        .mime_type
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();

    url.contains(".m3u8")
        || url.contains(".mpd")
        || url.contains(".mp4")
        || url.contains(".webm")
        || mime.contains("video/")
        || mime.contains("mpegurl")
        || mime.contains("dash")
}

fn with_cors<T>(response: T) -> impl IntoResponse
where
    T: IntoResponse,
{
    let mut response = response.into_response();
    let headers = response.headers_mut();
    headers.insert("access-control-allow-origin", HeaderValue::from_static("*"));
    headers.insert(
        "access-control-allow-methods",
        HeaderValue::from_static("POST, OPTIONS, GET"),
    );
    headers.insert(
        "access-control-allow-headers",
        HeaderValue::from_static("content-type"),
    );
    headers.insert("access-control-max-age", HeaderValue::from_static("86400"));
    response
}

#[allow(dead_code)]
fn _method_is_allowed(method: &Method) -> bool {
    matches!(method, &Method::POST | &Method::OPTIONS | &Method::GET)
}

#[allow(dead_code)]
async fn _short_pause_for_tests() {
    tokio::time::sleep(Duration::from_millis(25)).await;
}

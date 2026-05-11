use crate::{
    hls::spawn_hls_variant_analysis,
    state::{MediaCandidate, MediaItem, MediaSidecar, MediaType, SharedState},
};
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

    if looks_like_segmented_mp4(&candidate) {
        let hls_media_id = state.write(|app| accept_segmented_mp4(app, candidate));
        if let Some(media_id) = hls_media_id {
            spawn_hls_variant_analysis(state, media_id);
        }
        return with_cors((StatusCode::CREATED, "accepted"));
    }

    if looks_like_audio(&candidate) {
        state.write(|app| {
            let sidecar = MediaSidecar::from(candidate);
            if app.detected.iter().any(|item| {
                item.audio.as_ref().is_some_and(|audio| {
                    media_dedupe_key(&audio.url) == media_dedupe_key(&sidecar.url)
                })
            }) || app
                .pending_audio
                .iter()
                .any(|audio| media_dedupe_key(&audio.url) == media_dedupe_key(&sidecar.url))
            {
                return;
            }

            if let Some(item) = app
                .detected
                .iter_mut()
                .find(|item| should_pair_audio(item, &sidecar))
            {
                item.audio = Some(sidecar);
                return;
            }

            app.pending_audio.push_front(sidecar);
            while app.pending_audio.len() > 200 {
                app.pending_audio.pop_back();
            }
        });
        return with_cors((StatusCode::CREATED, "accepted"));
    }

    let candidate_key = media_dedupe_key(&candidate.url);
    let hls_media_id = state.write(|app| {
        if let Some(item) = app
            .detected
            .iter_mut()
            .find(|item| media_dedupe_key(&item.url) == candidate_key)
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
            if candidate.content_length.is_some() {
                item.content_length = candidate.content_length;
            }
            if candidate.duration_seconds.is_some() {
                item.duration_seconds = candidate.duration_seconds;
            }
            if item.media_type == MediaType::Hls && item.hls_variants.is_empty() {
                return Some(item.id);
            }
            return None;
        }

        let mut item = MediaItem::from(candidate);
        if let Some(index) = app
            .pending_audio
            .iter()
            .position(|audio| should_pair_audio(&item, audio))
            && let Some(audio) = app.pending_audio.remove(index)
        {
            item.audio = Some(audio);
        }
        let hls_media_id = (item.media_type == MediaType::Hls).then_some(item.id);
        app.detected.push_front(item);
        while app.detected.len() > 200 {
            app.detected.pop_back();
        }
        hls_media_id
    });

    if let Some(media_id) = hls_media_id {
        spawn_hls_variant_analysis(state, media_id);
    }

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
        || url.contains(".m4s")
        || url.contains(".webm")
        || mime.contains("video/")
        || mime.contains("audio/")
        || mime.contains("mpegurl")
        || mime.contains("dash")
}

fn looks_like_audio(candidate: &MediaCandidate) -> bool {
    let url = candidate.url.to_ascii_lowercase();
    let mime = candidate
        .mime_type
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();

    mime.contains("audio/")
        || url.contains(".m4a")
        || url.contains(".aac")
        || url.contains(".mp3")
        || url.contains(".opus")
        || url.contains("/audio/")
}

fn looks_like_segmented_mp4(candidate: &MediaCandidate) -> bool {
    candidate.url.to_ascii_lowercase().contains(".m4s")
}

fn accept_segmented_mp4(
    app: &mut crate::state::AppState,
    candidate: MediaCandidate,
) -> Option<uuid::Uuid> {
    let candidate_key = media_dedupe_key(&candidate.url);
    if app.detected.iter().any(|item| {
        media_dedupe_key(&item.url) == candidate_key
            || item
                .audio
                .as_ref()
                .is_some_and(|audio| media_dedupe_key(&audio.url) == candidate_key)
    }) {
        return None;
    }

    let mut item = MediaItem::from(candidate);
    if let Some(existing) = app
        .detected
        .iter_mut()
        .find(|existing| should_pair_segmented_mp4(existing, &item))
    {
        if should_promote_segmented_mp4(&item, existing) {
            let sidecar = sidecar_from_item(existing);
            std::mem::swap(&mut existing.url, &mut item.url);
            std::mem::swap(&mut existing.page_url, &mut item.page_url);
            std::mem::swap(&mut existing.title, &mut item.title);
            std::mem::swap(&mut existing.media_type, &mut item.media_type);
            std::mem::swap(&mut existing.content_length, &mut item.content_length);
            std::mem::swap(&mut existing.duration_seconds, &mut item.duration_seconds);
            std::mem::swap(&mut existing.headers, &mut item.headers);
            existing.audio = Some(sidecar);
        } else {
            if existing.duration_seconds.is_none() {
                existing.duration_seconds = item.duration_seconds;
            }
            existing.audio = Some(sidecar_from_item(&item));
        }
        return None;
    }

    if let Some(index) = app
        .pending_audio
        .iter()
        .position(|audio| should_pair_sidecar_with_segmented_mp4(&item, audio))
        && let Some(audio) = app.pending_audio.remove(index)
    {
        item.audio = Some(audio);
    }

    let hls_media_id = (item.media_type == MediaType::Hls).then_some(item.id);
    app.detected.push_front(item);
    while app.detected.len() > 200 {
        app.detected.pop_back();
    }
    hls_media_id
}

fn should_pair_segmented_mp4(existing: &MediaItem, candidate: &MediaItem) -> bool {
    existing.audio.is_none()
        && looks_like_segmented_url(&existing.url)
        && looks_like_segmented_url(&candidate.url)
        && same_media_group(
            existing.page_url.as_deref(),
            &existing.url,
            candidate.page_url.as_deref(),
            &candidate.url,
        )
}

fn should_pair_sidecar_with_segmented_mp4(item: &MediaItem, sidecar: &MediaSidecar) -> bool {
    looks_like_segmented_url(&item.url)
        && looks_like_segmented_url(&sidecar.url)
        && same_media_group(
            item.page_url.as_deref(),
            &item.url,
            sidecar.page_url.as_deref(),
            &sidecar.url,
        )
}

fn should_promote_segmented_mp4(candidate: &MediaItem, existing: &MediaItem) -> bool {
    stream_score(&candidate.url, candidate.content_length, None)
        > stream_score(&existing.url, existing.content_length, None)
}

fn sidecar_from_item(item: &MediaItem) -> MediaSidecar {
    MediaSidecar {
        url: item.url.clone(),
        page_url: item.page_url.clone(),
        title: Some(item.title.clone()),
        content_length: item.content_length,
        duration_seconds: item.duration_seconds,
        headers: item.headers.clone(),
    }
}

fn should_pair_audio(item: &MediaItem, audio: &MediaSidecar) -> bool {
    if item.audio.is_some() || media_dedupe_key(&item.url) == media_dedupe_key(&audio.url) {
        return false;
    }

    if should_pair_sidecar_with_segmented_mp4(item, audio) {
        return true;
    }

    match (&item.page_url, &audio.page_url) {
        (Some(left), Some(right)) if normalized_page(left) == normalized_page(right) => true,
        _ => {
            !item.title.trim().is_empty()
                && audio
                    .title
                    .as_deref()
                    .is_some_and(|title| title.trim() == item.title.trim())
        }
    }
}

fn normalized_page(url: &str) -> String {
    url.split('#').next().unwrap_or(url).to_ascii_lowercase()
}

fn looks_like_segmented_url(url: &str) -> bool {
    url.to_ascii_lowercase().contains(".m4s")
}

fn same_media_group(
    left_page: Option<&str>,
    left_url: &str,
    right_page: Option<&str>,
    right_url: &str,
) -> bool {
    match (left_page, right_page) {
        (Some(left), Some(right)) if normalized_page(left) == normalized_page(right) => true,
        _ => bilibili_group_key(left_url)
            .zip(bilibili_group_key(right_url))
            .is_some_and(|(left, right)| left == right),
    }
}

fn bilibili_group_key(url: &str) -> Option<String> {
    let lower = url.to_ascii_lowercase();
    if !lower.contains(".m4s") || !lower.contains("/upgcxcode/") {
        return None;
    }

    let path = lower.split('?').next().unwrap_or(&lower);
    let cid = path
        .rsplit('/')
        .next()?
        .split('-')
        .next()
        .filter(|value| !value.is_empty())?;
    let query = lower.split_once('?').map(|(_, query)| query).unwrap_or("");
    let trid = query_value(query, "trid").unwrap_or_default();
    Some(format!("{cid}:{trid}"))
}

fn stream_score(url: &str, content_length: Option<u64>, mime: Option<&str>) -> u64 {
    let mime = mime.unwrap_or_default().to_ascii_lowercase();
    if mime.contains("video/") {
        return u64::MAX;
    }
    if mime.contains("audio/") {
        return 0;
    }

    query_value(
        url.split_once('?').map(|(_, query)| query).unwrap_or(""),
        "bw",
    )
    .and_then(|value| value.parse::<u64>().ok())
    .or(content_length)
    .unwrap_or(1)
}

fn query_value<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query.split('&').find_map(|part| {
        let (part_key, value) = part.split_once('=')?;
        part_key.eq_ignore_ascii_case(key).then_some(value)
    })
}

fn media_dedupe_key(url: &str) -> String {
    let without_fragment = url.split('#').next().unwrap_or(url);
    let Some((base, query)) = without_fragment.split_once('?') else {
        return without_fragment.to_ascii_lowercase();
    };

    let mut stable_params = query
        .split('&')
        .filter(|part| !part.is_empty())
        .filter(|part| {
            let key = part
                .split('=')
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            !matches!(
                key.as_str(),
                "range"
                    | "start"
                    | "end"
                    | "r"
                    | "rnd"
                    | "random"
                    | "ts"
                    | "t"
                    | "_"
                    | "token"
                    | "expires"
                    | "expire"
                    | "deadline"
            )
        })
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    stable_params.sort();

    if stable_params.is_empty() {
        base.to_ascii_lowercase()
    } else {
        format!("{}?{}", base.to_ascii_lowercase(), stable_params.join("&"))
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    const BILI_AUDIO: &str = "https://b-baaac10nxz3a75cr81cljvml1d.edge.mountaintoys.cn:4483/upgcxcode/51/13/38210831351/38210831351-1-100026.m4s?trid=0000c1454ead28bb43658b12d98b08c3013u&bw=2487979";
    const BILI_VIDEO: &str = "https://rve5881c.edge.mountaintoys.cn:4483/upgcxcode/51/13/38210831351/38210831351-1-30280.m4s?trid=0000c1454ead28bb43658b12d98b08c3013u&bw=112104";

    #[test]
    fn bilibili_m4s_urls_share_group_key() {
        assert_eq!(
            bilibili_group_key(BILI_AUDIO),
            bilibili_group_key(BILI_VIDEO)
        );
    }

    #[test]
    fn bilibili_m4s_urls_pair_without_page_url() {
        assert!(same_media_group(None, BILI_AUDIO, None, BILI_VIDEO));
    }

    #[test]
    fn m4s_bandwidth_score_prefers_higher_stream() {
        assert!(stream_score(BILI_AUDIO, None, None) > stream_score(BILI_VIDEO, None, None));
    }
}

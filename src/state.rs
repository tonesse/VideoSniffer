use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashSet, VecDeque},
    fs,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use uuid::Uuid;

#[derive(Clone)]
pub struct SharedState {
    inner: Arc<Mutex<AppState>>,
    revision: Arc<AtomicU64>,
    persist_path: Arc<PathBuf>,
    active_tasks: Arc<Mutex<HashSet<Uuid>>>,
}

impl SharedState {
    pub fn new() -> Self {
        let persist_path = state_file_path();
        let mut app_state = load_state(&persist_path).unwrap_or_else(default_state);
        app_state.detected.clear();

        for task in &mut app_state.tasks {
            if matches!(
                task.status,
                DownloadStatus::Downloading | DownloadStatus::Queued
            ) {
                task.status = DownloadStatus::Paused;
                task.message = "应用已重启，点击恢复可继续下载".to_string();
            }
        }

        let state = Self {
            inner: Arc::new(Mutex::new(app_state)),
            revision: Arc::new(AtomicU64::new(1)),
            persist_path: Arc::new(persist_path),
            active_tasks: Arc::new(Mutex::new(HashSet::new())),
        };
        state.persist();
        state
    }

    pub fn read<R>(&self, f: impl FnOnce(&AppState) -> R) -> R {
        f(&self.inner.lock())
    }

    pub fn write<R>(&self, f: impl FnOnce(&mut AppState) -> R) -> R {
        let (result, snapshot) = {
            let mut guard = self.inner.lock();
            let result = f(&mut guard);
            (result, guard.clone())
        };
        self.bump();
        self.persist_snapshot(&snapshot);
        result
    }

    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Relaxed)
    }

    pub fn bump(&self) {
        self.revision.fetch_add(1, Ordering::Relaxed);
    }

    pub fn mark_task_active(&self, task_id: Uuid) -> bool {
        self.active_tasks.lock().insert(task_id)
    }

    pub fn mark_task_inactive(&self, task_id: Uuid) {
        self.active_tasks.lock().remove(&task_id);
    }

    pub fn is_task_active(&self, task_id: Uuid) -> bool {
        self.active_tasks.lock().contains(&task_id)
    }

    fn persist(&self) {
        let snapshot = self.inner.lock().clone();
        self.persist_snapshot(&snapshot);
    }

    fn persist_snapshot(&self, snapshot: &AppState) {
        if let Some(parent) = self.persist_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(snapshot) {
            let _ = fs::write(self.persist_path.as_ref(), json);
        }
    }
}

fn default_state() -> AppState {
    let download_dir = dirs::video_dir()
        .or_else(dirs::download_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("VideoSniffer");

    AppState {
        detected: VecDeque::new(),
        tasks: Vec::new(),
        settings: Settings {
            save_dir: download_dir,
            max_tasks: default_max_tasks(),
            part_threads: default_part_threads(),
            min_media_size_mb: default_min_media_size_mb(),
            listen_port: 37651,
            request_retry_attempts: default_request_retry_attempts(),
            part_retry_attempts: default_part_retry_attempts(),
        },
    }
}

fn load_state(path: &PathBuf) -> Option<AppState> {
    let json = fs::read_to_string(path).ok()?;
    serde_json::from_str(&json).ok()
}

fn state_file_path() -> PathBuf {
    dirs::config_dir()
        .or_else(dirs::data_local_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("VideoSniffer")
        .join("state.json")
}

#[derive(Clone, Deserialize, Serialize)]
pub struct AppState {
    pub detected: VecDeque<MediaItem>,
    pub tasks: Vec<DownloadTask>,
    pub settings: Settings,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Settings {
    pub save_dir: PathBuf,
    #[serde(default = "default_max_tasks")]
    pub max_tasks: usize,
    #[serde(default = "default_part_threads")]
    pub part_threads: usize,
    #[serde(default = "default_min_media_size_mb")]
    pub min_media_size_mb: u64,
    pub listen_port: u16,
    #[serde(default = "default_request_retry_attempts")]
    pub request_retry_attempts: usize,
    #[serde(default = "default_part_retry_attempts")]
    pub part_retry_attempts: usize,
}

fn default_max_tasks() -> usize {
    3
}

fn default_part_threads() -> usize {
    8
}

fn default_min_media_size_mb() -> u64 {
    10
}

fn default_request_retry_attempts() -> usize {
    4
}

fn default_part_retry_attempts() -> usize {
    5
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MediaCandidate {
    pub url: String,
    pub page_url: Option<String>,
    pub title: Option<String>,
    pub mime_type: Option<String>,
    pub method: Option<String>,
    pub request_headers: Vec<HeaderPair>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HeaderPair {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct MediaItem {
    pub id: Uuid,
    pub url: String,
    pub page_url: Option<String>,
    pub title: String,
    pub media_type: MediaType,
    pub headers: Vec<HeaderPair>,
    pub hls_variants: Vec<HlsVariant>,
    pub selected_hls_variant_url: Option<String>,
    pub hls_status: Option<String>,
    pub detected_at: DateTime<Utc>,
}

impl MediaItem {
    pub fn selected_hls_url(&self) -> String {
        if self.media_type != MediaType::Hls {
            return self.url.clone();
        }

        self.selected_hls_variant_url
            .clone()
            .or_else(|| self.hls_variants.first().map(|variant| variant.url.clone()))
            .unwrap_or_else(|| self.url.clone())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HlsVariant {
    pub url: String,
    pub bandwidth: Option<u64>,
    pub resolution: Option<String>,
    pub codecs: Option<String>,
}

impl HlsVariant {
    pub fn label(&self) -> String {
        let quality = match (self.resolution.as_deref(), self.bandwidth) {
            (Some(resolution), Some(bandwidth)) => {
                format!("{resolution} · {}", bitrate_label(bandwidth))
            }
            (Some(resolution), None) => resolution.to_string(),
            (None, Some(bandwidth)) => bitrate_label(bandwidth),
            (None, None) => "自动清晰度".to_string(),
        };

        match &self.codecs {
            Some(codecs) if !codecs.is_empty() => format!("{quality} · {codecs}"),
            _ => quality,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub enum MediaType {
    Hls,
    Dash,
    Mp4,
    Webm,
    Unknown,
}

impl MediaType {
    pub fn from_url_and_mime(url: &str, mime: Option<&str>) -> Self {
        let lower_url = url.to_ascii_lowercase();
        let lower_mime = mime.unwrap_or_default().to_ascii_lowercase();

        if lower_url.contains(".m3u8") || lower_mime.contains("mpegurl") {
            Self::Hls
        } else if lower_url.contains(".mpd") || lower_mime.contains("dash") {
            Self::Dash
        } else if lower_url.contains(".mp4") || lower_mime.contains("mp4") {
            Self::Mp4
        } else if lower_url.contains(".webm") || lower_mime.contains("webm") {
            Self::Webm
        } else {
            Self::Unknown
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Hls => "HLS",
            Self::Dash => "DASH",
            Self::Mp4 => "MP4",
            Self::Webm => "WEBM",
            Self::Unknown => "未知",
        }
    }
}

impl From<MediaCandidate> for MediaItem {
    fn from(candidate: MediaCandidate) -> Self {
        let media_type =
            MediaType::from_url_and_mime(&candidate.url, candidate.mime_type.as_deref());
        let title = candidate
            .title
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| filename_from_url(&candidate.url));

        Self {
            id: Uuid::new_v4(),
            url: candidate.url,
            page_url: candidate.page_url,
            title,
            media_type,
            headers: candidate.request_headers,
            hls_variants: Vec::new(),
            selected_hls_variant_url: None,
            hls_status: None,
            detected_at: Utc::now(),
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub struct DownloadTask {
    pub id: Uuid,
    pub media_id: Uuid,
    pub title: String,
    pub url: String,
    pub media_type: MediaType,
    pub headers: Vec<HeaderPair>,
    pub status: DownloadStatus,
    pub progress: f32,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub completed_segments: usize,
    pub message: String,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq, Serialize)]
pub enum DownloadStatus {
    Queued,
    Downloading,
    Paused,
    Completed,
    Failed,
    Unsupported,
}

impl DownloadStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "等待中",
            Self::Downloading => "下载中",
            Self::Paused => "已暂停",
            Self::Completed => "已完成",
            Self::Failed => "失败",
            Self::Unsupported => "暂不支持",
        }
    }
}

pub fn filename_from_url(url: &str) -> String {
    let without_query = url.split('?').next().unwrap_or(url);
    without_query
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or("video")
        .to_string()
}

fn bitrate_label(bits_per_second: u64) -> String {
    if bits_per_second >= 1_000_000 {
        format!("{:.1} Mbps", bits_per_second as f64 / 1_000_000.0)
    } else if bits_per_second >= 1_000 {
        format!("{:.0} Kbps", bits_per_second as f64 / 1_000.0)
    } else {
        format!("{bits_per_second} bps")
    }
}

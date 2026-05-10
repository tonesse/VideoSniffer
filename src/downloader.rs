use crate::{
    hls::{
        HlsKey, HlsKeyMethod, HlsSegment, choose_highest_variant, parse_hls_playlist,
        sort_variants_highest_first,
    },
    net::{build_client, bytes_with_retry, send_with_retry, text_with_retry},
    state::{
        DownloadStatus, DownloadTask, HeaderPair, MediaItem, MediaType, SharedState,
        filename_from_url,
    },
};
use aes::Aes128;
use anyhow::{Context, anyhow};
use cbc::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
use reqwest::{
    Client,
    header::{ACCEPT_RANGES, CONTENT_LENGTH, HeaderMap, HeaderName, HeaderValue, RANGE},
};
use sanitize_filename::sanitize;
use std::{collections::HashMap, path::PathBuf, str::FromStr, sync::Arc};
use tokio::{
    fs::{self, File},
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    sync::{Mutex, Semaphore},
    time::{Duration, sleep},
};
use uuid::Uuid;

type Aes128CbcDec = cbc::Decryptor<Aes128>;

pub fn enqueue_download(state: SharedState, media: &MediaItem) {
    let task = DownloadTask {
        id: Uuid::new_v4(),
        media_id: media.id,
        title: media.title.clone(),
        url: media.selected_hls_url(),
        media_type: media.media_type,
        headers: media.headers.clone(),
        status: DownloadStatus::Queued,
        progress: 0.0,
        downloaded_bytes: 0,
        total_bytes: None,
        completed_segments: 0,
        message: "等待下载".to_string(),
    };
    let task_id = task.id;

    state.write(|app| app.tasks.push(task));
    spawn_download_worker(state, task_id);
}

pub fn pause_download(state: SharedState, task_id: Uuid) {
    state.write(|app| {
        if let Some(task) = app.tasks.iter_mut().find(|task| task.id == task_id)
            && matches!(
                task.status,
                DownloadStatus::Queued | DownloadStatus::Downloading
            )
        {
            task.status = DownloadStatus::Paused;
            task.message = "已暂停".to_string();
        }
    });
}

pub fn resume_download(state: SharedState, task_id: Uuid) {
    let should_spawn = state.write(|app| {
        let Some(task) = app.tasks.iter_mut().find(|task| task.id == task_id) else {
            return false;
        };

        if matches!(
            task.status,
            DownloadStatus::Completed | DownloadStatus::Unsupported
        ) {
            return false;
        }

        let was_failed = task.status == DownloadStatus::Failed;
        task.status = DownloadStatus::Downloading;
        task.message = "正在恢复下载".to_string();

        if was_failed || task.progress >= 1.0 {
            task.progress = 0.0;
            task.downloaded_bytes = 0;
            task.completed_segments = 0;
            task.total_bytes = None;
        }

        true
    });

    if should_spawn && !state.is_task_active(task_id) {
        spawn_download_worker(state, task_id);
    }
}

fn spawn_download_worker(state: SharedState, task_id: Uuid) {
    if !state.mark_task_active(task_id) {
        return;
    }

    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(err) => {
                mark_failed(&state, task_id, format!("无法启动下载运行时: {err}"));
                state.mark_task_inactive(task_id);
                return;
            }
        };

        runtime.block_on(async move {
            if let Err(err) = run_task(state.clone(), task_id).await {
                mark_failed(&state, task_id, err.to_string());
            }
            state.mark_task_inactive(task_id);
        });
    });
}

async fn run_task(state: SharedState, task_id: Uuid) -> anyhow::Result<()> {
    let (task, save_dir, part_threads, headers) = state.read(|app| {
        let task = app
            .tasks
            .iter()
            .find(|task| task.id == task_id)
            .cloned()
            .expect("task exists");
        (
            task.clone(),
            app.settings.save_dir.clone(),
            app.settings.part_threads,
            task.headers.clone(),
        )
    });

    if task.media_type == MediaType::Hls {
        return download_hls(state, task_id).await;
    }

    if !matches!(
        task.media_type,
        MediaType::Mp4 | MediaType::Webm | MediaType::Unknown
    ) {
        state.write(|app| {
            if let Some(task) = app.tasks.iter_mut().find(|task| task.id == task_id) {
                task.status = DownloadStatus::Unsupported;
                task.message = "DASH 下载将在下一步加入解析与音视频合并".to_string();
            }
        });
        return Ok(());
    }

    state.write(|app| {
        if let Some(task) = app.tasks.iter_mut().find(|task| task.id == task_id) {
            task.status = DownloadStatus::Downloading;
            task.message = "正在连接服务器".to_string();
        }
    });

    wait_if_paused(&state, task_id).await?;
    fs::create_dir_all(&save_dir).await?;
    let client = build_client()?;
    let header_map = to_header_map(&headers);
    let head = send_with_retry(
        client.head(&task.url).headers(header_map.clone()),
        "获取直链媒体信息",
    )
    .await?;
    let total = head
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    let accepts_ranges = head
        .headers()
        .get(ACCEPT_RANGES)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.eq_ignore_ascii_case("bytes"))
        .unwrap_or(false);

    state.write(|app| {
        if let Some(task) = app.tasks.iter_mut().find(|task| task.id == task_id) {
            task.total_bytes = total;
            task.message = if accepts_ranges {
                format!("服务器支持分片下载，线程数 {part_threads}")
            } else {
                "服务器不支持分片，使用单线程下载".to_string()
            };
        }
    });

    let output = save_dir.join(output_filename(&task));
    match (total, accepts_ranges && part_threads > 1) {
        (Some(total), true) if total > 1024 * 1024 => {
            download_ranged(
                state.clone(),
                task_id,
                client,
                header_map,
                task.url,
                output,
                total,
                part_threads,
            )
            .await?;
        }
        _ => {
            download_single(state.clone(), task_id, client, header_map, task.url, output).await?;
        }
    }

    mark_completed(&state, task_id, "下载完成".to_string());
    Ok(())
}

async fn download_hls(state: SharedState, task_id: Uuid) -> anyhow::Result<()> {
    let (task, save_dir, part_threads, headers) = state.read(|app| {
        let task = app
            .tasks
            .iter()
            .find(|task| task.id == task_id)
            .cloned()
            .expect("task exists");
        (
            task.clone(),
            app.settings.save_dir.clone(),
            app.settings.part_threads.max(1),
            task.headers.clone(),
        )
    });

    state.write(|app| {
        if let Some(task) = app.tasks.iter_mut().find(|task| task.id == task_id) {
            task.status = DownloadStatus::Downloading;
            task.message = "正在解析 HLS 播放列表".to_string();
        }
    });

    wait_if_paused(&state, task_id).await?;
    fs::create_dir_all(&save_dir).await?;
    let client = build_client()?;
    let header_map = to_header_map(&headers);
    let mut playlist_url = task.url.clone();
    let mut playlist_text = text_with_retry(
        client.get(&playlist_url).headers(header_map.clone()),
        "获取 HLS 播放列表",
    )
    .await?;

    let mut playlist = parse_hls_playlist(&playlist_url, &playlist_text)?;
    if playlist.is_master() {
        let mut variants = playlist.variants.clone();
        sort_variants_highest_first(&mut variants);
        let selected_variant = choose_highest_variant(&variants)?;
        playlist_url = selected_variant.url.clone();
        state.write(|app| {
            if let Some(task) = app.tasks.iter_mut().find(|task| task.id == task_id) {
                task.message = format!("自动选择清晰度 {}", selected_variant.label());
            }
        });
        playlist_text = text_with_retry(
            client.get(&playlist_url).headers(header_map.clone()),
            "获取 HLS 清晰度播放列表",
        )
        .await?;
        playlist = parse_hls_playlist(&playlist_url, &playlist_text)?;
    }
    if playlist.segments.is_empty() {
        return Err(anyhow!("HLS 播放列表中没有可下载分片"));
    }

    let task_dir = save_dir.join(".parts").join(task_id.to_string());
    fs::create_dir_all(&task_dir).await?;
    let output = save_dir.join(output_filename(&task));
    let total_segments = playlist.segments.len();

    state.write(|app| {
        if let Some(task) = app.tasks.iter_mut().find(|task| task.id == task_id) {
            task.completed_segments = 0;
            task.total_bytes = None;
            task.message = format!("开始下载 {total_segments} 个 HLS 分片，线程数 {part_threads}");
        }
    });

    let semaphore = Arc::new(Semaphore::new(part_threads));
    let key_cache = Arc::new(Mutex::new(HashMap::<String, Vec<u8>>::new()));
    let mut handles = Vec::with_capacity(total_segments);

    for (index, segment) in playlist.segments.iter().cloned().enumerate() {
        let permit = semaphore.clone().acquire_owned().await?;
        let state = state.clone();
        let client = client.clone();
        let header_map = header_map.clone();
        let task_dir = task_dir.clone();
        let key_cache = key_cache.clone();

        handles.push(tokio::spawn(async move {
            let _permit = permit;
            wait_if_paused(&state, task_id).await?;
            let bytes = bytes_with_retry(
                client.get(&segment.url).headers(header_map.clone()),
                "下载 HLS 分片",
            )
            .await?;
            let bytes =
                decrypt_hls_segment(&client, header_map, &key_cache, &segment, bytes.as_ref())
                    .await?;
            let part_path = task_dir.join(format!("{index:06}.part"));
            fs::write(part_path, &bytes).await?;
            add_hls_segment(
                &state,
                task_id,
                index + 1,
                total_segments,
                bytes.len() as u64,
            );
            Ok::<_, anyhow::Error>(())
        }));
    }

    for handle in handles {
        handle.await??;
    }

    state.write(|app| {
        if let Some(task) = app.tasks.iter_mut().find(|task| task.id == task_id) {
            task.message = "正在合并 HLS 分片".to_string();
        }
    });

    wait_if_paused(&state, task_id).await?;
    let mut output_file = File::create(&output).await?;
    for index in 0..total_segments {
        let part_path = task_dir.join(format!("{index:06}.part"));
        let mut part_file = File::open(&part_path).await?;
        let mut buffer = Vec::new();
        part_file.read_to_end(&mut buffer).await?;
        output_file.write_all(&buffer).await?;
    }
    output_file.flush().await?;
    fs::remove_dir_all(task_dir).await.ok();

    mark_completed(
        &state,
        task_id,
        format!("HLS 合并完成: {}", output.display()),
    );
    Ok(())
}

async fn decrypt_hls_segment(
    client: &Client,
    headers: HeaderMap,
    key_cache: &Mutex<HashMap<String, Vec<u8>>>,
    segment: &HlsSegment,
    bytes: &[u8],
) -> anyhow::Result<Vec<u8>> {
    let Some(key) = &segment.key else {
        return Ok(bytes.to_vec());
    };

    match key.method {
        HlsKeyMethod::Aes128 => {
            let key_bytes = fetch_hls_key(client, headers, key_cache, key).await?;
            Aes128CbcDec::new(key_bytes.as_slice().into(), (&key.iv).into())
                .decrypt_padded_vec_mut::<Pkcs7>(bytes)
                .map_err(|err| anyhow!("HLS 分片 AES-128 解密失败: {err}"))
        }
        HlsKeyMethod::Unsupported => Err(anyhow!("当前 HLS 加密 METHOD 暂不支持")),
    }
}

async fn fetch_hls_key(
    client: &Client,
    headers: HeaderMap,
    key_cache: &Mutex<HashMap<String, Vec<u8>>>,
    key: &HlsKey,
) -> anyhow::Result<Vec<u8>> {
    if let Some(cached) = key_cache.lock().await.get(&key.uri).cloned() {
        return Ok(cached);
    }

    let bytes =
        bytes_with_retry(client.get(&key.uri).headers(headers), "获取 HLS 解密 key").await?;
    if bytes.len() != 16 {
        return Err(anyhow!("HLS AES-128 key 长度不是 16 字节"));
    }

    let key_bytes = bytes.to_vec();
    key_cache
        .lock()
        .await
        .insert(key.uri.clone(), key_bytes.clone());
    Ok(key_bytes)
}

async fn download_single(
    state: SharedState,
    task_id: Uuid,
    client: Client,
    headers: HeaderMap,
    url: String,
    output: PathBuf,
) -> anyhow::Result<()> {
    let mut response = send_with_retry(client.get(url).headers(headers), "下载直链媒体").await?;
    let total = response.content_length();
    let mut file = File::create(output).await?;
    let mut downloaded = 0_u64;

    while let Some(chunk) = response.chunk().await? {
        wait_if_paused(&state, task_id).await?;
        file.write_all(&chunk).await?;
        downloaded += chunk.len() as u64;
        update_progress(&state, task_id, downloaded, total);
    }

    file.flush().await?;
    Ok(())
}

async fn download_ranged(
    state: SharedState,
    task_id: Uuid,
    client: Client,
    headers: HeaderMap,
    url: String,
    output: PathBuf,
    total: u64,
    part_threads: usize,
) -> anyhow::Result<()> {
    let file = File::create(&output).await?;
    file.set_len(total).await?;
    drop(file);

    let ranges = split_ranges(total, part_threads.max(1));
    let semaphore = Arc::new(Semaphore::new(part_threads.max(1)));
    let mut handles = Vec::with_capacity(ranges.len());

    for (start, end) in ranges {
        let permit = semaphore.clone().acquire_owned().await?;
        let state = state.clone();
        let client = client.clone();
        let headers = headers.clone();
        let url = url.clone();
        let output = output.clone();

        handles.push(tokio::spawn(async move {
            let _permit = permit;
            wait_if_paused(&state, task_id).await?;
            let mut part_headers = headers;
            part_headers.insert(
                RANGE,
                HeaderValue::from_str(&format!("bytes={start}-{end}"))
                    .context("invalid range header")?,
            );
            let mut response =
                send_with_retry(client.get(url).headers(part_headers), "下载直链分片").await?;

            let mut file = File::options().write(true).open(output).await?;
            file.seek(std::io::SeekFrom::Start(start)).await?;

            while let Some(chunk) = response.chunk().await? {
                wait_if_paused(&state, task_id).await?;
                file.write_all(&chunk).await?;
                add_downloaded(&state, task_id, chunk.len() as u64, total);
            }
            Ok::<_, anyhow::Error>(())
        }));
    }

    for handle in handles {
        handle.await??;
    }

    Ok(())
}

fn split_ranges(total: u64, parts: usize) -> Vec<(u64, u64)> {
    let chunk = (total / parts as u64).max(1);
    let mut ranges = Vec::new();
    let mut start = 0_u64;

    while start < total {
        let end = (start + chunk - 1).min(total - 1);
        ranges.push((start, end));
        start = end + 1;
    }

    ranges
}

fn update_progress(state: &SharedState, task_id: Uuid, downloaded: u64, total: Option<u64>) {
    state.write(|app| {
        if let Some(task) = app.tasks.iter_mut().find(|task| task.id == task_id) {
            task.downloaded_bytes = downloaded;
            task.total_bytes = total;
            task.progress = total
                .map(|total| downloaded as f32 / total.max(1) as f32)
                .unwrap_or(0.0)
                .clamp(0.0, 1.0);
            task.message = format_bytes(downloaded);
        }
    });
}

fn add_downloaded(state: &SharedState, task_id: Uuid, bytes: u64, total: u64) {
    state.write(|app| {
        if let Some(task) = app.tasks.iter_mut().find(|task| task.id == task_id) {
            task.downloaded_bytes = task.downloaded_bytes.saturating_add(bytes).min(total);
            task.total_bytes = Some(total);
            task.progress = (task.downloaded_bytes as f32 / total.max(1) as f32).clamp(0.0, 1.0);
            task.message = format!(
                "{} / {}",
                format_bytes(task.downloaded_bytes),
                format_bytes(total)
            );
        }
    });
}

fn add_hls_segment(
    state: &SharedState,
    task_id: Uuid,
    segment_number: usize,
    total_segments: usize,
    bytes: u64,
) {
    state.write(|app| {
        if let Some(task) = app.tasks.iter_mut().find(|task| task.id == task_id) {
            task.downloaded_bytes = task.downloaded_bytes.saturating_add(bytes);
            task.completed_segments = task.completed_segments.saturating_add(1);
            task.progress =
                (task.completed_segments as f32 / total_segments.max(1) as f32).clamp(0.0, 1.0);
            task.message = format!(
                "已下载分片 {}/{total_segments}，最近完成 #{segment_number}，累计 {}",
                task.completed_segments,
                format_bytes(task.downloaded_bytes)
            );
        }
    });
}

fn mark_completed(state: &SharedState, task_id: Uuid, message: String) {
    state.write(|app| {
        if let Some(task) = app.tasks.iter_mut().find(|task| task.id == task_id) {
            task.status = DownloadStatus::Completed;
            task.progress = 1.0;
            task.message = message;
        }
    });
}

fn mark_failed(state: &SharedState, task_id: Uuid, message: String) {
    state.write(|app| {
        if let Some(task) = app.tasks.iter_mut().find(|task| task.id == task_id) {
            if task.status != DownloadStatus::Paused {
                task.status = DownloadStatus::Failed;
                task.message = message;
            }
        }
    });
}

async fn wait_if_paused(state: &SharedState, task_id: Uuid) -> anyhow::Result<()> {
    loop {
        let status = state.read(|app| {
            app.tasks
                .iter()
                .find(|task| task.id == task_id)
                .map(|task| task.status)
        });

        match status {
            Some(DownloadStatus::Paused) => sleep(Duration::from_millis(250)).await,
            Some(
                DownloadStatus::Failed | DownloadStatus::Unsupported | DownloadStatus::Completed,
            ) => return Err(anyhow!("下载任务已停止")),
            Some(DownloadStatus::Queued | DownloadStatus::Downloading) => return Ok(()),
            None => return Err(anyhow!("下载任务不存在")),
        }
    }
}

fn to_header_map(headers: &[HeaderPair]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for pair in headers {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_str(&pair.name),
            HeaderValue::from_str(&pair.value),
        ) {
            map.insert(name, value);
        }
    }
    map
}

fn output_filename(task: &DownloadTask) -> String {
    let mut filename = sanitize(&task.title);
    if filename.trim().is_empty() {
        filename = sanitize(filename_from_url(&task.url));
    }
    let extension = match task.media_type {
        MediaType::Hls => "ts",
        MediaType::Webm => "webm",
        _ => "mp4",
    };

    if filename.to_ascii_lowercase().ends_with(extension) {
        filename
    } else {
        format!("{filename}.{extension}")
    }
}

fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let bytes = bytes as f64;

    if bytes >= GB {
        format!("{:.2} GB", bytes / GB)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes / MB)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes / KB)
    } else {
        format!("{bytes:.0} B")
    }
}

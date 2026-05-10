use crate::state::{
    DownloadStatus, DownloadTask, HeaderPair, MediaItem, MediaType, SharedState, filename_from_url,
};
use anyhow::{Context, anyhow};
use reqwest::{
    Client,
    header::{ACCEPT_RANGES, CONTENT_LENGTH, HeaderMap, HeaderName, HeaderValue, RANGE},
};
use sanitize_filename::sanitize;
use std::{path::PathBuf, str::FromStr, sync::Arc};
use tokio::{
    fs::{self, File},
    io::{AsyncSeekExt, AsyncWriteExt},
    sync::Semaphore,
};
use uuid::Uuid;

pub fn enqueue_download(state: SharedState, media: &MediaItem) {
    let task = DownloadTask {
        id: Uuid::new_v4(),
        media_id: media.id,
        title: media.title.clone(),
        url: media.url.clone(),
        media_type: media.media_type,
        status: DownloadStatus::Queued,
        progress: 0.0,
        downloaded_bytes: 0,
        total_bytes: None,
        message: "等待下载".to_string(),
    };
    let task_id = task.id;
    let headers = media.headers.clone();

    state.write(|app| app.tasks.push(task));

    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(err) => {
                mark_failed(&state, task_id, format!("无法启动下载运行时: {err}"));
                return;
            }
        };

        runtime.block_on(async move {
            if let Err(err) = run_task(state.clone(), task_id, headers).await {
                mark_failed(&state, task_id, err.to_string());
            }
        });
    });
}

async fn run_task(
    state: SharedState,
    task_id: Uuid,
    headers: Vec<HeaderPair>,
) -> anyhow::Result<()> {
    let (task, save_dir, part_threads) = state.read(|app| {
        let task = app
            .tasks
            .iter()
            .find(|task| task.id == task_id)
            .cloned()
            .expect("task exists");
        (
            task,
            app.settings.save_dir.clone(),
            app.settings.part_threads,
        )
    });

    if !matches!(
        task.media_type,
        MediaType::Mp4 | MediaType::Webm | MediaType::Unknown
    ) {
        state.write(|app| {
            if let Some(task) = app.tasks.iter_mut().find(|task| task.id == task_id) {
                task.status = DownloadStatus::Unsupported;
                task.message =
                    "第一版先支持直链 MP4/WEBM；HLS/DASH 会在下一步加入解析与合并".to_string();
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

    fs::create_dir_all(&save_dir).await?;
    let client = Client::builder().build()?;
    let header_map = to_header_map(&headers);
    let head = client
        .head(&task.url)
        .headers(header_map.clone())
        .send()
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

    let filename = output_filename(&task);
    let output = save_dir.join(filename);

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

    state.write(|app| {
        if let Some(task) = app.tasks.iter_mut().find(|task| task.id == task_id) {
            task.status = DownloadStatus::Completed;
            task.progress = 1.0;
            task.message = "下载完成".to_string();
        }
    });

    Ok(())
}

async fn download_single(
    state: SharedState,
    task_id: Uuid,
    client: Client,
    headers: HeaderMap,
    url: String,
    output: PathBuf,
) -> anyhow::Result<()> {
    let mut response = client.get(url).headers(headers).send().await?;
    let total = response.content_length();
    let mut file = File::create(output).await?;
    let mut downloaded = 0_u64;

    while let Some(chunk) = response.chunk().await? {
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
            let mut part_headers = headers;
            part_headers.insert(
                RANGE,
                HeaderValue::from_str(&format!("bytes={start}-{end}"))
                    .context("invalid range header")?,
            );
            let mut response = client.get(url).headers(part_headers).send().await?;
            if !response.status().is_success() {
                return Err(anyhow!("range request failed: {}", response.status()));
            }

            let mut file = File::options().write(true).open(output).await?;
            file.seek(std::io::SeekFrom::Start(start)).await?;

            while let Some(chunk) = response.chunk().await? {
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

fn mark_failed(state: &SharedState, task_id: Uuid, message: String) {
    state.write(|app| {
        if let Some(task) = app.tasks.iter_mut().find(|task| task.id == task_id) {
            task.status = DownloadStatus::Failed;
            task.message = message;
        }
    });
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

use crate::state::{HeaderPair, HlsVariant, MediaType, SharedState};
use anyhow::anyhow;
use reqwest::{
    Client,
    header::{HeaderMap, HeaderName, HeaderValue},
};
use std::str::FromStr;
use uuid::Uuid;

pub fn spawn_hls_variant_analysis(state: SharedState, media_id: Uuid) {
    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(err) => {
                update_hls_status(&state, media_id, format!("无法启动 HLS 分析: {err}"));
                return;
            }
        };

        runtime.block_on(async move {
            if let Err(err) = analyze_hls_variants(state.clone(), media_id).await {
                update_hls_status(&state, media_id, format!("HLS 清晰度分析失败: {err}"));
            }
        });
    });
}

async fn analyze_hls_variants(state: SharedState, media_id: Uuid) -> anyhow::Result<()> {
    let Some((url, headers)) = state.read(|app| {
        app.detected
            .iter()
            .find(|item| item.id == media_id && item.media_type == MediaType::Hls)
            .map(|item| (item.url.clone(), item.headers.clone()))
    }) else {
        return Ok(());
    };

    update_hls_status(&state, media_id, "正在分析 HLS 清晰度".to_string());

    let client = Client::builder().build()?;
    let text = client
        .get(&url)
        .headers(to_header_map(&headers))
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let playlist = parse_hls_playlist(&url, &text)?;

    if playlist.variants.is_empty() {
        update_hls_status(&state, media_id, "单清晰度 HLS".to_string());
        return Ok(());
    }

    let mut variants = playlist.variants;
    sort_variants_highest_first(&mut variants);
    let selected = variants.first().map(|variant| variant.url.clone());
    let message = format!("已发现 {} 个清晰度，默认最高码率", variants.len());

    state.write(|app| {
        if let Some(item) = app.detected.iter_mut().find(|item| item.id == media_id) {
            item.hls_variants = variants;
            item.selected_hls_variant_url = selected;
            item.hls_status = Some(message);
        }
    });

    Ok(())
}

pub struct HlsPlaylist {
    pub segments: Vec<String>,
    pub variants: Vec<HlsVariant>,
    pub encrypted: bool,
}

impl HlsPlaylist {
    pub fn is_master(&self) -> bool {
        !self.variants.is_empty()
    }
}

pub fn parse_hls_playlist(base_url: &str, text: &str) -> anyhow::Result<HlsPlaylist> {
    let base = reqwest::Url::parse(base_url)?;
    let mut segments = Vec::new();
    let mut variants = Vec::new();
    let mut encrypted = false;
    let mut pending_variant: Option<HlsVariantInfo> = None;

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("#EXT-X-KEY") {
            encrypted = true;
            continue;
        }
        if let Some(attrs) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            pending_variant = Some(parse_variant_info(attrs));
            continue;
        }
        if line.starts_with('#') {
            continue;
        }

        let url = base.join(line)?.to_string();
        if let Some(info) = pending_variant.take() {
            variants.push(HlsVariant {
                url,
                bandwidth: info.bandwidth,
                resolution: info.resolution,
                codecs: info.codecs,
            });
        } else {
            segments.push(url);
        }
    }

    Ok(HlsPlaylist {
        segments,
        variants,
        encrypted,
    })
}

pub fn choose_highest_variant(variants: &[HlsVariant]) -> anyhow::Result<&HlsVariant> {
    variants
        .iter()
        .max_by_key(|variant| variant.bandwidth.unwrap_or(0))
        .ok_or_else(|| anyhow!("HLS master playlist 没有可用清晰度"))
}

pub fn sort_variants_highest_first(variants: &mut [HlsVariant]) {
    variants.sort_by(|a, b| {
        b.bandwidth
            .unwrap_or(0)
            .cmp(&a.bandwidth.unwrap_or(0))
            .then_with(|| b.resolution.cmp(&a.resolution))
    });
}

#[derive(Default)]
struct HlsVariantInfo {
    bandwidth: Option<u64>,
    resolution: Option<String>,
    codecs: Option<String>,
}

fn parse_variant_info(attrs: &str) -> HlsVariantInfo {
    let mut info = HlsVariantInfo::default();

    for (key, value) in parse_attribute_list(attrs) {
        match key.as_str() {
            "BANDWIDTH" => info.bandwidth = value.parse::<u64>().ok(),
            "AVERAGE-BANDWIDTH" if info.bandwidth.is_none() => {
                info.bandwidth = value.parse::<u64>().ok();
            }
            "RESOLUTION" => info.resolution = Some(value),
            "CODECS" => info.codecs = Some(value),
            _ => {}
        }
    }

    info
}

fn parse_attribute_list(input: &str) -> Vec<(String, String)> {
    let mut attrs = Vec::new();
    let mut key = String::new();
    let mut value = String::new();
    let mut reading_key = true;
    let mut in_quotes = false;

    for ch in input.chars() {
        match ch {
            '=' if reading_key => reading_key = false,
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => {
                if !key.trim().is_empty() {
                    attrs.push((key.trim().to_string(), clean_attr_value(&value)));
                }
                key.clear();
                value.clear();
                reading_key = true;
            }
            _ if reading_key => key.push(ch),
            _ => value.push(ch),
        }
    }

    if !key.trim().is_empty() {
        attrs.push((key.trim().to_string(), clean_attr_value(&value)));
    }

    attrs
}

fn clean_attr_value(value: &str) -> String {
    value.trim().trim_matches('"').to_string()
}

fn update_hls_status(state: &SharedState, media_id: Uuid, message: String) {
    state.write(|app| {
        if let Some(item) = app.detected.iter_mut().find(|item| item.id == media_id) {
            item.hls_status = Some(message);
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

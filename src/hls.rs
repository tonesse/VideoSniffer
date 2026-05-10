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
    pub segments: Vec<HlsSegment>,
    pub variants: Vec<HlsVariant>,
}

impl HlsPlaylist {
    pub fn is_master(&self) -> bool {
        !self.variants.is_empty()
    }
}

#[derive(Clone)]
pub struct HlsSegment {
    pub url: String,
    pub key: Option<HlsKey>,
}

#[derive(Clone)]
pub struct HlsKey {
    pub method: HlsKeyMethod,
    pub uri: String,
    pub iv: [u8; 16],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HlsKeyMethod {
    Aes128,
    Unsupported,
}

pub fn parse_hls_playlist(base_url: &str, text: &str) -> anyhow::Result<HlsPlaylist> {
    let base = reqwest::Url::parse(base_url)?;
    let mut segments = Vec::new();
    let mut variants = Vec::new();
    let mut pending_variant: Option<HlsVariantInfo> = None;
    let mut current_key: Option<PendingHlsKey> = None;
    let mut next_sequence = 0_u64;

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(value) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            next_sequence = value.parse::<u64>().unwrap_or(0);
            continue;
        }
        if let Some(attrs) = line.strip_prefix("#EXT-X-KEY:") {
            current_key = parse_key_info(&base, attrs)?;
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
            segments.push(HlsSegment {
                url,
                key: current_key
                    .as_ref()
                    .map(|key| key.to_segment_key(next_sequence)),
            });
            next_sequence = next_sequence.saturating_add(1);
        }
    }

    Ok(HlsPlaylist { segments, variants })
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

#[derive(Clone)]
struct PendingHlsKey {
    method: HlsKeyMethod,
    uri: String,
    iv: Option<[u8; 16]>,
}

impl PendingHlsKey {
    fn to_segment_key(&self, sequence: u64) -> HlsKey {
        HlsKey {
            method: self.method,
            uri: self.uri.clone(),
            iv: self.iv.unwrap_or_else(|| sequence_to_iv(sequence)),
        }
    }
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

fn parse_key_info(base: &reqwest::Url, attrs: &str) -> anyhow::Result<Option<PendingHlsKey>> {
    let mut method = None;
    let mut uri = None;
    let mut iv = None;

    for (key, value) in parse_attribute_list(attrs) {
        match key.as_str() {
            "METHOD" => method = Some(value),
            "URI" => uri = Some(base.join(&value)?.to_string()),
            "IV" => iv = Some(parse_iv(&value)?),
            _ => {}
        }
    }

    let Some(method) = method else {
        return Ok(None);
    };
    if method.eq_ignore_ascii_case("NONE") {
        return Ok(None);
    }

    let method = if method.eq_ignore_ascii_case("AES-128") {
        HlsKeyMethod::Aes128
    } else {
        HlsKeyMethod::Unsupported
    };

    let Some(uri) = uri else {
        return Ok(None);
    };

    Ok(Some(PendingHlsKey { method, uri, iv }))
}

fn parse_iv(value: &str) -> anyhow::Result<[u8; 16]> {
    let hex = value
        .trim()
        .strip_prefix("0x")
        .or_else(|| value.trim().strip_prefix("0X"))
        .unwrap_or_else(|| value.trim());
    if hex.len() > 32 {
        return Err(anyhow!("HLS EXT-X-KEY IV 长度超过 16 字节"));
    }

    let padded = format!("{hex:0>32}");
    let mut bytes = [0_u8; 16];
    for (index, chunk) in padded.as_bytes().chunks(2).enumerate() {
        let hex_pair = std::str::from_utf8(chunk)?;
        bytes[index] = u8::from_str_radix(hex_pair, 16)?;
    }
    Ok(bytes)
}

fn sequence_to_iv(sequence: u64) -> [u8; 16] {
    let mut iv = [0_u8; 16];
    iv[8..].copy_from_slice(&sequence.to_be_bytes());
    iv
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_master_playlist_variants() {
        let playlist = parse_hls_playlist(
            "https://cdn.example.com/master.m3u8",
            r#"
#EXTM3U
#EXT-X-STREAM-INF:BANDWIDTH=900000,RESOLUTION=854x480,CODECS="avc1.4d401f,mp4a.40.2"
480p/index.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=4500000,RESOLUTION=1920x1080
1080p/index.m3u8
"#,
        )
        .expect("playlist parses");

        assert!(playlist.is_master());
        assert_eq!(playlist.variants.len(), 2);
        assert_eq!(
            choose_highest_variant(&playlist.variants).unwrap().url,
            "https://cdn.example.com/1080p/index.m3u8"
        );
    }

    #[test]
    fn parses_aes_128_key_with_explicit_iv() {
        let playlist = parse_hls_playlist(
            "https://cdn.example.com/live/index.m3u8",
            r#"
#EXTM3U
#EXT-X-KEY:METHOD=AES-128,URI="keys/key.bin",IV=0x0000000000000000000000000000002A
#EXTINF:4.0,
seg-1.ts
"#,
        )
        .expect("playlist parses");

        let key = playlist.segments[0].key.as_ref().expect("segment key");
        assert_eq!(key.uri, "https://cdn.example.com/live/keys/key.bin");
        assert_eq!(key.method, HlsKeyMethod::Aes128);
        assert_eq!(key.iv[15], 42);
    }

    #[test]
    fn uses_media_sequence_as_default_iv() {
        let playlist = parse_hls_playlist(
            "https://cdn.example.com/live/index.m3u8",
            r#"
#EXTM3U
#EXT-X-MEDIA-SEQUENCE:99
#EXT-X-KEY:METHOD=AES-128,URI="key.bin"
#EXTINF:4.0,
seg-99.ts
#EXTINF:4.0,
seg-100.ts
"#,
        )
        .expect("playlist parses");

        let first = playlist.segments[0].key.as_ref().expect("first key");
        let second = playlist.segments[1].key.as_ref().expect("second key");
        assert_eq!(&first.iv[8..], &99_u64.to_be_bytes());
        assert_eq!(&second.iv[8..], &100_u64.to_be_bytes());
    }
}

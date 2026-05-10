use anyhow::{Context, anyhow};
use reqwest::{Client, RequestBuilder, Response, StatusCode};
use std::time::Duration;
use tokio::time::sleep;

const MAX_ATTEMPTS: usize = 4;

pub fn build_client() -> anyhow::Result<Client> {
    Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(45))
        .pool_idle_timeout(Duration::from_secs(30))
        .build()
        .context("创建 HTTP 客户端失败")
}

pub async fn send_with_retry(
    builder: RequestBuilder,
    description: &str,
) -> anyhow::Result<Response> {
    let mut last_error = None;

    for attempt in 1..=MAX_ATTEMPTS {
        let Some(request) = builder.try_clone() else {
            return Err(anyhow!("{description}: 请求无法克隆，不能重试"));
        };

        match request.send().await {
            Ok(response) if response.status().is_success() => return Ok(response),
            Ok(response) => {
                let status = response.status();
                if should_retry_status(status) && attempt < MAX_ATTEMPTS {
                    sleep(backoff(attempt)).await;
                    continue;
                }
                return Err(http_status_error(description, status));
            }
            Err(err) => {
                let retryable =
                    err.is_timeout() || err.is_connect() || err.is_request() || err.is_body();
                last_error = Some(classify_reqwest_error(description, &err));
                if retryable && attempt < MAX_ATTEMPTS {
                    sleep(backoff(attempt)).await;
                    continue;
                }
                return Err(last_error.unwrap_or_else(|| anyhow!("{description}: 网络请求失败")));
            }
        }
    }

    Err(last_error.unwrap_or_else(|| anyhow!("{description}: 超过最大重试次数")))
}

pub async fn text_with_retry(builder: RequestBuilder, description: &str) -> anyhow::Result<String> {
    send_with_retry(builder, description)
        .await?
        .text()
        .await
        .with_context(|| format!("{description}: 读取文本响应失败"))
}

pub async fn bytes_with_retry(
    builder: RequestBuilder,
    description: &str,
) -> anyhow::Result<bytes::Bytes> {
    send_with_retry(builder, description)
        .await?
        .bytes()
        .await
        .with_context(|| format!("{description}: 读取响应内容失败"))
}

fn should_retry_status(status: StatusCode) -> bool {
    status == StatusCode::REQUEST_TIMEOUT
        || status == StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
}

fn backoff(attempt: usize) -> Duration {
    Duration::from_millis(500 * 2_u64.pow((attempt - 1) as u32))
}

fn http_status_error(description: &str, status: StatusCode) -> anyhow::Error {
    let hint = match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            "请求被拒绝，Cookie/Referer/User-Agent 可能已过期或不完整"
        }
        StatusCode::NOT_FOUND => "资源不存在，视频地址可能已失效",
        StatusCode::TOO_MANY_REQUESTS => "请求过多，源站限流",
        status if status.is_server_error() => "源站服务器异常，请稍后重试",
        _ => "源站返回非成功状态",
    };
    anyhow!("{description}: HTTP {status}，{hint}")
}

fn classify_reqwest_error(description: &str, err: &reqwest::Error) -> anyhow::Error {
    if err.is_timeout() {
        anyhow!("{description}: 请求超时，目标源可能响应过慢或网络不稳定")
    } else if err.is_connect() {
        anyhow!("{description}: 无法连接目标源，请检查网络、DNS 或代理")
    } else if err.is_body() {
        anyhow!("{description}: 响应读取中断，网络连接可能被重置")
    } else {
        anyhow!("{description}: 网络请求失败: {err}")
    }
}

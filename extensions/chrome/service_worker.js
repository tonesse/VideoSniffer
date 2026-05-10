const LOCAL_ENDPOINT = "http://127.0.0.1:37651/api/media";
const requestHeaderCache = new Map();
const REQUEST_HEADER_TTL_MS = 2 * 60 * 1000;

const MEDIA_PATTERNS = [
  ".m3u8",
  ".mpd",
  ".mp4",
  ".webm"
];

const FORWARDED_REQUEST_HEADERS = new Set([
  "accept",
  "accept-language",
  "cookie",
  "origin",
  "range",
  "referer",
  "user-agent"
]);

function looksLikeMedia(details) {
  const url = details.url.toLowerCase();
  const mime = (details.responseHeaders || [])
    .find((header) => header.name.toLowerCase() === "content-type")
    ?.value?.toLowerCase() || "";

  return MEDIA_PATTERNS.some((pattern) => url.includes(pattern)) ||
    mime.includes("video/") ||
    mime.includes("mpegurl") ||
    mime.includes("dash");
}

function headerValue(headers, name) {
  return (headers || []).find((header) => header.name.toLowerCase() === name)?.value;
}

function contentLength(headers) {
  const value = headerValue(headers, "content-length");
  const parsed = value ? Number.parseInt(value, 10) : Number.NaN;
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : null;
}

function captureForwardedHeaders(details) {
  const headers = (details.requestHeaders || [])
    .filter((header) => FORWARDED_REQUEST_HEADERS.has(header.name.toLowerCase()))
    .map((header) => ({ name: header.name, value: header.value || "" }))
    .filter((header) => header.value);

  requestHeaderCache.set(details.requestId, {
    headers,
    capturedAt: Date.now()
  });
  cleanupHeaderCache();
}

function cleanupHeaderCache() {
  const now = Date.now();
  for (const [requestId, entry] of requestHeaderCache.entries()) {
    if (now - entry.capturedAt > REQUEST_HEADER_TTL_MS) {
      requestHeaderCache.delete(requestId);
    }
  }
}

chrome.webRequest.onBeforeSendHeaders.addListener(
  captureForwardedHeaders,
  { urls: ["<all_urls>"] },
  ["requestHeaders", "extraHeaders"]
);

chrome.webRequest.onHeadersReceived.addListener(
  async (details) => {
    if (!looksLikeMedia(details)) {
      return;
    }

    const tab = details.tabId >= 0
      ? await chrome.tabs.get(details.tabId).catch(() => null)
      : null;

    const cached = requestHeaderCache.get(details.requestId);
    requestHeaderCache.delete(details.requestId);
    const capturedHeaders = cached?.headers || [];
    const hasHeader = (name) =>
      capturedHeaders.some((header) => header.name.toLowerCase() === name);

    const payload = {
      url: details.url,
      page_url: tab?.url || details.initiator || null,
      title: tab?.title || null,
      mime_type: headerValue(details.responseHeaders, "content-type") || null,
      content_length: contentLength(details.responseHeaders),
      method: details.method,
      request_headers: [
        ...capturedHeaders,
        hasHeader("referer") ? null : { name: "Referer", value: tab?.url || details.initiator || "" },
        hasHeader("user-agent") ? null : { name: "User-Agent", value: navigator.userAgent }
      ].filter((header) => header?.value)
    };

    fetch(LOCAL_ENDPOINT, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(payload)
    }).catch(() => {});
  },
  { urls: ["<all_urls>"] },
  ["responseHeaders", "extraHeaders"]
);

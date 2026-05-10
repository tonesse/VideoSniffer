const LOCAL_ENDPOINT = "http://127.0.0.1:37651/api/media";

const MEDIA_PATTERNS = [
  ".m3u8",
  ".mpd",
  ".mp4",
  ".webm"
];

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

chrome.webRequest.onHeadersReceived.addListener(
  async (details) => {
    if (!looksLikeMedia(details)) {
      return;
    }

    const tab = details.tabId >= 0
      ? await chrome.tabs.get(details.tabId).catch(() => null)
      : null;

    const payload = {
      url: details.url,
      page_url: tab?.url || details.initiator || null,
      title: tab?.title || null,
      mime_type: headerValue(details.responseHeaders, "content-type") || null,
      method: details.method,
      request_headers: [
        { name: "Referer", value: tab?.url || details.initiator || "" },
        { name: "User-Agent", value: navigator.userAgent }
      ].filter((header) => header.value)
    };

    fetch(LOCAL_ENDPOINT, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(payload)
    }).catch(() => {});
  },
  { urls: ["<all_urls>"] },
  ["responseHeaders"]
);

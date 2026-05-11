# VideoSniffer

VideoSniffer 是一个基于 Rust 的 Windows 桌面视频嗅探与下载工具。它通过浏览器扩展捕获视频网络请求，并在桌面应用中提供嗅探列表、手动新建任务、多线程下载、断点恢复和已下载记录管理。

## 当前功能

- 浏览器视频资源嗅探，支持 Chrome / Edge 扩展上报视频地址。
- 支持手动新建下载任务，直接输入视频地址下载。
- 支持 MP4、WEBM、HLS 资源下载。
- 直链媒体在服务端支持 `Accept-Ranges: bytes` 时使用多线程分片下载。
- HLS 支持并发分片下载、合并为 TS，并在可用时通过 ffmpeg remux 为 MP4。
- HLS master playlist 会自动选择最高码率，也可以在 UI 中切换清晰度。
- 支持非 DRM 的 HLS AES-128 解密。
- 下载任务支持暂停、恢复、失败重试和断点续传。
- 分片下载进度会写入临时 manifest，应用重启后可以跳过已完成分片。
- 支持设置保存目录、线程数量、请求重试次数和分片重试次数。
- 已下载记录支持删除，双击文件名可打开本地文件；如果文件不存在，会自动删除记录。
- 嗅探资源地址支持复制。

## 运行

```powershell
cargo run
```

## 加载浏览器扩展

1. 打开 Chrome 或 Edge 的扩展管理页面。
2. 开启“开发者模式”。
3. 选择“加载已解压的扩展程序”。
4. 选择项目目录下的 `extensions/chrome`。
5. 启动 VideoSniffer 后，在浏览器中播放视频即可嗅探。

扩展会把视频地址、页面来源、标题、Content-Type、Content-Length 以及下载所需的 Cookie、Referer、Origin、User-Agent 等请求头发送到本地应用。

## 打包

直接运行：

```bat
scripts\package.bat
```

打包结果会生成在 `dist` 目录中：

- `dist\VideoSniffer\VideoSniffer.exe`
- `dist\VideoSniffer\bin\ffmpeg.exe`，如果本地存在 ffmpeg
- `dist\browser-extension\VideoSniffer-Chrome-Extension.zip`

`dist`、`target` 和本地 ffmpeg 工具文件不会提交到仓库。

## FFmpeg

HLS 下载完成后会优先尝试通过 ffmpeg 将 TS remux 为 MP4。应用按以下顺序查找 ffmpeg：

1. `VIDEOSNIFFER_FFMPEG` 环境变量。
2. 打包目录中的 `ffmpeg.exe`。
3. 打包目录中的 `bin\ffmpeg.exe`。
4. 项目或打包目录中的 `third_party\ffmpeg\ffmpeg.exe`。
5. 系统 `PATH` 中的 `ffmpeg`。

如果没有找到 ffmpeg，应用会保留 TS 文件。

## 注意事项

- 某些网站的视频地址有时效性，过期后需要重新嗅探。
- 如果更新了扩展代码，需要在浏览器扩展管理页重新加载扩展。
- 嗅探列表只保留当前页面捕获到且尚未下载的资源，离开嗅探页后未下载资源会被清理。

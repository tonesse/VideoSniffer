use crate::{
    downloader::{enqueue_download, pause_download, resume_download},
    sniffer::spawn_sniffer_server,
    state::{DownloadStatus, MediaItem, MediaType, SharedState},
};
use eframe::egui;
use serde_json::Value;
use std::{collections::HashSet, fs, path::PathBuf, sync::Arc};
use uuid::Uuid;

pub struct VideoSnifferApp {
    state: SharedState,
    save_dir_input: String,
    last_revision: u64,
    selected_task_id: Option<Uuid>,
}

impl VideoSnifferApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_chinese_font(&cc.egui_ctx);

        let state = SharedState::new();
        let (save_dir_input, port) = state.read(|app| {
            (
                app.settings.save_dir.to_string_lossy().to_string(),
                app.settings.listen_port,
            )
        });
        spawn_sniffer_server(state.clone(), port);

        Self {
            state,
            save_dir_input,
            last_revision: 0,
            selected_task_id: None,
        }
    }
}

impl eframe::App for VideoSnifferApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let revision = self.state.revision();
        if revision != self.last_revision {
            self.last_revision = revision;
            ctx.request_repaint();
        }

        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("VideoSniffer");
                ui.separator();
                ui.label("浏览器视频嗅探与多线程下载");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label("监听 127.0.0.1:37651");
                });
            });
        });

        egui::SidePanel::left("sidebar")
            .resizable(false)
            .default_width(220.0)
            .show(ctx, |ui| {
                ui.heading("状态");
                ui.add_space(8.0);
                self.state.read(|app| {
                    ui.label(format!("发现视频: {}", app.detected.len()));
                    ui.label(format!("下载任务: {}", app.tasks.len()));
                    ui.label(format!("并发任务: {}", app.settings.max_tasks));
                    ui.label(format!("分片线程: {}", app.settings.part_threads));
                    ui.label(format!("最小媒体: {} MB", app.settings.min_media_size_mb));
                });

                ui.separator();
                ui.label("保存位置");
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.save_dir_input)
                        .desired_width(f32::INFINITY),
                );
                if response.lost_focus() {
                    let value = self.save_dir_input.clone();
                    self.state.write(|app| app.settings.save_dir = value.into());
                }
                if ui.button("选择文件夹...").clicked()
                    && let Some(folder) = rfd::FileDialog::new()
                        .set_title("选择视频保存位置")
                        .set_directory(PathBuf::from(&self.save_dir_input))
                        .pick_folder()
                {
                    self.save_dir_input = folder.to_string_lossy().to_string();
                    let value = folder;
                    self.state.write(|app| app.settings.save_dir = value);
                }

                ui.add_space(8.0);
                ui.label("嗅探记录、任务和设置会自动保存到本地。应用重启后可恢复任务。");
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.columns(2, |columns| {
                draw_detected(&self.state, &mut columns[0]);
                draw_tasks(&self.state, &mut columns[1], &mut self.selected_task_id);
                columns[1].separator();
                draw_task_detail(&self.state, &mut columns[1], self.selected_task_id);
            });
        });
    }
}

fn draw_detected(state: &SharedState, ui: &mut egui::Ui) {
    ui.heading("嗅探到的视频");
    ui.add_space(6.0);

    let items = state.read(|app| app.detected.iter().cloned().collect::<Vec<_>>());
    if items.is_empty() {
        ui.group(|ui| {
            ui.label("还没有捕获到视频请求。");
            ui.label("加载 extensions/chrome 到 Chrome/Edge 后，打开视频网站播放视频即可测试。");
        });
        return;
    }

    egui::ScrollArea::vertical().show(ui, |ui| {
        for item in items {
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.label(item.media_type.label());
                    ui.label(item.detected_at.format("%H:%M:%S").to_string());
                });
                ui.strong(&item.title);
                small_url(ui, "地址", &item.url);
                if let Some(page_url) = &item.page_url {
                    small_url(ui, "来源页", page_url);
                }
                if item.media_type == MediaType::Hls {
                    draw_hls_quality_selector(state, ui, &item);
                }

                ui.horizontal(|ui| {
                    let supported = matches!(
                        item.media_type,
                        MediaType::Hls | MediaType::Mp4 | MediaType::Webm | MediaType::Unknown
                    );
                    if ui
                        .add_enabled(supported, egui::Button::new("下载"))
                        .clicked()
                    {
                        enqueue_download(state.clone(), &item);
                    }
                    if !supported {
                        ui.label("DASH 解析将在下一步加入");
                    }
                });
            });
            ui.add_space(8.0);
        }
    });
}

fn draw_hls_quality_selector(state: &SharedState, ui: &mut egui::Ui, item: &MediaItem) {
    if item.hls_variants.is_empty() {
        if let Some(status) = &item.hls_status {
            ui.small(status);
        } else {
            ui.small("正在等待 HLS 清晰度分析");
        }
        return;
    }

    let selected_url = item
        .selected_hls_variant_url
        .clone()
        .or_else(|| item.hls_variants.first().map(|variant| variant.url.clone()));
    let selected_label = selected_url
        .as_ref()
        .and_then(|url| {
            item.hls_variants
                .iter()
                .find(|variant| &variant.url == url)
                .map(|variant| variant.label())
        })
        .unwrap_or_else(|| "自动最高码率".to_string());

    ui.horizontal(|ui| {
        ui.label("清晰度");
        egui::ComboBox::from_id_salt(format!("hls-quality-{}", item.id))
            .selected_text(selected_label)
            .show_ui(ui, |ui| {
                for variant in &item.hls_variants {
                    let label = variant.label();
                    let selected = selected_url.as_deref() == Some(variant.url.as_str());
                    if ui.selectable_label(selected, label).clicked() {
                        let url = variant.url.clone();
                        state.write(|app| {
                            if let Some(media) =
                                app.detected.iter_mut().find(|media| media.id == item.id)
                            {
                                media.selected_hls_variant_url = Some(url);
                                media.hls_status = Some("已切换清晰度".to_string());
                            }
                        });
                    }
                }
            });
    });

    if let Some(status) = &item.hls_status {
        ui.small(status);
    }
}

fn draw_tasks(state: &SharedState, ui: &mut egui::Ui, selected_task_id: &mut Option<Uuid>) {
    ui.heading("下载队列");
    ui.add_space(6.0);

    let tasks = state.read(|app| app.tasks.clone());
    if tasks.is_empty() {
        ui.group(|ui| {
            ui.label("暂无下载任务。");
        });
        return;
    }

    egui::ScrollArea::vertical().show(ui, |ui| {
        for task in tasks {
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.strong(&task.title);
                    status_label(ui, task.status);
                });
                small_url(ui, "地址", &task.url);
                ui.add(egui::ProgressBar::new(task.progress).show_percentage());
                ui.label(&task.message);
                if let Some(total) = task.total_bytes {
                    ui.small(format!("总大小: {}", human_bytes(total)));
                }
                ui.small(format!("媒体 ID: {}", task.media_id));

                ui.horizontal(|ui| {
                    if matches!(
                        task.status,
                        DownloadStatus::Queued | DownloadStatus::Downloading
                    ) && ui.button("暂停").clicked()
                    {
                        pause_download(state.clone(), task.id);
                    }
                    if matches!(
                        task.status,
                        DownloadStatus::Paused | DownloadStatus::Failed | DownloadStatus::Queued
                    ) && ui.button("恢复").clicked()
                    {
                        resume_download(state.clone(), task.id);
                    }
                    if ui.button("详情").clicked() {
                        *selected_task_id = Some(task.id);
                    }
                });
            });
            ui.add_space(8.0);
        }
    });
}

fn draw_task_detail(state: &SharedState, ui: &mut egui::Ui, selected_task_id: Option<Uuid>) {
    ui.heading("任务详情");
    ui.add_space(6.0);

    let Some(task_id) = selected_task_id else {
        ui.group(|ui| {
            ui.label("选择一个下载任务查看分片状态。");
        });
        return;
    };

    let detail = state.read(|app| {
        let task = app.tasks.iter().find(|task| task.id == task_id).cloned();
        let save_dir = app.settings.save_dir.clone();
        (task, save_dir)
    });

    let Some(task) = detail.0 else {
        ui.group(|ui| {
            ui.label("任务不存在。");
        });
        return;
    };

    let manifest_path = detail
        .1
        .join(".parts")
        .join(task.id.to_string())
        .join("manifest.json");
    let manifest = read_manifest_summary(&manifest_path);

    ui.group(|ui| {
        ui.horizontal(|ui| {
            ui.strong(&task.title);
            status_label(ui, task.status);
        });
        small_url(ui, "地址", &task.url);
        ui.label(format!("任务 ID: {}", task.id));
        ui.label(format!("媒体类型: {}", task.media_type.label()));
        ui.label(format!("进度: {:.1}%", task.progress * 100.0));
        ui.label(format!("已下载: {}", human_bytes(task.downloaded_bytes)));
        ui.label(format!("临时记录: {}", manifest_path.display()));

        match manifest {
            Some(summary) => {
                ui.separator();
                ui.label(format!("分片类型: {}", summary.kind));
                if let Some(total) = summary.total_parts {
                    ui.label(format!("总分片: {total}"));
                }
                ui.label(format!("已完成分片: {}", summary.completed_parts));
                ui.label(format!(
                    "待下载分片: {}",
                    summary.pending_parts.unwrap_or(0)
                ));
                if summary.downloaded_bytes > 0 {
                    ui.label(format!(
                        "manifest 已记录: {}",
                        human_bytes(summary.downloaded_bytes)
                    ));
                }

                egui::ScrollArea::vertical()
                    .max_height(160.0)
                    .show(ui, |ui| {
                        for line in summary.preview_lines {
                            ui.small(line);
                        }
                    });
            }
            None => {
                ui.separator();
                if task.status == DownloadStatus::Completed {
                    ui.label("任务已完成，临时分片记录已清理。");
                } else {
                    ui.label("还没有临时分片记录，任务开始分片下载后会生成。");
                }
            }
        }
    });
}

struct ManifestSummary {
    kind: String,
    total_parts: Option<usize>,
    completed_parts: usize,
    pending_parts: Option<usize>,
    downloaded_bytes: u64,
    preview_lines: Vec<String>,
}

fn read_manifest_summary(path: &PathBuf) -> Option<ManifestSummary> {
    let json = fs::read_to_string(path).ok()?;
    let value = serde_json::from_str::<Value>(&json).ok()?;
    let kind = value
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("Unknown")
        .to_string();
    let total_parts = value
        .get("total_parts")
        .and_then(Value::as_u64)
        .map(|value| value as usize)
        .filter(|value| *value > 0);

    if kind == "Hls" {
        let parts = value
            .get("hls_parts")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let completed = parts
            .iter()
            .filter(|part| {
                part.get("completed")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            })
            .count();
        let downloaded_bytes = parts
            .iter()
            .filter(|part| {
                part.get("completed")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            })
            .filter_map(|part| part.get("bytes").and_then(Value::as_u64))
            .sum();
        let preview_lines = parts
            .iter()
            .take(80)
            .map(|part| {
                let index = part.get("index").and_then(Value::as_u64).unwrap_or(0);
                let bytes = part.get("bytes").and_then(Value::as_u64).unwrap_or(0);
                let done = part
                    .get("completed")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                format!(
                    "#{index}: {} · {}",
                    if done { "完成" } else { "待下载" },
                    human_bytes(bytes)
                )
            })
            .collect::<Vec<_>>();

        return Some(ManifestSummary {
            kind,
            total_parts,
            completed_parts: completed,
            pending_parts: total_parts.map(|total| total.saturating_sub(completed)),
            downloaded_bytes,
            preview_lines,
        });
    }

    let parts = value
        .get("range_parts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let completed_ranges = parts
        .iter()
        .filter(|part| {
            part.get("completed")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    let downloaded_bytes = completed_ranges
        .iter()
        .map(|part| {
            let start = part.get("start").and_then(Value::as_u64).unwrap_or(0);
            let end = part.get("end").and_then(Value::as_u64).unwrap_or(0);
            end.saturating_sub(start).saturating_add(1)
        })
        .sum();
    let preview_lines = completed_ranges
        .iter()
        .take(80)
        .map(|part| {
            let start = part.get("start").and_then(Value::as_u64).unwrap_or(0);
            let end = part.get("end").and_then(Value::as_u64).unwrap_or(0);
            format!(
                "{start}-{end}: 完成 · {}",
                human_bytes(end.saturating_sub(start).saturating_add(1))
            )
        })
        .collect::<Vec<_>>();
    let unique_ranges = completed_ranges
        .iter()
        .map(|part| {
            (
                part.get("start").and_then(Value::as_u64).unwrap_or(0),
                part.get("end").and_then(Value::as_u64).unwrap_or(0),
            )
        })
        .collect::<HashSet<_>>();

    Some(ManifestSummary {
        kind,
        total_parts,
        completed_parts: unique_ranges.len(),
        pending_parts: None,
        downloaded_bytes,
        preview_lines,
    })
}

fn status_label(ui: &mut egui::Ui, status: DownloadStatus) {
    let color = match status {
        DownloadStatus::Queued => egui::Color32::GRAY,
        DownloadStatus::Downloading => egui::Color32::from_rgb(37, 99, 235),
        DownloadStatus::Paused => egui::Color32::from_rgb(217, 119, 6),
        DownloadStatus::Completed => egui::Color32::from_rgb(22, 163, 74),
        DownloadStatus::Failed => egui::Color32::from_rgb(220, 38, 38),
        DownloadStatus::Unsupported => egui::Color32::from_rgb(217, 119, 6),
    };
    ui.colored_label(color, status.label());
}

fn install_chinese_font(ctx: &egui::Context) {
    let Some(font_bytes) = load_system_chinese_font() else {
        return;
    };

    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "system_chinese".to_owned(),
        Arc::new(egui::FontData::from_owned(font_bytes)),
    );

    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .insert(0, "system_chinese".to_owned());
    }

    ctx.set_fonts(fonts);
}

fn load_system_chinese_font() -> Option<Vec<u8>> {
    let candidates = [
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\msyh.ttf",
        r"C:\Windows\Fonts\simhei.ttf",
        r"C:\Windows\Fonts\simsun.ttc",
    ];

    candidates.iter().find_map(|path| fs::read(path).ok())
}

fn small_url(ui: &mut egui::Ui, label: &str, url: &str) {
    let text = format!("{label}: {}", compact_text(url, 120));
    let response = ui.add(egui::Label::new(egui::RichText::new(text).small()).wrap());
    response.on_hover_text(url);
}

fn compact_text(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let prefix = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{prefix}...")
    } else {
        prefix
    }
}

fn human_bytes(bytes: u64) -> String {
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

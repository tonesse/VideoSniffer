use crate::{
    downloader::{enqueue_download, pause_download, resume_download},
    net::set_retry_attempts,
    sniffer::spawn_sniffer_server,
    state::{DownloadStatus, MediaItem, MediaType, Settings, SharedState},
};
use eframe::egui;
use serde_json::Value;
use std::{collections::HashSet, fs, path::PathBuf, sync::Arc};
use uuid::Uuid;

#[derive(Clone, Copy, PartialEq, Eq)]
enum AppView {
    Sniffing,
    Downloading,
    Completed,
}

struct SettingsDraft {
    save_dir: String,
    max_tasks: usize,
    part_threads: usize,
    min_media_size_mb: u64,
    request_retry_attempts: usize,
    part_retry_attempts: usize,
}

impl SettingsDraft {
    fn from_settings(settings: &Settings) -> Self {
        Self {
            save_dir: settings.save_dir.to_string_lossy().to_string(),
            max_tasks: settings.max_tasks,
            part_threads: settings.part_threads,
            min_media_size_mb: settings.min_media_size_mb,
            request_retry_attempts: settings.request_retry_attempts,
            part_retry_attempts: settings.part_retry_attempts,
        }
    }
}

pub struct VideoSnifferApp {
    state: SharedState,
    active_view: AppView,
    show_settings: bool,
    settings_draft: SettingsDraft,
    last_revision: u64,
    selected_task_id: Option<Uuid>,
}

impl VideoSnifferApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_chinese_font(&cc.egui_ctx);

        let state = SharedState::new();
        let (settings, port) = state.read(|app| (app.settings.clone(), app.settings.listen_port));
        set_retry_attempts(settings.request_retry_attempts);
        spawn_sniffer_server(state.clone(), port);

        Self {
            state,
            active_view: AppView::Sniffing,
            show_settings: false,
            settings_draft: SettingsDraft::from_settings(&settings),
            last_revision: 0,
            selected_task_id: None,
        }
    }

    fn clear_undownloaded_detected(&self) {
        self.state.write(|app| {
            let downloaded_or_queued = app
                .tasks
                .iter()
                .map(|task| task.media_id)
                .collect::<HashSet<_>>();
            app.detected
                .retain(|item| downloaded_or_queued.contains(&item.id));
        });
    }

    fn open_settings(&mut self) {
        self.settings_draft = self
            .state
            .read(|app| SettingsDraft::from_settings(&app.settings));
        self.show_settings = true;
    }

    fn save_settings(&mut self) {
        let save_dir = PathBuf::from(self.settings_draft.save_dir.trim());
        let request_retry_attempts = self.settings_draft.request_retry_attempts.clamp(1, 20);
        let part_retry_attempts = self.settings_draft.part_retry_attempts.clamp(1, 50);
        set_retry_attempts(request_retry_attempts);

        self.state.write(|app| {
            app.settings.save_dir = save_dir;
            app.settings.max_tasks = self.settings_draft.max_tasks.clamp(1, 16);
            app.settings.part_threads = self.settings_draft.part_threads.clamp(1, 64);
            app.settings.min_media_size_mb = self.settings_draft.min_media_size_mb.clamp(1, 1024);
            app.settings.request_retry_attempts = request_retry_attempts;
            app.settings.part_retry_attempts = part_retry_attempts;
        });
        self.show_settings = false;
    }
}

impl eframe::App for VideoSnifferApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let revision = self.state.revision();
        if revision != self.last_revision {
            self.last_revision = revision;
            ctx.request_repaint();
        }

        let previous_view = self.active_view;
        self.draw_top_bar(ctx);
        if previous_view == AppView::Sniffing && self.active_view != AppView::Sniffing {
            self.clear_undownloaded_detected();
        }
        self.draw_settings_window(ctx);

        egui::CentralPanel::default()
            .frame(egui::Frame::default().fill(egui::Color32::from_rgb(246, 248, 251)))
            .show(ctx, |ui| {
                ui.add_space(12.0);
                match self.active_view {
                    AppView::Sniffing => draw_sniffing(&self.state, ui, &mut self.active_view),
                    AppView::Downloading => {
                        draw_task_list(&self.state, ui, &mut self.selected_task_id, false)
                    }
                    AppView::Completed => {
                        draw_task_list(&self.state, ui, &mut self.selected_task_id, true)
                    }
                }
            });
    }
}

impl VideoSnifferApp {
    fn draw_top_bar(&mut self, ctx: &egui::Context) {
        let (detected, running, completed, settings) = self.state.read(|app| {
            (
                app.detected.len(),
                app.tasks
                    .iter()
                    .filter(|task| task.status != DownloadStatus::Completed)
                    .count(),
                app.tasks
                    .iter()
                    .filter(|task| task.status == DownloadStatus::Completed)
                    .count(),
                app.settings.clone(),
            )
        });

        egui::TopBottomPanel::top("top_bar")
            .exact_height(74.0)
            .frame(egui::Frame::default().fill(egui::Color32::WHITE))
            .show(ctx, |ui| {
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.add_space(12.0);
                    ui.vertical(|ui| {
                        ui.heading("VideoSniffer");
                        ui.small(format!(
                            "监听 127.0.0.1:{} · 保存到 {}",
                            settings.listen_port,
                            compact_text(&settings.save_dir.to_string_lossy(), 54)
                        ));
                    });

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("设置").clicked() {
                            self.open_settings();
                        }
                        ui.add_space(8.0);
                        nav_button(
                            ui,
                            &mut self.active_view,
                            AppView::Completed,
                            "已下载",
                            completed,
                        );
                        nav_button(
                            ui,
                            &mut self.active_view,
                            AppView::Downloading,
                            "下载中",
                            running,
                        );
                        nav_button(
                            ui,
                            &mut self.active_view,
                            AppView::Sniffing,
                            "嗅探资源",
                            detected,
                        );
                    });
                });
            });
    }

    fn draw_settings_window(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }

        let mut open = self.show_settings;
        let mut should_save = false;
        let mut should_close = false;

        egui::Window::new("设置")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(520.0)
            .show(ctx, |ui| {
                ui.label("下载保存位置");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.settings_draft.save_dir)
                            .desired_width(390.0),
                    );
                    if ui.button("选择文件夹...").clicked()
                        && let Some(folder) = rfd::FileDialog::new()
                            .set_title("选择视频保存位置")
                            .set_directory(PathBuf::from(&self.settings_draft.save_dir))
                            .pick_folder()
                    {
                        self.settings_draft.save_dir = folder.to_string_lossy().to_string();
                    }
                });

                ui.add_space(12.0);
                egui::Grid::new("settings_grid")
                    .num_columns(2)
                    .spacing([16.0, 10.0])
                    .show(ui, |ui| {
                        ui.label("同时下载任务");
                        ui.add(
                            egui::DragValue::new(&mut self.settings_draft.max_tasks).range(1..=16),
                        );
                        ui.end_row();

                        ui.label("分片线程数");
                        ui.add(
                            egui::DragValue::new(&mut self.settings_draft.part_threads)
                                .range(1..=64),
                        );
                        ui.end_row();

                        ui.label("请求重试次数");
                        ui.add(
                            egui::DragValue::new(&mut self.settings_draft.request_retry_attempts)
                                .range(1..=20),
                        );
                        ui.end_row();

                        ui.label("单分片重试次数");
                        ui.add(
                            egui::DragValue::new(&mut self.settings_draft.part_retry_attempts)
                                .range(1..=50),
                        );
                        ui.end_row();

                        ui.label("最小媒体大小 MB");
                        ui.add(
                            egui::DragValue::new(&mut self.settings_draft.min_media_size_mb)
                                .range(1..=1024),
                        );
                        ui.end_row();
                    });

                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if ui.button("保存").clicked() {
                        should_save = true;
                    }
                    if ui.button("取消").clicked() {
                        should_close = true;
                    }
                });
            });

        self.show_settings = open && !should_close;
        if should_save {
            self.save_settings();
        }
    }
}

fn nav_button(ui: &mut egui::Ui, active: &mut AppView, view: AppView, label: &str, count: usize) {
    let selected = *active == view;
    let text = format!("{label} {count}");
    if ui.selectable_label(selected, text).clicked() {
        *active = view;
    }
}

fn draw_sniffing(state: &SharedState, ui: &mut egui::Ui, active_view: &mut AppView) {
    section_header(ui, "嗅探资源", "只显示当前页面捕获到、尚未加入下载的资源");

    let items = state.read(|app| app.detected.iter().cloned().collect::<Vec<_>>());
    if items.is_empty() {
        empty_state(
            ui,
            "还没有捕获到视频资源",
            "打开浏览器播放视频后，可下载资源会出现在这里。",
        );
        return;
    }

    egui::ScrollArea::vertical().show(ui, |ui| {
        for item in items {
            resource_card(state, ui, &item, active_view);
            ui.add_space(8.0);
        }
    });
}

fn resource_card(
    state: &SharedState,
    ui: &mut egui::Ui,
    item: &MediaItem,
    active_view: &mut AppView,
) {
    egui::Frame::group(ui.style())
        .fill(egui::Color32::WHITE)
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                badge(
                    ui,
                    item.media_type.label(),
                    egui::Color32::from_rgb(37, 99, 235),
                );
                ui.label(item.detected_at.format("%H:%M:%S").to_string());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let supported = matches!(
                        item.media_type,
                        MediaType::Hls | MediaType::Mp4 | MediaType::Webm | MediaType::Unknown
                    );
                    if ui
                        .add_enabled(supported, egui::Button::new("下载"))
                        .clicked()
                    {
                        let media_id = item.id;
                        enqueue_download(state.clone(), item);
                        state.write(|app| app.detected.retain(|media| media.id != media_id));
                        *active_view = AppView::Downloading;
                    }
                    if !supported {
                        ui.label("DASH 暂未支持");
                    }
                });
            });
            ui.add_space(6.0);
            ui.strong(compact_text(&item.title, 84));
            small_url(ui, "地址", &item.url);
            if let Some(page_url) = &item.page_url {
                small_url(ui, "来源页", page_url);
            }
            if item.media_type == MediaType::Hls {
                draw_hls_quality_selector(state, ui, item);
            }
        });
}

fn draw_hls_quality_selector(state: &SharedState, ui: &mut egui::Ui, item: &MediaItem) {
    if item.hls_variants.is_empty() {
        ui.small(
            item.hls_status
                .as_deref()
                .unwrap_or("正在等待 HLS 清晰度分析"),
        );
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

fn draw_task_list(
    state: &SharedState,
    ui: &mut egui::Ui,
    selected_task_id: &mut Option<Uuid>,
    completed_only: bool,
) {
    let title = if completed_only {
        "已下载"
    } else {
        "下载中"
    };
    let subtitle = if completed_only {
        "已完成的下载记录，可按需删除记录"
    } else {
        "等待、下载、暂停和失败的任务都在这里处理"
    };
    section_header(ui, title, subtitle);

    let tasks = state.read(|app| {
        app.tasks
            .iter()
            .filter(|task| {
                if completed_only {
                    task.status == DownloadStatus::Completed
                } else {
                    task.status != DownloadStatus::Completed
                }
            })
            .cloned()
            .collect::<Vec<_>>()
    });

    if tasks.is_empty() {
        let message = if completed_only {
            "还没有已下载记录"
        } else {
            "当前没有下载任务"
        };
        empty_state(ui, message, "从嗅探资源中点击下载后会出现在这里。");
        return;
    }

    egui::ScrollArea::vertical().show(ui, |ui| {
        for task in tasks {
            task_card(state, ui, &task, selected_task_id, completed_only);
            ui.add_space(8.0);
        }
    });

    ui.separator();
    draw_task_detail(state, ui, *selected_task_id);
}

fn task_card(
    state: &SharedState,
    ui: &mut egui::Ui,
    task: &crate::state::DownloadTask,
    selected_task_id: &mut Option<Uuid>,
    completed_only: bool,
) {
    egui::Frame::group(ui.style())
        .fill(egui::Color32::WHITE)
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.strong(compact_text(&task.title, 74));
                status_label(ui, task.status);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if completed_only && ui.button("删除记录").clicked() {
                        let id = task.id;
                        state.write(|app| app.tasks.retain(|task| task.id != id));
                        if *selected_task_id == Some(id) {
                            *selected_task_id = None;
                        }
                    }
                    if !completed_only && ui.button("详情").clicked() {
                        *selected_task_id = Some(task.id);
                    }
                });
            });

            small_url(ui, "地址", &task.url);
            ui.add(egui::ProgressBar::new(task.progress).show_percentage());
            ui.label(&task.message);
            if let Some(total) = task.total_bytes {
                ui.small(format!("总大小: {}", human_bytes(total)));
            }

            if !completed_only {
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
                });
            }
        });
}

fn draw_task_detail(state: &SharedState, ui: &mut egui::Ui, selected_task_id: Option<Uuid>) {
    let Some(task_id) = selected_task_id else {
        return;
    };

    let detail = state.read(|app| {
        let task = app.tasks.iter().find(|task| task.id == task_id).cloned();
        let save_dir = app.settings.save_dir.clone();
        (task, save_dir)
    });

    let Some(task) = detail.0 else {
        return;
    };

    let manifest_path = detail
        .1
        .join(".parts")
        .join(task.id.to_string())
        .join("manifest.json");
    let manifest = read_manifest_summary(&manifest_path);

    egui::CollapsingHeader::new("分片详情")
        .default_open(true)
        .show(ui, |ui| {
            ui.label(format!("任务 ID: {}", task.id));
            ui.label(format!("媒体类型: {}", task.media_type.label()));
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
                        .max_height(140.0)
                        .show(ui, |ui| {
                            for line in summary.preview_lines {
                                ui.small(line);
                            }
                        });
                }
                None => {
                    if task.status == DownloadStatus::Completed {
                        ui.label("任务已完成，临时分片记录已清理。");
                    } else {
                        ui.label("任务开始分片下载后会生成临时记录。");
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

fn section_header(ui: &mut egui::Ui, title: &str, subtitle: &str) {
    ui.horizontal(|ui| {
        ui.add_space(12.0);
        ui.vertical(|ui| {
            ui.heading(title);
            ui.small(subtitle);
        });
    });
    ui.add_space(12.0);
}

fn empty_state(ui: &mut egui::Ui, title: &str, subtitle: &str) {
    egui::Frame::group(ui.style())
        .fill(egui::Color32::WHITE)
        .inner_margin(egui::Margin::same(16))
        .show(ui, |ui| {
            ui.strong(title);
            ui.label(subtitle);
        });
}

fn badge(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    ui.colored_label(color, text);
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
    let text = format!("{label}: {}", compact_text(url, 96));
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

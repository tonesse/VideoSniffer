use crate::{
    downloader::{enqueue_download, pause_download, resume_download},
    net::set_retry_attempts,
    sniffer::spawn_sniffer_server,
    state::{DownloadStatus, MediaCandidate, MediaItem, MediaType, Settings, SharedState},
};
use chrono::{DateTime, Local, Utc};
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

const DEFAULT_SNIFF_COLUMNS: [f32; 7] = [210.0, 70.0, 90.0, 90.0, 90.0, 300.0, 80.0];
const DEFAULT_TASK_COLUMNS: [f32; 8] = [220.0, 90.0, 80.0, 120.0, 90.0, 90.0, 150.0, 260.0];

struct SettingsDraft {
    save_dir: String,
    max_tasks: usize,
    part_threads: usize,
    min_media_size_mb: u64,
    request_retry_attempts: usize,
    part_retry_attempts: usize,
}

#[derive(Default)]
struct ManualDownloadDraft {
    url: String,
    title: String,
    error: Option<String>,
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
    show_new_task: bool,
    settings_draft: SettingsDraft,
    manual_download: ManualDownloadDraft,
    sniff_columns: [f32; 7],
    task_columns: [f32; 8],
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
            show_new_task: false,
            settings_draft: SettingsDraft::from_settings(&settings),
            manual_download: ManualDownloadDraft::default(),
            sniff_columns: DEFAULT_SNIFF_COLUMNS,
            task_columns: DEFAULT_TASK_COLUMNS,
            last_revision: 0,
            selected_task_id: None,
        }
    }

    fn clear_undownloaded_detected(&self) {
        self.state.write(|app| {
            let queued_media = app
                .tasks
                .iter()
                .map(|task| task.media_id)
                .collect::<HashSet<_>>();
            app.detected.retain(|item| queued_media.contains(&item.id));
        });
    }

    fn set_view(&mut self, next: AppView) {
        if self.active_view == AppView::Sniffing && next != AppView::Sniffing {
            self.clear_undownloaded_detected();
        }
        self.active_view = next;
        self.selected_task_id = None;
    }

    fn open_settings(&mut self) {
        self.settings_draft = self
            .state
            .read(|app| SettingsDraft::from_settings(&app.settings));
        self.show_settings = true;
    }

    fn open_new_task(&mut self) {
        self.manual_download = ManualDownloadDraft::default();
        self.show_new_task = true;
    }

    fn submit_manual_download(&mut self) {
        let url = self.manual_download.url.trim().to_string();
        if url.is_empty() {
            self.manual_download.error = Some("请输入下载地址".to_string());
            return;
        }

        let title = self
            .manual_download
            .title
            .trim()
            .to_string()
            .is_empty()
            .then_some(None)
            .unwrap_or_else(|| Some(self.manual_download.title.trim().to_string()));
        let candidate = MediaCandidate {
            url,
            page_url: None,
            title,
            mime_type: None,
            content_length: None,
            method: Some("GET".to_string()),
            request_headers: Vec::new(),
        };
        let item = MediaItem::from(candidate);
        enqueue_download(self.state.clone(), &item);

        self.show_new_task = false;
        self.set_view(AppView::Downloading);
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

    fn selected_task_status(&self) -> Option<DownloadStatus> {
        let selected = self.selected_task_id?;
        self.state.read(|app| {
            app.tasks
                .iter()
                .find(|task| task.id == selected)
                .map(|task| task.status)
        })
    }

    fn delete_selected_record(&mut self) {
        let Some(selected) = self.selected_task_id else {
            return;
        };
        self.state
            .write(|app| app.tasks.retain(|task| task.id != selected));
        self.selected_task_id = None;
    }

    fn delete_current_page_items(&mut self) {
        match self.active_view {
            AppView::Sniffing => {
                self.state.write(|app| app.detected.clear());
            }
            AppView::Downloading => {
                self.state.write(|app| {
                    app.tasks
                        .retain(|task| task.status == DownloadStatus::Completed)
                });
            }
            AppView::Completed => {
                self.state.write(|app| {
                    app.tasks
                        .retain(|task| task.status != DownloadStatus::Completed)
                });
            }
        }
        self.selected_task_id = None;
    }
}

impl eframe::App for VideoSnifferApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let revision = self.state.revision();
        if revision != self.last_revision {
            self.last_revision = revision;
            ctx.request_repaint();
        }

        self.draw_menu_and_toolbar(ctx);
        self.draw_left_categories(ctx);
        self.draw_settings_window(ctx);
        self.draw_new_task_window(ctx);

        egui::CentralPanel::default()
            .frame(egui::Frame::default().fill(egui::Color32::WHITE))
            .show(ctx, |ui| match self.active_view {
                AppView::Sniffing => draw_sniffing_table(
                    &self.state,
                    ui,
                    &mut self.active_view,
                    &mut self.sniff_columns,
                ),
                AppView::Downloading => draw_task_table(
                    &self.state,
                    ui,
                    &mut self.selected_task_id,
                    false,
                    &mut self.task_columns,
                ),
                AppView::Completed => draw_task_table(
                    &self.state,
                    ui,
                    &mut self.selected_task_id,
                    true,
                    &mut self.task_columns,
                ),
            });
    }
}

impl VideoSnifferApp {
    fn draw_menu_and_toolbar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("menu_toolbar")
            .exact_height(58.0)
            .frame(egui::Frame::default().fill(egui::Color32::from_rgb(238, 238, 238)))
            .show(ctx, |ui| {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    toolbar_button(ui, "新建任务", true, || self.open_new_task());

                    let selected_status = self.selected_task_status();
                    toolbar_button(
                        ui,
                        "继续",
                        matches!(
                            selected_status,
                            Some(
                                DownloadStatus::Paused
                                    | DownloadStatus::Failed
                                    | DownloadStatus::Queued
                            )
                        ),
                        || {
                            if let Some(id) = self.selected_task_id {
                                resume_download(self.state.clone(), id);
                            }
                        },
                    );
                    toolbar_button(
                        ui,
                        "暂停",
                        matches!(
                            selected_status,
                            Some(DownloadStatus::Queued | DownloadStatus::Downloading)
                        ),
                        || {
                            if let Some(id) = self.selected_task_id {
                                pause_download(self.state.clone(), id);
                            }
                        },
                    );
                    toolbar_button(ui, "删除任务", self.selected_task_id.is_some(), || {
                        self.delete_selected_record();
                    });
                    toolbar_button(ui, "删除全部", true, || {
                        self.delete_current_page_items()
                    });

                    ui.separator();
                    toolbar_button(ui, "选项", true, || self.open_settings());
                    toolbar_button(ui, "嗅探资源", true, || {
                        self.set_view(AppView::Sniffing)
                    });
                    toolbar_button(ui, "下载中", true, || {
                        self.set_view(AppView::Downloading)
                    });
                    toolbar_button(ui, "已下载", true, || self.set_view(AppView::Completed));
                });
            });
    }

    fn draw_left_categories(&mut self, ctx: &egui::Context) {
        let (detected, running, completed) = self.state.read(|app| {
            let running = app
                .tasks
                .iter()
                .filter(|task| task.status != DownloadStatus::Completed)
                .count();
            let completed = app
                .tasks
                .iter()
                .filter(|task| task.status == DownloadStatus::Completed)
                .count();
            (app.detected.len(), running, completed)
        });

        egui::SidePanel::left("categories")
            .exact_width(178.0)
            .frame(
                egui::Frame::default()
                    .fill(egui::Color32::from_rgb(247, 247, 247))
                    .stroke(egui::Stroke::new(
                        1.0,
                        egui::Color32::from_rgb(210, 210, 210),
                    )),
            )
            .show(ctx, |ui| {
                ui.add_space(6.0);
                ui.label("分类");
                ui.separator();
                category_row(
                    ui,
                    "嗅探资源",
                    detected,
                    self.active_view == AppView::Sniffing,
                    || {
                        self.set_view(AppView::Sniffing);
                    },
                );
                category_row(
                    ui,
                    "正在下载",
                    running,
                    self.active_view == AppView::Downloading,
                    || {
                        self.set_view(AppView::Downloading);
                    },
                );
                category_row(
                    ui,
                    "已下载",
                    completed,
                    self.active_view == AppView::Completed,
                    || {
                        self.set_view(AppView::Completed);
                    },
                );
            });
    }

    fn draw_settings_window(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }

        let mut open = self.show_settings;
        let mut should_save = false;
        let mut should_close = false;

        egui::Window::new("选项")
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

    fn draw_new_task_window(&mut self, ctx: &egui::Context) {
        if !self.show_new_task {
            return;
        }

        let mut open = self.show_new_task;
        let mut should_submit = false;
        let mut should_close = false;

        egui::Window::new("新建任务")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(560.0)
            .show(ctx, |ui| {
                ui.label("下载地址");
                ui.add(
                    egui::TextEdit::singleline(&mut self.manual_download.url)
                        .desired_width(f32::INFINITY)
                        .hint_text("https://example.com/video.mp4 或 .m3u8"),
                );

                ui.add_space(8.0);
                ui.label("文件名（可选）");
                ui.add(
                    egui::TextEdit::singleline(&mut self.manual_download.title)
                        .desired_width(f32::INFINITY),
                );

                if let Some(error) = &self.manual_download.error {
                    ui.add_space(8.0);
                    ui.colored_label(egui::Color32::from_rgb(220, 38, 38), error);
                }

                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if ui.button("开始下载").clicked() {
                        should_submit = true;
                    }
                    if ui.button("取消").clicked() {
                        should_close = true;
                    }
                });
            });

        self.show_new_task = open && !should_close;
        if should_submit {
            self.submit_manual_download();
        }
    }
}

fn toolbar_button(ui: &mut egui::Ui, text: &str, enabled: bool, action: impl FnOnce()) {
    let button = egui::Button::new(text).min_size(egui::vec2(68.0, 42.0));
    if ui.add_enabled(enabled, button).clicked() {
        action();
    }
}

fn category_row(
    ui: &mut egui::Ui,
    label: &str,
    count: usize,
    selected: bool,
    action: impl FnOnce(),
) {
    let text = format!("  {label} ({count})");
    if ui.selectable_label(selected, text).clicked() {
        action();
    }
}

fn draw_sniffing_table(
    state: &SharedState,
    ui: &mut egui::Ui,
    active_view: &mut AppView,
    columns: &mut [f32; 7],
) {
    table_header(
        ui,
        &["文件名", "类型", "大小", "状态", "时间", "地址", "操作"],
        columns,
    );

    let items = state.read(|app| app.detected.iter().cloned().collect::<Vec<_>>());
    if items.is_empty() {
        empty_row(
            ui,
            "还没有嗅探到视频资源。打开浏览器播放视频后，可下载资源会出现在这里。",
        );
        return;
    }

    egui::ScrollArea::vertical().show(ui, |ui| {
        for item in items {
            table_row(ui, columns, |ui| {
                table_cell(ui, columns[0], &item.title);
                table_cell(ui, columns[1], item.media_type.label());
                table_cell(
                    ui,
                    columns[2],
                    item.content_length
                        .map(human_bytes)
                        .unwrap_or_else(|| "未知".to_string()),
                );
                table_cell(ui, columns[3], "已嗅探");
                table_cell(ui, columns[4], local_time(item.detected_at));
                table_cell(ui, columns[5], &item.url);
                cell_ui(ui, columns[6], |ui| {
                    let supported = matches!(
                        item.media_type,
                        MediaType::Hls | MediaType::Mp4 | MediaType::Webm | MediaType::Unknown
                    );
                    if ui
                        .add_enabled(supported, egui::Button::new("下载"))
                        .clicked()
                    {
                        let media_id = item.id;
                        enqueue_download(state.clone(), &item);
                        state.write(|app| app.detected.retain(|media| media.id != media_id));
                        *active_view = AppView::Downloading;
                    }
                });
            });

            if item.media_type == MediaType::Hls {
                table_row(ui, columns, |ui| {
                    table_cell(ui, columns[0], "");
                    table_cell(ui, columns[1], "");
                    table_cell(ui, columns[2], "");
                    table_cell(ui, columns[3], "");
                    table_cell(ui, columns[4], "");
                    cell_ui(ui, columns[5], |ui| {
                        draw_hls_quality_selector(state, ui, &item);
                    });
                    table_cell(ui, columns[6], "");
                });
            }
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
}

fn draw_task_table(
    state: &SharedState,
    ui: &mut egui::Ui,
    selected_task_id: &mut Option<Uuid>,
    completed_only: bool,
    columns: &mut [f32; 8],
) {
    table_header(
        ui,
        &[
            "文件名",
            "大小",
            "状态",
            "进度",
            "剩余时间",
            "下载速度",
            "最后连接时间",
            "描述",
        ],
        columns,
    );

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
            "还没有已下载记录。"
        } else {
            "当前没有下载任务。"
        };
        empty_row(ui, message);
        return;
    }

    egui::ScrollArea::vertical().show(ui, |ui| {
        for task in tasks {
            table_row(ui, columns, |ui| {
                let selected = *selected_task_id == Some(task.id);
                if selectable_table_cell(ui, columns[0], &task.title, selected).clicked() {
                    *selected_task_id = Some(task.id);
                }
                table_cell(
                    ui,
                    columns[1],
                    task.total_bytes
                        .map(human_bytes)
                        .unwrap_or_else(|| "-".to_string()),
                );
                cell_ui(ui, columns[2], |ui| {
                    status_label(ui, task.status);
                });
                cell_ui(ui, columns[3], |ui| {
                    ui.add(
                        egui::ProgressBar::new(task.progress)
                            .show_percentage()
                            .desired_width(columns[3] - 8.0),
                    );
                });
                table_cell(
                    ui,
                    columns[4],
                    if task.status == DownloadStatus::Completed {
                        "0 秒"
                    } else {
                        "-"
                    },
                );
                table_cell(ui, columns[5], "-");
                table_cell(ui, columns[6], "-");
                cell_ui(ui, columns[7], |ui| {
                    ui.horizontal(|ui| {
                        let action_width = if completed_only { 54.0 } else { 104.0 };
                        table_cell(ui, (columns[7] - action_width).max(48.0), &task.message);
                        if completed_only && ui.button("删除").clicked() {
                            let id = task.id;
                            state.write(|app| app.tasks.retain(|task| task.id != id));
                            if *selected_task_id == Some(id) {
                                *selected_task_id = None;
                            }
                        } else if !completed_only {
                            if matches!(
                                task.status,
                                DownloadStatus::Queued | DownloadStatus::Downloading
                            ) && ui.button("暂停").clicked()
                            {
                                pause_download(state.clone(), task.id);
                            }
                            if matches!(
                                task.status,
                                DownloadStatus::Paused
                                    | DownloadStatus::Failed
                                    | DownloadStatus::Queued
                            ) && ui.button("继续").clicked()
                            {
                                resume_download(state.clone(), task.id);
                            }
                        }
                    });
                });
            });
        }
    });

    if !completed_only {
        draw_task_detail(state, ui, *selected_task_id);
    }
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

    ui.separator();
    egui::CollapsingHeader::new("分片详情")
        .default_open(false)
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
                        .max_height(120.0)
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

fn table_header(ui: &mut egui::Ui, columns: &[&str], widths: &mut [f32]) {
    egui::Frame::default()
        .fill(egui::Color32::from_rgb(245, 245, 245))
        .stroke(egui::Stroke::new(
            1.0,
            egui::Color32::from_rgb(214, 214, 214),
        ))
        .inner_margin(egui::Margin::symmetric(6, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                for (index, column) in columns.iter().enumerate() {
                    let response =
                        ui.add_sized([widths[index], 18.0], egui::Label::new(*column).truncate());
                    if index + 1 < columns.len() {
                        let rect = egui::Rect::from_min_max(
                            egui::pos2(response.rect.right() - 3.0, response.rect.top()),
                            egui::pos2(response.rect.right() + 4.0, response.rect.bottom()),
                        );
                        let drag = ui.interact(
                            rect,
                            ui.id().with(("column_resize", index)),
                            egui::Sense::click_and_drag(),
                        );
                        ui.painter().line_segment(
                            [rect.center_top(), rect.center_bottom()],
                            egui::Stroke::new(1.0, egui::Color32::from_rgb(190, 190, 190)),
                        );
                        if drag.hovered() || drag.dragged() {
                            ui.output_mut(|output| {
                                output.cursor_icon = egui::CursorIcon::ResizeHorizontal;
                            });
                        }
                        if drag.dragged() {
                            let delta = ui.input(|input| input.pointer.delta().x);
                            widths[index] = (widths[index] + delta).clamp(48.0, 620.0);
                        }
                    }
                }
            });
        });
}

fn table_row(ui: &mut egui::Ui, widths: &[f32], add_contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::default()
        .fill(egui::Color32::WHITE)
        .inner_margin(egui::Margin::symmetric(6, 2))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                add_contents(ui);
                let used = widths.iter().sum::<f32>();
                let remaining = (ui.available_width() - used).max(0.0);
                if remaining > 0.0 {
                    ui.add_space(remaining);
                }
            });
        });
}

fn table_cell(ui: &mut egui::Ui, width: f32, text: impl AsRef<str>) -> egui::Response {
    let text = text.as_ref();
    let (display, truncated) = elide_to_width(ui, text, width - 8.0);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 22.0), egui::Sense::hover());
    let painter = ui.painter().with_clip_rect(rect);
    painter.text(
        egui::pos2(rect.left() + 4.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        display,
        egui::TextStyle::Body.resolve(ui.style()),
        ui.visuals().text_color(),
    );

    if truncated {
        response.on_hover_text(text)
    } else {
        response
    }
}

fn selectable_table_cell(
    ui: &mut egui::Ui,
    width: f32,
    text: &str,
    selected: bool,
) -> egui::Response {
    let (display, truncated) = elide_to_width(ui, text, width - 8.0);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 22.0), egui::Sense::click());
    let fill = if selected {
        egui::Color32::from_rgb(205, 232, 255)
    } else if response.hovered() {
        egui::Color32::from_rgb(238, 246, 252)
    } else {
        egui::Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 0.0, fill);
    let painter = ui.painter().with_clip_rect(rect);
    painter.text(
        egui::pos2(rect.left() + 4.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        display,
        egui::TextStyle::Body.resolve(ui.style()),
        ui.visuals().text_color(),
    );

    if truncated {
        response.on_hover_text(text)
    } else {
        response
    }
}

fn cell_ui(ui: &mut egui::Ui, width: f32, add_contents: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(
        egui::vec2(width, 22.0),
        egui::Layout::left_to_right(egui::Align::Center),
        add_contents,
    );
}

fn elide_to_width(ui: &egui::Ui, text: &str, width: f32) -> (String, bool) {
    if text.is_empty() {
        return (String::new(), false);
    }

    let available = width.max(0.0);
    if text_width(ui, text) <= available {
        return (text.to_string(), false);
    }

    let ellipsis = "...";
    let ellipsis_width = text_width(ui, ellipsis);
    if ellipsis_width >= available {
        return (ellipsis.to_string(), true);
    }

    let chars = text.chars().collect::<Vec<_>>();
    let mut low = 0;
    let mut high = chars.len();
    while low < high {
        let mid = (low + high).div_ceil(2);
        let candidate = chars[..mid].iter().collect::<String>() + ellipsis;
        if text_width(ui, &candidate) <= available {
            low = mid;
        } else {
            high = mid - 1;
        }
    }

    let display = chars[..low].iter().collect::<String>() + ellipsis;
    (display, true)
}

fn text_width(ui: &egui::Ui, text: &str) -> f32 {
    let font_id = egui::TextStyle::Body.resolve(ui.style());
    let color = ui.visuals().text_color();
    ui.painter()
        .layout_no_wrap(text.to_string(), font_id, color)
        .size()
        .x
}

fn empty_row(ui: &mut egui::Ui, text: &str) {
    ui.add_space(12.0);
    ui.label(text);
}

fn local_time(time: DateTime<Utc>) -> String {
    time.with_timezone(&Local).format("%H:%M:%S").to_string()
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

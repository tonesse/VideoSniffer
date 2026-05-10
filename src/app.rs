use crate::{
    downloader::enqueue_download,
    sniffer::spawn_sniffer_server,
    state::{DownloadStatus, MediaType, SharedState},
};
use eframe::egui;

pub struct VideoSnifferApp {
    state: SharedState,
    save_dir_input: String,
    last_revision: u64,
}

impl VideoSnifferApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
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
                let response = ui.text_edit_singleline(&mut self.save_dir_input);
                if response.lost_focus() {
                    let value = self.save_dir_input.clone();
                    self.state.write(|app| app.settings.save_dir = value.into());
                }

                ui.add_space(8.0);
                ui.label("浏览器扩展把媒体请求 POST 到本地接口后，会自动出现在右侧列表。");
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.columns(2, |columns| {
                draw_detected(&self.state, &mut columns[0]);
                draw_tasks(&self.state, &mut columns[1]);
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
                ui.small(&item.url);
                if let Some(page_url) = &item.page_url {
                    ui.small(format!("来源页: {page_url}"));
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

fn draw_tasks(state: &SharedState, ui: &mut egui::Ui) {
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
                ui.small(&task.url);
                ui.add(egui::ProgressBar::new(task.progress).show_percentage());
                ui.label(&task.message);
                if let Some(total) = task.total_bytes {
                    ui.small(format!("总大小: {}", human_bytes(total)));
                }
                ui.small(format!("媒体 ID: {}", task.media_id));
            });
            ui.add_space(8.0);
        }
    });
}

fn status_label(ui: &mut egui::Ui, status: DownloadStatus) {
    let color = match status {
        DownloadStatus::Queued => egui::Color32::GRAY,
        DownloadStatus::Downloading => egui::Color32::from_rgb(37, 99, 235),
        DownloadStatus::Completed => egui::Color32::from_rgb(22, 163, 74),
        DownloadStatus::Failed => egui::Color32::from_rgb(220, 38, 38),
        DownloadStatus::Unsupported => egui::Color32::from_rgb(217, 119, 6),
    };
    ui.colored_label(color, status.label());
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

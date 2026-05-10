mod app;
mod downloader;
mod hls;
mod net;
mod sniffer;
mod state;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([980.0, 620.0])
            .with_title("VideoSniffer"),
        ..Default::default()
    };

    eframe::run_native(
        "VideoSniffer",
        options,
        Box::new(|cc| Ok(Box::new(app::VideoSnifferApp::new(cc)))),
    )
}

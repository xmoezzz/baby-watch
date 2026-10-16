#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod app;
mod browser;
mod fonts;
mod image_cache;
mod model;
mod monitor;
mod release;
mod shopify;
mod storage;

use crossbeam_channel::unbounded;

fn main() -> eframe::Result<()> {
    let (command_tx, command_rx) = unbounded();
    let (event_tx, event_rx) = unbounded();
    let _monitor = monitor::spawn(command_rx, event_tx);

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1240.0, 800.0])
            .with_min_inner_size([920.0, 620.0])
            .with_title("BABY Watch"),
        ..Default::default()
    };

    eframe::run_native(
        "BABY Watch",
        native_options,
        Box::new(move |cc| Ok(Box::new(app::BabyWatchApp::new(cc, command_tx, event_rx)))),
    )
}

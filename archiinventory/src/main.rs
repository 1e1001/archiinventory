//#![windows_subsystem = "windows"]
#![feature(const_path_separators)]

use std::fmt;

use eframe::egui;
use multiline_logger::log;

use crate::ui::App;

const APP_ID: &str = "archiinventory";

mod conn;
mod data;
mod ui;

fn main() -> eframe::Result {
	multiline_logger::Settings {
		title: APP_ID,
		filters: &[
			//("archipelago_rs", log::LevelFilter::Trace),
			("archiinventory", log::LevelFilter::Trace),
			("", log::LevelFilter::Info),
		],
		file_out: None,
		console_out: true,
		panic_hook: Some(|panic| {
			rfd::MessageDialog::new()
				.set_title(format!("{} - {} Panic", panic.title, panic.thread))
				.set_description(format!(
					"{}\n→ {}",
					panic.message.unwrap_or("[non-string message]"),
					panic
						.location
						.as_ref()
						.map_or(&"[citation needed]" as &dyn fmt::Display, |v| v)
				))
				.set_buttons(rfd::MessageButtons::Ok)
				.set_level(rfd::MessageLevel::Error)
				.show();
		}),
	}
	.init();
	let options = eframe::NativeOptions {
		// TODO: add my own icon
		viewport: egui::ViewportBuilder::default()
			.with_icon(egui::IconData::default())
			.with_app_id(APP_ID),
		..Default::default()
	};
	eframe::run_native(APP_ID, options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}

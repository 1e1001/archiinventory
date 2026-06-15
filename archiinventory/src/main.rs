use std::fmt;

use eframe::egui;
use multiline_logger::log;

use crate::ui::App;

mod conn;
mod data;
mod istr;
mod ui;

const APP_ID: &str = "archiinventory";

fn main() -> anyhow::Result<()> {
	let client = conn::ClientMain::from_args();
	multiline_logger::Settings {
		title: if client.is_some() { "archiinventory (client)" } else { APP_ID },
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
	if let Some(client) = client {
		client.run();
	}
	let options = eframe::NativeOptions {
		// TODO: add my own icon
		viewport: egui::ViewportBuilder::default()
			.with_icon(egui::IconData::default())
			.with_app_id(APP_ID),
		..Default::default()
	};
	eframe::run_native(APP_ID, options, Box::new(|cc| Ok(Box::new(App::new(cc)))))?;
	Ok(())
}

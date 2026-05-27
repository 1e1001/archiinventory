use std::ffi::OsString;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use eframe::{Storage, egui};
use multiline_logger::log;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::APP_ID;
use crate::conn::Connection;
use crate::world::{World, WorldSlot};

fn try_load_cbor<T: DeserializeOwned>(path: impl AsRef<Path>) -> anyhow::Result<T> {
	Ok(ciborium::from_reader(File::open(path)?)?)
}
fn try_save_cbor<T: Serialize>(path: impl AsRef<Path>, value: &T) -> anyhow::Result<()> {
	Ok(ciborium::into_writer(value, File::create(path)?)?)
}

const OSSTR_VERSION: &str = include_str!(concat!(env!("OUT_DIR"), "/osstr.txt"));

fn encode_path(path: &Path) -> String {
	let mut res = z85::encode(path.as_os_str().as_encoded_bytes());
	res.insert_str(0, OSSTR_VERSION);
	res
}
fn decode_path(path: &str) -> anyhow::Result<PathBuf> {
	let Some((version, text)) = path.split_once(',') else {
		anyhow::bail!("Invalid path {path}")
	};
	let bytes = z85::decode(text.as_bytes())?;
	match String::from_utf8(bytes) {
		// if the string is valid utf-8, there's no need to do compatibility checks
		Ok(text) => Ok(text.into()),
		Err(err) => {
			let bytes = err.into_bytes();
			let expected = &OSSTR_VERSION[..OSSTR_VERSION.len() - 1];
			if version != expected {
				anyhow::bail!("Incompatible path format\nExpected {expected}\nGot {version}");
			}
			Ok(unsafe { OsString::from_encoded_bytes_unchecked(bytes) }.into())
		}
	}
}

fn dialog_overwrite_unsaved() -> bool {
	rfd::MessageDialog::new()
		.set_title("Unsaved World - archiinventory")
		.set_level(rfd::MessageLevel::Warning)
		.set_description("Current world is unsaved, and data will be lost.")
		.set_buttons(rfd::MessageButtons::OkCancel)
		.show()
		== rfd::MessageDialogResult::Ok
}
fn dialog_error(title: &str, err: anyhow::Error) {
	rfd::MessageDialog::new()
		.set_title(format!("{title} - archiinventory"))
		.set_level(rfd::MessageLevel::Error)
		.set_description(err.to_string())
		.set_buttons(rfd::MessageButtons::Ok)
		.show();
}

pub struct App {
	egui_ctx: egui::Context,
	storage_dir: PathBuf,
	world_path: Option<PathBuf>,
	world: World,
	world_dirty: bool,
	world_focus: Option<usize>,
	// TODO: inventory search metadata goes here (so it stays between slots)
	connection: Option<Connection>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SlotAction {
	None,
	Focus(usize),
	Swap(usize, usize),
	Delete(usize),
}
impl App {
	pub fn new(cc: &eframe::CreationContext) -> Self {
		let storage_dir = eframe::storage_dir(APP_ID).expect("Failed to find system path");
		let (world, world_path) = cc
			.storage
			.and_then(|storage| storage.get_string("world_path"))
			.and_then(|path| match decode_path(&path) {
				Ok(path) => match try_load_cbor::<World>(&path) {
					Ok(world) => {
						log::info!("Using previously-loaded world");
						Some((world, path))
					}
					Err(err) => {
						log::warn!("Failed to load previous world\n{err}");
						None
					}
				},
				Err(err) => {
					log::warn!("Failed to decode world path\n{err}");
					None
				}
			})
			.unzip();
		let world = world.unwrap_or_default();
		cc.egui_ctx
			.send_viewport_cmd(egui::ViewportCommand::Title(format!(
				"{} - archiinventory",
				world.name
			)));
		Self {
			egui_ctx: cc.egui_ctx.clone(),
			storage_dir,
			world,
			world_path,
			world_dirty: false,
			world_focus: None,
			connection: None,
		}
	}
	fn save_world(&mut self) {
		log::info!("Saving world");
		if let Err(err) = try_save_cbor(
			self.world_path
				.as_ref()
				.expect("save_world called without path set"),
			&self.world,
		) {
			dialog_error("Failed to save world", err);
		} else {
			self.world_dirty = false;
		}
	}
	fn dialog_filepicker_base(&self) -> rfd::FileDialog {
		rfd::FileDialog::new()
			.add_filter("archiinventory world data", &["cbor"])
			.set_directory(
				self.world_path
					.as_deref()
					.and_then(Path::parent)
					.unwrap_or(&self.storage_dir),
			)
	}
	fn modified_world(&mut self) { self.world_dirty = true; }
	// TODO: bring this out to its own struct and add:
	// - up/down buttons should focus the previous/next button (so you can press enter)
	// - good icons (does svg work?)
	fn slot_name_ui(
		ui: &mut egui::Ui,
		action: &mut SlotAction,
		modified: &mut bool,
		world_focus: &mut Option<usize>,
		last: usize,
		i: usize,
		slot: &mut WorldSlot,
	) {
		ui.horizontal(|ui| {
			if ui
				.button("X")
				.on_hover_ui(|ui| {
					ui.label("Delete this slot");
				})
				.clicked()
			{
				// TODO: this might want some confirmation if the slot has items?
				*action = SlotAction::Delete(i);
				if let Some(slot) = world_focus
					&& *slot > i
				{
					*slot -= 1;
				}
			}
			if ui
				.add_enabled(i > 0, egui::Button::new("U"))
				.on_hover_ui(|ui| {
					ui.label("Move slot up");
				})
				.clicked()
			{
				*action = SlotAction::Swap(i - 1, i);
				if let Some(slot) = world_focus {
					if *slot == i {
						*slot -= 1
					} else if *slot == i - 1 {
						*slot += 1;
					}
				}
			}
			if ui
				.add_enabled(i < last, egui::Button::new("D"))
				.on_hover_ui(|ui| {
					ui.label("Move slot down");
				})
				.clicked()
			{
				*action = SlotAction::Swap(i, i + 1);
				if let Some(slot) = world_focus {
					if *slot == i {
						*slot += 1
					} else if *slot == i + 1 {
						*slot -= 1;
					}
				}
			}
			// TODO: should focus instead be by last selected textbox?
			if ui
				.add_enabled(*world_focus != Some(i), egui::Button::new("V"))
				.on_hover_ui(|ui| {
					ui.label("View slot inventory");
				})
				.clicked()
			{
				*world_focus = Some(i);
			}
		});
		let response = ui.add(
			egui::TextEdit::singleline(&mut slot.name)
				.hint_text("Slot name")
				.id_source(i),
		);
		if response.changed() {
			*modified = true;
		}
		if *action == SlotAction::Focus(i) {
			response.request_focus();
		}
	}
	fn ui(&mut self, ui: &mut egui::Ui) {
		// handle close button via modal
		if ui.ctx().input(|input| input.viewport().close_requested()) && self.world_dirty {
			match rfd::MessageDialog::new()
				.set_title("archiinventory - Unsaved World")
				.set_level(rfd::MessageLevel::Warning)
				.set_description("Save world before exiting?")
				.set_buttons(rfd::MessageButtons::YesNoCancel)
				.show()
			{
				rfd::MessageDialogResult::Yes => self.save_world(),
				rfd::MessageDialogResult::No => {}
				_ => ui
					.ctx()
					.send_viewport_cmd(egui::ViewportCommand::CancelClose),
			}
		}
		egui::Panel::top("t").show_inside(ui, |ui| {
			// world file management
			ui.horizontal(|ui| {
				if ui.button("New").clicked() {
					if !self.world_dirty || dialog_overwrite_unsaved() {
						log::info!("Resetting world");
						self.world_path = None;
						self.world_dirty = false;
						self.world_focus = None;
						self.world = World::default();
					}
				}
				if ui.button("Open").clicked() {
					if let Some(path) = self
						.dialog_filepicker_base()
						.set_title("Open world file")
						.pick_file()
					{
						match try_load_cbor(&path) {
							Ok(new_world) => {
								if !self.world_dirty || dialog_overwrite_unsaved() {
									log::info!("Loading world");
									self.world_path = Some(path);
									self.world_dirty = false;
									self.world_focus = None;
									self.world = new_world;
								}
							}
							Err(err) => {
								dialog_error("Failed to open world", err);
							}
						}
					}
				}
				if ui
					.add_enabled(
						self.world_path.is_some() && self.world_dirty,
						egui::Button::new("Save"),
					)
					.on_hover_ui(|ui| {
						ui.horizontal(|ui| {
							ui.spacing_mut().item_spacing.x = 0.0;
							ui.label("to ");
							ui.horizontal_wrapped(|ui| {
								ui.spacing_mut().item_spacing.x = 0.0;
								for segment in self
									.world_path
									.as_ref()
									.unwrap_or(&PathBuf::new())
									.to_string_lossy()
									.split_inclusive(std::path::SEPARATORS)
								{
									ui.label(segment);
								}
							});
						});
					})
					.clicked()
				{
					self.save_world();
				}
				if ui.button("Save As").clicked() {
					if let Some(path) = self
						.dialog_filepicker_base()
						.set_title("Save world file")
						.save_file()
					{
						self.world_path = Some(path);
						self.save_world();
					}
				}
				ui.add_space(8.0);
				if ui.button("Help").clicked() {
					rfd::MessageDialog::new()
        .set_title("Help - archiinventory")
		.set_description("good luck :)")
		.set_level(rfd::MessageLevel::Info)
		.set_buttons(rfd::MessageButtons::YesNo)
		.show();
				}
			});
		});
		egui::Panel::left("world")
			.resizable(true)
			.frame(egui::Frame::central_panel(&ui.style()).inner_margin(egui::Margin::ZERO))
			.show_inside(ui, |ui| {
				egui::Panel::bottom("connection").show_inside(ui, |ui| {
					ui.horizontal(|ui| {
						if ui.button("Connect").clicked() {
							// todo!
						}
						ui.add(egui::ProgressBar::new(0.5).show_percentage());
					});
				});
				let style_item_spacing_y = ui.style().spacing.item_spacing.y;

				let screen_size = ui.available_size();

				// world setup (connection info & slot names)
				egui::ScrollArea::vertical()
					.content_margin(egui::Margin::same(8))
					.show(ui, |ui| {
						let mut modified = false;
						//ui.take_available_width();
						egui::Grid::new("world")
							.num_columns(2)
							//.with_row_color(slots_striped)
							.show(ui, |ui| {
								//ui.take_available_width();
								ui.label("Name");
								let name_modified = ui
									.add(
										egui::TextEdit::singleline(&mut self.world.name)
											.id_source("world.name"),
									)
									.changed();
								if name_modified {
									modified = true;
									ui.send_viewport_cmd(egui::ViewportCommand::Title(format!(
										"{} - archiinventory",
										self.world.name
									)));
								}
								ui.end_row();
								ui.label("Address");
								modified |= ui
									.add(
										egui::TextEdit::singleline(&mut self.world.address)
											.id_source("world.address"),
									)
									.changed();
								ui.end_row();
								ui.label("Password");
								modified |= ui
									.add(
										egui::TextEdit::singleline(&mut self.world.password)
											.id_source("world.password")
											.password(true),
									)
									.changed();
								ui.end_row();
								let mut action = SlotAction::None;
								ui.label("Slots");
								ui.horizontal(|ui| {
									if ui
										.button("S")
										.on_hover_ui(|ui| {
											ui.label("Sort slots by name");
										})
										.clicked()
									{
										let focused_slot_id = self
											.world_focus
											.and_then(|slot| self.world.slots.get(slot))
											.map(|slot| slot.id);
										self.world.slots.sort_by(|l, r| l.name.cmp(&r.name));
										self.world_focus = focused_slot_id
											.and_then(|id| {
												self.world
													.slots
													.iter()
													.enumerate()
													.find(|(_, slot)| slot.id == id)
											})
											.map(|(i, _)| i);
										modified = true;
									}
									if ui
										.button("A")
										.on_hover_ui(|ui| {
											ui.label("Add a new slot");
										})
										.clicked()
									{
										action = SlotAction::Focus(self.world.slots.len());
										self.world.slots.push(Default::default());
										modified = true;
									};
									ui.label(format!("{}", self.world.slots.len()));
								});
								ui.end_row();
								let last = self.world.slots.len().saturating_sub(1);
								for (i, slot) in self.world.slots.iter_mut().enumerate() {
									let pre_cursor = ui.cursor();
									Self::slot_name_ui(
										ui,
										&mut action,
										&mut modified,
										&mut self.world_focus,
										last,
										i,
										slot,
									);
									ui.end_row();
									if i % 2 == 0 && i != last {
										// paint full-width table stripe myself
										let post_cursor = ui.cursor();
										// all slots have the same height
										let row_top =
											post_cursor.min.y - style_item_spacing_y / 2.0;
										let row_height = post_cursor.min.y - pre_cursor.min.y;
										let stripe_rect = egui::Rect::from_min_size(
											egui::pos2(0.0, row_top),
											egui::vec2(screen_size.x, row_height),
										);
										ui.painter().rect(
											stripe_rect,
											0,
											ui.style().visuals.faint_bg_color,
											egui::Stroke::NONE,
											egui::StrokeKind::Inside,
										);
									}
								}
								if self.world.slots.is_empty() {
									// invisible slot to ensure grid spacing
									ui.set_invisible();
									Self::slot_name_ui(
										ui,
										&mut action,
										&mut modified,
										&mut self.world_focus,
										0,
										0,
										&mut Default::default(),
									);
								}
								match action {
									SlotAction::None | SlotAction::Focus(_) => {}
									SlotAction::Delete(n) => {
										self.world.slots.remove(n);
										modified = true;
									}
									SlotAction::Swap(a, b) => {
										self.world.slots.swap(a, b);
										modified = true;
									}
								}
								// do not add any more elements here, they will be invisible
							});
						if modified {
							self.modified_world();
						}
						// some overscroll for convenience
						ui.allocate_space(egui::vec2(0.0, screen_size.y / 2.0));
					});
			});
		egui::CentralPanel::default().show_inside(ui, |ui| {
			if let Some(slot) = self
				.world_focus
				.and_then(|slot| self.world.slots.get_mut(slot))
			{
				ui.heading(&slot.name);
				ui.label("nothing here yet...")
			} else {
				ui.heading("No slot selected")
			}
		});
	}
}
impl eframe::App for App {
	fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) { self.ui(ui); }
	fn save(&mut self, storage: &mut dyn Storage) {
		log::debug!("Persistance save");
		if let Some(path) = &self.world_path {
			storage.set_string("world_path", encode_path(path));
		}
	}
}

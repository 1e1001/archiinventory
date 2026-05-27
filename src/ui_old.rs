use std::ffi::OsString;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use eframe::{Storage, egui};
use multiline_logger::log;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::APP_ID;
use crate::world::{World, WorldSlot};
use crate::conn::Connection;

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
		anyhow::bail!("Invalid path {path}");
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

#[derive(Default)]
pub struct SlotUi {
	open: bool,
	viewport: Option<SlotViewport>,
}

impl SlotUi {
	fn ui(slot: &mut WorldSlot, ui: &mut egui::Ui) {
		ui.heading(&slot.name);
		ui.label("nothing here yet...");

		// close window?
		if let Some(viewport) = ui.ctx().input(|input| {
			input
				.viewport()
				.close_requested()
				.then(|| SlotViewport::new(input.viewport()))
		}) {
			slot.ui.open = false;
			slot.ui.viewport = Some(viewport);
		}
	}
}

#[derive(Default, Debug, Serialize, Deserialize, Clone, Copy)]
pub struct SlotViewport {
	outer_position: Option<egui::Pos2>,
	inner_size: Option<egui::Vec2>,
	fullscreen: Option<bool>,
	maximized: Option<bool>,
}

impl SlotViewport {
	fn new(vp: &egui::ViewportInfo) -> Self {
		Self {
			outer_position: vp.outer_rect.map(|rect| rect.min),
			inner_size: vp.inner_rect.map(|rect| rect.size()),
			fullscreen: vp.fullscreen,
			maximized: vp.maximized,
		}
	}
	fn restore(self, vb: &mut egui::ViewportBuilder) {
		log::debug!("Restoring viewport {self:#?}");
		vb.position = self.outer_position;
		vb.inner_size = self.inner_size;
		vb.fullscreen = self.fullscreen;
		vb.maximized = self.maximized;
	}
}

pub struct App {
	egui_ctx: egui::Context,
	storage_dir: PathBuf,
	world_path: Option<PathBuf>,
	world: World,
	world_dirty: bool,
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
		let world = cc
			.storage
			.and_then(|storage| storage.get_string("world_path"))
			.and_then(|path| match decode_path(&path) {
				Ok(path) => match try_load_cbor(&path) {
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
			});
		let mut app = Self {
			egui_ctx: cc.egui_ctx.clone(),
			storage_dir,
			world: Default::default(),
			world_path: None,
			world_dirty: false,
			connection: None,
		};

		// awful hack to set the minimum size
		let (old_rect, input) = cc.egui_ctx.input(|input| {
			(input.raw.screen_rect, egui::RawInput {
				screen_rect: Some(egui::Rect::ZERO),
				..input.raw.clone()
			})
		});
		let _ = cc.egui_ctx.run_ui(input, |ui| app.ui(ui));
		let size = cc.egui_ctx.globally_used_rect().size();
		log::debug!("Pre-measured minimum viewport size is {size:?}");
		// always None in my testing, but better to be safe
		cc.egui_ctx
			.input_mut(|input| input.raw.screen_rect = old_rect);
		cc.egui_ctx
			.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(size));
		// only load world after measure so it doesn't affect it
		if let Some((world, world_path)) = world {
			app.world = world;
			app.world_path = Some(world_path);
			cc.egui_ctx
				.send_viewport_cmd(egui::ViewportCommand::Title(format!(
					"{} - archiinventory",
					app.world.name
				)));
		}
		// try to load viewport positions, assuming world data is unchanged since last run
		if let Some(storage) = cc.storage
			&& let Some(viewports) = eframe::get_value::<Vec<SlotViewport>>(storage, "viewports")
		{
			for (slot, viewport) in app.world.slots.iter().zip(viewports) {
				slot.lock().unwrap().ui.viewport = Some(viewport);
			}
		}
		app
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
	fn slot_name_ui(
		ui: &mut egui::Ui,
		action: &mut SlotAction,
		modified: &mut bool,
		last: usize,
		i: usize,
		slot_arc: &Arc<Mutex<WorldSlot>>,
	) {
		let mut slot = slot_arc.lock().unwrap();
		let viewport_id = egui::ViewportId::from_hash_of(Arc::as_ptr(slot_arc));
		ui.horizontal(|ui| {
			if ui
				.button("\u{FF52}")
				.on_hover_ui(|ui| {
					ui.label("Delete this slot");
				})
				.clicked()
			{
				// TODO: this might want some confirmation if the slot has items?
				*action = SlotAction::Delete(i);
			}
			if ui
				.add_enabled(i > 0, egui::Button::new("\u{FF55}"))
				.on_hover_ui(|ui| {
					ui.label("Move slot up");
				})
				.clicked()
			{
				*action = SlotAction::Swap(i - 1, i);
			}
			if ui
				.add_enabled(i < last, egui::Button::new("\u{FF44}"))
				.on_hover_ui(|ui| {
					ui.label("Move slot down");
				})
				.clicked()
			{
				*action = SlotAction::Swap(i, i + 1);
			}
			if slot.ui.open {
				if ui
					.button("\u{FF46}")
					.on_hover_ui(|ui| {
						ui.label("Focus slot inventory window");
					})
					.clicked()
				{
					ui.send_viewport_cmd_to(viewport_id, egui::ViewportCommand::Focus);
				}
			} else {
				if ui
					.button("\u{FF56}")
					.on_hover_ui(|ui| {
						ui.label("View slot inventory in new window");
					})
					.clicked()
				{
					slot.ui.open = true;
				}
			}
		});
		let response = ui.add(
			egui::TextEdit::singleline(&mut slot.name)
				.hint_text("Slot name")
				.id_source(i),
		);
		if response.changed() {
			*modified = true;
			ui.request_repaint_of(viewport_id);
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
			});
		});
		egui::Panel::bottom("b").show_inside(ui, |ui| {
			ui.horizontal(|ui| {
				if ui.add_enabled(true, egui::Button::new("Connect")).clicked() {
					todo!("connect in background thread");
				}
				//ui.label("Connect status log can go here and still be extremely long or something and hopefully this doesn't clamp the minimum width");
			});
		});
		egui::CentralPanel::default()
			.frame(egui::Frame::central_panel(&ui.style()).inner_margin(egui::Margin::ZERO))
			.show_inside(ui, |ui| {
				let style_item_spacing_y = ui.style().spacing.item_spacing.y;

				let screen_size = ui.available_size();

				// world setup (connection info & slot names)
				egui::ScrollArea::vertical()
					.content_margin(egui::Margin::same(8))
					.show(ui, |ui| {
						let mut modified = false;
						ui.take_available_width();
						egui::Grid::new("world")
							.num_columns(2)
							//.with_row_color(slots_striped)
							.show(ui, |ui| {
								ui.take_available_width();
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
										.button("\u{FF53}")
										.on_hover_ui(|ui| {
											ui.label("Sort slots by name");
										})
										.clicked()
									{
										self.world.slots.sort_by_cached_key(|slot| {
											slot.lock().unwrap().name.clone()
										});
										modified = true;
									}
									if ui
										.button("\u{FF41}")
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
								for (i, slot_arc) in self.world.slots.iter().enumerate() {
									let pre_cursor = ui.cursor();
									Self::slot_name_ui(
										ui,
										&mut action,
										&mut modified,
										last,
										i,
										slot_arc,
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
									static NULL_SLOT: LazyLock<Arc<Mutex<WorldSlot>>> =
										LazyLock::new(Default::default);
									Self::slot_name_ui(
										ui,
										&mut action,
										&mut modified,
										0,
										0,
										&*NULL_SLOT,
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
						//// some overscroll for convenience
						//ui.allocate_space(egui::vec2(0.0, screen_size.y / 2.0));
					});
			});
		for slot_arc in &self.world.slots {
			let open = {
				let mut slot = slot_arc.lock().unwrap();
				slot.ui.open.then(|| {
					let mut vb = egui::ViewportBuilder {
						title: Some(format!(
							"{} ({}) - archiinventory",
							slot.name, self.world.name
						)),
						..Default::default()
					};
					// apply & remove viewport at first iteration
					if let Some(viewport) = slot.ui.viewport.take() {
						viewport.restore(&mut vb);
					}
					vb
				})
			};
			if let Some(vb) = open {
				let viewport_id = egui::ViewportId::from_hash_of(Arc::as_ptr(slot_arc));
				let slot = slot_arc.clone();
				ui.show_viewport_deferred(viewport_id, vb, move |ui, _vc| {
					let mut slot = slot.lock().unwrap();
					SlotUi::ui(&mut slot, ui);
				});
			}
		}
	}
}
impl eframe::App for App {
	fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) { self.ui(ui); }
	fn save(&mut self, storage: &mut dyn Storage) {
		log::debug!("Persistance save");
		if let Some(path) = &self.world_path {
			storage.set_string("world_path", encode_path(path));
		}
		// save per-slot viewports, which are only re-applied at startup
		let slot_viewports = self
			.world
			.slots
			.iter()
			.map(|slot| {
				slot.lock().unwrap().ui.viewport.unwrap_or_else(|| {
					self.egui_ctx
						.input_for(egui::ViewportId::from_hash_of(Arc::as_ptr(slot)), |input| {
							SlotViewport::new(input.viewport())
						})
				})
			})
			.collect::<Vec<_>>();
		eframe::set_value(storage, "viewports", &slot_viewports);
	}
}

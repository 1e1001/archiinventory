use std::mem::take;
use std::path::{Path, PathBuf};

use eframe::egui;
use multiline_logger::log;

use crate::APP_ID;
use crate::conn::Connection;
use crate::data::{WorldConnType, WorldFile, WorldFileSlot, decode_path, encode_path};
use crate::istr::Istr;
use crate::ui::search::{SlotSearcher, normalize_str};

mod search;

macro_rules! shortcuts {
	($($name:ident [$($modifier:ident)*] $key:ident;)*) => {
		$(const $name: egui::KeyboardShortcut = egui::KeyboardShortcut::new(egui::Modifiers::NONE$(.plus(egui::Modifiers::$modifier))*, egui::Key::$key);)*
	}
}

shortcuts! {
	SHORTCUT_NEW [CTRL] N;
	SHORTCUT_OPEN [CTRL] O;
	SHORTCUT_SAVE [CTRL] S;
	SHORTCUT_SAVE_AS [CTRL SHIFT] S;
	SHORTCUT_SLOT_DELETE [] Backspace;
	SHORTCUT_SLOT_UP [] ArrowUp;
	SHORTCUT_SLOT_DOWN [] ArrowDown;
	SHORTCUT_SLOT_MOVE_UP [CTRL] ArrowUp;
	SHORTCUT_SLOT_MOVE_DOWN [CTRL] ArrowDown;
	SHORTCUT_SLOT_TOGGLE [CTRL] Space;
}

const ICON_VIEW_NONE: &str = "\u{2796}";
const ICON_VIEW_SOME: &str = "\u{271A}";
const ICON_SORT_NONE: &str = "\u{2796}";
const ICON_SORT_UP: &str = "\u{23F6}";
const ICON_SORT_DOWN: &str = "\u{23F7}";
const ICON_FILTER_NONE: &str = "\u{2796}";
const ICON_FILTER_YES: &str = "\u{2714}";
const ICON_FILTER_NO: &str = "\u{1f5d9}";

fn consume_shortcuts<const N: usize>(
	ui: &egui::Ui,
	shortcuts: [egui::KeyboardShortcut; N],
) -> [bool; N] {
	ui.input_mut(|input| shortcuts.map(|sc| input.consume_shortcut(&sc)))
}

fn plural(i: usize) -> &'static str { if i == 1 { "" } else { "s" } }

pub struct App {
	storage_dir: PathBuf,
	world_path: Option<PathBuf>,
	world: WorldFile,
	world_dirty: bool,
	connection: Option<Connection>,
	slot_focus: usize,
	slot_searcher: SlotSearcher,
	// TODO: this is kinda a hack
	prev_window_title: String,
}

impl App {
	fn load_world(&mut self, path: Option<PathBuf>, world: WorldFile) {
		if !self.world_dirty || Self::dialog_overwrite_unsaved() {
			self.world_path = path;
			self.world = world;
			self.world_dirty = false;
			// reset other state
			self.connection = None;
			self.slot_searcher.inventory_changed();
			log::debug!("loaded world {:#?}", self.world);
		}
	}

	fn save_world(&mut self, force_save_as: bool) {
		log::debug!("saving world");
		let path = if force_save_as { None } else { self.world_path.as_ref() };
		if let Err(err) = if let Some(path) = path {
			self.world.save(path)
		} else if let Some(new_path) = self
			.dialog_filepicker_world()
			.set_title("Save world file")
			.set_file_name(format!("{}.json", self.world.name))
			.save_file()
		{
			self.world.save(self.world_path.insert(new_path))
		} else {
			Ok(())
		} {
			Self::dialog_error("Failed to save world", &err);
		} else {
			self.world_dirty = false;
		}
	}

	fn update_window_title(&mut self, ctx: &egui::Context) {
		let title = format!(
			"{}{} - archiinventory",
			self.world.name,
			if self.world_dirty { "*" } else { "" }
		);
		if title != self.prev_window_title {
			self.prev_window_title.clone_from(&title);
			ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
		}
	}

	fn mark_dirty(&mut self) { self.world_dirty = true; }

	pub fn new(cc: &eframe::CreationContext) -> Self {
		let storage_dir = eframe::storage_dir(APP_ID).expect("Failed to find system path");
		let mut res = Self {
			storage_dir,
			world_path: None,
			world: WorldFile::default(),
			world_dirty: false,
			slot_focus: 0,
			slot_searcher: SlotSearcher::default(),
			connection: None,
			prev_window_title: String::new(),
		};
		if let Some(storage) = cc.storage
			&& let Some(path) = storage.get_string("world_path")
		{
			match decode_path(&path) {
				Ok(path) => match WorldFile::load(&path) {
					Ok(world) => {
						res.load_world(Some(path), world);
					},
					Err(err) => log::warn!("Failed to open world\n{err}"),
				},
				Err(err) => log::warn!("Failed to decode world_path\n{err}"),
			}
		}
		res
	}

	fn dialog_overwrite_unsaved() -> bool {
		rfd::MessageDialog::new()
			.set_title("Unsaved world - archiinventory")
			.set_level(rfd::MessageLevel::Warning)
			.set_description("Current world is unsaved, and data will be lost.")
			.set_buttons(rfd::MessageButtons::OkCancel)
			.show() == rfd::MessageDialogResult::Ok
	}

	fn dialog_error(title: &str, err: &anyhow::Error) {
		rfd::MessageDialog::new()
			.set_title(format!("{title} - archiinventory"))
			.set_level(rfd::MessageLevel::Error)
			.set_description(err.to_string())
			.set_buttons(rfd::MessageButtons::Ok)
			.show();
	}

	fn dialog_filepicker_base(world_path: Option<&Path>, storage_dir: &Path) -> rfd::FileDialog {
		rfd::FileDialog::new()
			.set_directory(world_path.and_then(Path::parent).unwrap_or(storage_dir))
	}

	fn dialog_filepicker_world(&self) -> rfd::FileDialog {
		Self::dialog_filepicker_base(self.world_path.as_deref(), &self.storage_dir)
			.add_filter("archiinventory world", &["json"])
	}

	fn ui_file_management(&mut self, ui: &mut egui::Ui) {
		ui.horizontal(|ui| {
			let [new, open, save_as, save] = consume_shortcuts(ui, [
				SHORTCUT_NEW,
				SHORTCUT_OPEN,
				SHORTCUT_SAVE_AS,
				SHORTCUT_SAVE,
			]);
			if ui.button("New").clicked() || new {
				self.load_world(None, WorldFile::default());
			}
			if ui.button("Open").clicked() || open {
				// try picking a file
				if let Some(path) =
					self.dialog_filepicker_world().set_title("Open world file").pick_file()
				{
					match WorldFile::load(&path) {
						Ok(world) => self.load_world(Some(path), world),
						Err(err) => Self::dialog_error("Failed to load world", &err),
					}
				}
			}
			if ui.add_enabled(self.world_path.is_some(), egui::Button::new("Save")).clicked()
				|| save
			{
				self.save_world(false);
			}
			if ui.button("Save As").clicked() || save_as {
				self.save_world(true);
			}
		});
	}

	#[expect(clippy::too_many_lines, reason = "egui")]
	fn ui_slot_info(&mut self, ui: &mut egui::Ui) {
		let mut changed = false;
		egui::ScrollArea::vertical().show(ui, |ui| {
			let panel_height = ui.available_size().y;
			let mut panel_width = ui.max_rect().width();
			egui::Grid::new("slot info").num_columns(2).show(ui, |ui| {
				enum SlotAction {
					None,
					Swap(usize, usize),
					Remove(usize),
				}
				let min_cursor = ui.cursor().min.x;
				ui.label("Name");
				changed |= ui.text_edit_singleline(&mut self.world.name).changed();
				panel_width = ui.cursor().min.x - min_cursor;
				ui.end_row();
				ui.label("Type");
				ui.horizontal(|ui| {
					if ui
						.add_enabled(
							!matches!(self.world.conn_type, WorldConnType::Client),
							egui::Button::new("Client"),
						)
						.clicked()
					{
						self.world.conn_type = WorldConnType::Client;
						changed = true;
					}
					if ui
						.add_enabled(
							!matches!(self.world.conn_type, WorldConnType::Room),
							egui::Button::new("Room"),
						)
						.clicked()
					{
						self.world.conn_type = WorldConnType::Room;
						changed = true;
					}
				});
				ui.end_row();
				ui.label("Address");
				changed |= ui.text_edit_singleline(&mut self.world.address).changed();
				ui.end_row();
				ui.label("Password");
				changed |= ui
					.add_enabled(
						matches!(self.world.conn_type, WorldConnType::Client),
						egui::TextEdit::singleline(&mut self.world.password),
					)
					.changed();
				ui.end_row();
				let mut previous_response = None::<egui::Response>;
				let mut next_focus = false;
				let mut action = SlotAction::None;
				let last_i = self.world.slots.len().saturating_sub(1);
				for (i, slot) in self.world.slots.iter_mut().enumerate() {
					let new_items = slot.total_items.saturating_sub(slot.confirmed);
					ui.horizontal(|ui| {
						ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
						changed |= ui.checkbox(&mut slot.active, "").changed();
						if ui
							.add_enabled(
								self.slot_focus != i,
								egui::Button::new(if new_items == 0 {
									ICON_VIEW_NONE.to_owned()
								} else {
									format!("{ICON_VIEW_SOME} {new_items}")
								}),
							)
							.clicked()
						{
							self.slot_focus = i;
							self.slot_searcher.inventory_changed();
						}
					});
					let is_empty = slot.name.is_empty();
					let res = ui.text_edit_singleline(&mut slot.name);
					if take(&mut next_focus) {
						res.request_focus();
					}
					if res.has_focus() {
						let [up, down, move_up, move_down, toggle] = consume_shortcuts(ui, [
							SHORTCUT_SLOT_UP,
							SHORTCUT_SLOT_DOWN,
							SHORTCUT_SLOT_MOVE_UP,
							SHORTCUT_SLOT_MOVE_DOWN,
							SHORTCUT_SLOT_TOGGLE,
						]);
						if (up || move_up)
							&& let Some(prev) = previous_response.take()
						{
							prev.request_focus();
							if move_up {
								action = SlotAction::Swap(i - 1, i);
							}
						}
						if (down || move_down) && i < last_i {
							next_focus = true;
							if move_down {
								action = SlotAction::Swap(i, i + 1);
							}
						}
						if is_empty {
							let [delete] = consume_shortcuts(ui, [SHORTCUT_SLOT_DELETE]);
							if delete {
								action = SlotAction::Remove(i);
								if i == last_i
									&& let Some(prev) = previous_response.take()
								{
									prev.request_focus();
								}
							}
						}
						if toggle {
							slot.active ^= true;
							changed = true;
						}
					}
					changed |= res.changed();
					previous_response = Some(res);
					ui.end_row();
				}
				match action {
					SlotAction::None => {},
					SlotAction::Swap(a, b) => {
						self.world.slots.swap(a, b);
						if self.slot_focus == a {
							self.slot_focus = b;
						} else if self.slot_focus == b {
							self.slot_focus = a;
						}
						changed = true;
					},
					SlotAction::Remove(i) => {
						self.world.slots.remove(i);
						changed = true;
					},
				}
			});
			ui.horizontal(|ui| {
				if ui.button("Sort").clicked() {
					self.world.slots.sort_by_cached_key(|slot| normalize_str(&slot.name));
					changed = true;
				}
				let res = ui.button("Add");
				if res.clicked() {
					res.request_focus();
					self.world.slots.push(WorldFileSlot::default());
					changed = true;
				}
				if let Some(conn) = &self.connection
					&& conn.active()
				{
					if ui.button("Cancel").clicked() {
						self.connection = None;
					}
				} else if ui
					.add_enabled(
						self.world.slots.iter().find(|slot| slot.active).is_some(),
						egui::Button::new("Refresh"),
					)
					.clicked()
				{
					self.connection =
						Some(Connection::new(self.world.clone_for_connection(), ui.ctx().clone()));
				}
			});
			if let Some(conn) = &mut self.connection {
				ui.allocate_ui(egui::vec2(panel_width, 0.0), |ui| {
					conn.ui(ui, |name, total_items, inventory| {
						if let Some((i, slot)) = self
							.world
							.slots
							.iter_mut()
							.enumerate()
							.find(|(_, slot)| slot.name == name)
						{
							slot.total_items = total_items;
							if slot.confirmed > total_items {
								slot.confirmed = total_items;
								changed = true;
							}
							slot.inventory = inventory;
							if self.slot_focus == i {
								self.slot_searcher.inventory_changed();
							}
						}
					});
				});
			}
			ui.allocate_space(egui::vec2(0.0, panel_height / 2.0));
		});
		// todo: connection update here (and remember to invalidate searcher!)
		if changed {
			self.mark_dirty();
		}
	}

	fn ui_root(&mut self, ui: &mut egui::Ui) {
		// handle close button via modal
		if ui.ctx().input(|input| input.viewport().close_requested()) && self.world_dirty {
			match rfd::MessageDialog::new()
				.set_title("archiinventory - Unsaved World")
				.set_level(rfd::MessageLevel::Warning)
				.set_description("Save world before exiting?")
				.set_buttons(rfd::MessageButtons::YesNoCancel)
				.show()
			{
				rfd::MessageDialogResult::Yes => self.save_world(false),
				rfd::MessageDialogResult::No => {},
				_ => ui.ctx().send_viewport_cmd(egui::ViewportCommand::CancelClose),
			}
		}
		egui::Panel::left("file").show_inside(ui, |ui| {
			// TODO: add some short separators
			self.ui_file_management(ui);
			self.ui_slot_info(ui);
			self.update_window_title(ui);
		});
		egui::CentralPanel::default().show_inside(ui, |ui| {
			if self.world.slots.is_empty() {
				ui.heading("No slots");
			} else {
				let [up, down] =
					consume_shortcuts(ui, [SHORTCUT_SLOT_MOVE_UP, SHORTCUT_SLOT_MOVE_DOWN]);
				if up && self.slot_focus > 0 {
					self.slot_focus = self.slot_focus.saturating_sub(1);
					self.slot_searcher.inventory_changed();
				}
				if down {
					self.slot_focus += 1;
					self.slot_searcher.inventory_changed();
				}
				if self.slot_focus >= self.world.slots.len() {
					self.slot_focus = self.world.slots.len() - 1;
					self.slot_searcher.inventory_changed();
				}
				let slot = &mut self.world.slots[self.slot_focus];
				if self.slot_searcher.ui(ui, slot) {
					self.mark_dirty();
				}
			}
		});
	}
}

impl eframe::App for App {
	fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) { self.ui_root(ui); }

	fn save(&mut self, storage: &mut dyn eframe::Storage) {
		log::debug!("Persistence save");
		if let Some(path) = &self.world_path {
			storage.set_string("world_path", encode_path(path));
		}
		Istr::gc();
	}
}

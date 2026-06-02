use std::cmp::Reverse;
use std::ffi::OsString;
use std::mem::replace;
use std::path::{Path, PathBuf, SEPARATORS};

use eframe::{Storage, egui};
use hashbrown::HashMap;
use multiline_logger::log;
use ustr::Ustr;

use crate::APP_ID;
use crate::conn::Connection;
use crate::data::{InstanceLocalId, World, WorldItem, WorldSlot};

const OSSTR_VERSION: &str = include_str!(concat!(env!("OUT_DIR"), "/osstr.txt"));

// TODO: macro these up
const SHORTCUT_NEW: egui::KeyboardShortcut =
	egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::N);
const SHORTCUT_OPEN: egui::KeyboardShortcut =
	egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::O);
const SHORTCUT_SAVE: egui::KeyboardShortcut =
	egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::S);
const SHORTCUT_SAVE_AS: egui::KeyboardShortcut = egui::KeyboardShortcut::new(
	egui::Modifiers::CTRL.plus(egui::Modifiers::SHIFT),
	egui::Key::S,
);
const SHORTCUT_DEBUG_TOGGLE: egui::KeyboardShortcut = egui::KeyboardShortcut::new(
	egui::Modifiers::CTRL.plus(egui::Modifiers::SHIFT),
	egui::Key::D,
);
const SHORTCUT_SLOT_PREV: egui::KeyboardShortcut =
	egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::ArrowLeft);
const SHORTCUT_SLOT_NEXT: egui::KeyboardShortcut =
	egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::ArrowRight);
const SHORTCUT_SLOT_EXPORT: egui::KeyboardShortcut =
	egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::E);
const SHORTCUT_SLOT_IMPORT: egui::KeyboardShortcut =
	egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::I);

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
	#[expect(clippy::shadow_unrelated, reason = "they're related")]
	match String::from_utf8(bytes) {
		// if the string is valid utf-8, there's no need to do compatibility checks
		Ok(text) => Ok(text.into()),
		Err(err) => {
			let bytes = err.into_bytes();
			let expected = &OSSTR_VERSION[..OSSTR_VERSION.len() - 1];
			if version != expected {
				anyhow::bail!("Incompatible path format\nExpected {expected}\nGot {version}");
			}
			// SAFETY: we just checked that the bytes were encoded with the same target & rust version
			Ok(unsafe { OsString::from_encoded_bytes_unchecked(bytes) }.into())
		}
	}
}

fn dialog_overwrite_unsaved() -> bool {
	rfd::MessageDialog::new()
		.set_title("Unsaved world - archiinventory")
		.set_level(rfd::MessageLevel::Warning)
		.set_description("Current world is unsaved, and data will be lost.")
		.set_buttons(rfd::MessageButtons::OkCancel)
		.show()
		== rfd::MessageDialogResult::Ok
}
fn dialog_delete_slot() -> bool {
	rfd::MessageDialog::new()
		.set_title("Confirm deletion - archiinventory")
		.set_level(rfd::MessageLevel::Warning)
		.set_description("Deleting this slot will delete its item records.")
		.set_buttons(rfd::MessageButtons::OkCancel)
		.show()
		== rfd::MessageDialogResult::Ok
}
fn dialog_overwrite_table(
	slot: &str,
	old_game: &str,
	old_len: usize,
	new_game: &str,
	new_len: usize,
) -> bool {
	fn plural(i: usize) -> &'static str { if i == 1 { "" } else { "s" } }
	rfd::MessageDialog::new()
		.set_title("Load table - archiinventory")
		.set_level(rfd::MessageLevel::Warning)
		.set_description(if old_game == new_game {format!(
			"Slot {slot} has {old_len} item{}, which will be overwritten with {new_len} item{} from the table",
			plural(old_len),
			plural(new_len)
		)} else {
		format!(
			"Slot {slot} has {old_len} item{} from game {old_game}, which will be overwritten with {new_len} item{} from game {new_game} from the table",
			plural(old_len),
			plural(new_len)
		)
		})
		.set_buttons(rfd::MessageButtons::OkCancel)
		.show()
		== rfd::MessageDialogResult::Ok
}
fn dialog_merge_warning(message: String) -> bool {
	rfd::MessageDialog::new()
		.set_title("Data merge warning - archiinventory")
		.set_level(rfd::MessageLevel::Warning)
		.set_description(message)
		.set_buttons(rfd::MessageButtons::OkCancel)
		.show()
		== rfd::MessageDialogResult::Ok
}
fn dialog_error(title: &str, err: &anyhow::Error) {
	rfd::MessageDialog::new()
		.set_title(format!("{title} - archiinventory"))
		.set_level(rfd::MessageLevel::Error)
		.set_description(err.to_string())
		.set_buttons(rfd::MessageButtons::Ok)
		.show();
}

fn fuzzy_matches(needle: &str, haystack: &str) -> bool {
	let mut needle_iter = needle.chars().peekable();
	for ch in haystack.chars() {
		match needle_iter.peek() {
			None => return true,
			// TODO: extend to case insensitive unicode match?
			Some(c) if ch.eq_ignore_ascii_case(c) => _ = needle_iter.next(),
			Some(_) => {}
		}
	}
	needle_iter.peek().is_none()
}

// true = reverse order
#[derive(Clone, Copy)]
enum SlotViewSort {
	Count(bool),
	// sort by recent quantity is somewhat meaningless for resolving ties, so instead it's its own setting
	PercentProgression(bool),
	PercentUseful(bool),
	PercentTrap(bool),
	ItemName(bool),
}
impl Default for SlotViewSort {
	fn default() -> Self { Self::ItemName(false) }
}
impl SlotViewSort {
	fn select_or_flip(self, new: Self) -> Self {
		match (self, new) {
			(Self::Count(prev), Self::Count(_)) => Self::Count(!prev),
			(Self::ItemName(prev), Self::ItemName(_)) => Self::ItemName(!prev),
			(Self::PercentProgression(prev), Self::PercentProgression(_)) => {
				Self::PercentProgression(!prev)
			}
			(Self::PercentUseful(prev), Self::PercentUseful(_)) => Self::PercentUseful(!prev),
			(Self::PercentTrap(prev), Self::PercentTrap(_)) => Self::PercentTrap(!prev),
			(_, new) => new,
		}
	}
	// expects indexes to start sorted, so that fallback sort by name
	fn apply_sort(self, recent_first: bool, indexes: &mut [usize], items: &[SlotViewItem]) {
		// TODO: do this in one sort step?
		#[expect(
			clippy::cast_precision_loss,
			reason = "doesn't matter for quantity used"
		)]
		match self {
			SlotViewSort::Count(false) => indexes.sort_by_key(|&i| items[i].count),
			SlotViewSort::Count(true) => indexes.sort_by_key(|&i| Reverse(items[i].count)),
			SlotViewSort::PercentProgression(false) => indexes
				.sort_by_key(|&i| (items[i].progression as f32 / items[i].count as f32).to_bits()),
			SlotViewSort::PercentProgression(true) => indexes.sort_by_key(|&i| {
				Reverse((items[i].progression as f32 / items[i].count as f32).to_bits())
			}),
			SlotViewSort::PercentUseful(false) => {
				indexes
					.sort_by_key(|&i| (items[i].useful as f32 / items[i].count as f32).to_bits());
			}
			SlotViewSort::PercentUseful(true) => indexes.sort_by_key(|&i| {
				Reverse((items[i].useful as f32 / items[i].count as f32).to_bits())
			}),
			SlotViewSort::PercentTrap(false) => {
				indexes.sort_by_key(|&i| (items[i].trap as f32 / items[i].count as f32).to_bits());
			}
			SlotViewSort::PercentTrap(true) => indexes.sort_by_key(|&i| {
				Reverse((items[i].trap as f32 / items[i].count as f32).to_bits())
			}),
			SlotViewSort::ItemName(false) => indexes.sort_by_key(|&i| items[i].name),
			SlotViewSort::ItemName(true) => indexes.sort_by_key(|&i| Reverse(items[i].name)),
		}
		if recent_first {
			// false comes first
			indexes.sort_by_key(|&i| items[i].recent_count == 0);
		}
	}
}

struct SlotViewItem {
	id: i64,
	name: Ustr,
	// these counts are refreshed by sub-category filters (not implemented)
	count: usize,
	recent_count: usize,
	progression: usize,
	useful: usize,
	trap: usize,
	// raw list of this item
	items: Vec<WorldItem>,
}

#[derive(Default)]
struct SlotView {
	slot_id: InstanceLocalId,
	sort_by: SlotViewSort,
	recent_first: bool,
	filter_name: String,
	items: Vec<SlotViewItem>,
	filtered_indexes: Vec<usize>,
	filtered_indexes_stale: bool,
	viewing_item: Option<usize>,
}

impl SlotView {
	#[expect(clippy::shadow_unrelated, clippy::too_many_lines, reason = "ui code")]
	fn ui(
		&mut self,
		slot: &mut WorldSlot,
		slot_index: &mut usize,
		slot_count: usize,
		ui: &mut egui::Ui,
		dialog_filepicker_csv: impl Fn() -> rfd::FileDialog,
	) -> bool {
		// data caching
		if slot.id != self.slot_id || slot.ui_data_stale {
			// reload item data
			slot.ui_data_stale = false;
			self.slot_id = slot.id;
			let mut items_set = HashMap::new();
			for item in &slot.inventory {
				let cluster = items_set.entry(item.item_id).or_insert(SlotViewItem {
					count: 0,
					recent_count: 0,
					id: item.item_id,
					name: item.item_name,
					progression: 0,
					useful: 0,
					trap: 0,
					items: Vec::new(),
				});
				cluster.count += 1;
				if item.time > slot.confirmed {
					cluster.recent_count += 1;
				}
				if item.progression {
					cluster.progression += 1;
				}
				if item.useful {
					cluster.useful += 1;
				}
				if item.trap {
					cluster.trap += 1;
				}
				cluster.items.push(item.clone());
			}
			self.items = items_set.into_values().collect();
			self.items.sort_by_key(|i| i.name);
			self.filtered_indexes_stale = true;
			self.viewing_item = None;
		}
		let mut modified = false;
		// ui
		egui::Panel::top("slot").show_inside(ui, |ui| {
			ui.horizontal(|ui| {
				let (prev, next, export, import) = ui.input_mut(|input| {
					(
						input.consume_shortcut(&SHORTCUT_SLOT_PREV),
						input.consume_shortcut(&SHORTCUT_SLOT_NEXT),
						input.consume_shortcut(&SHORTCUT_SLOT_EXPORT),
						input.consume_shortcut(&SHORTCUT_SLOT_IMPORT),
					)
				});
				if ui
					.add_enabled(*slot_index > 0, egui::Button::new("Prev"))
					.clicked() || (prev && *slot_index > 0)
				{
					*slot_index -= 1;
				}
				if ui
					.add_enabled(*slot_index + 1 < slot_count, egui::Button::new("Next"))
					.clicked() || (next && *slot_index + 1 < slot_count)
				{
					*slot_index += 1;
				}
				if ui.button("Confirm").clicked() {
					// inventory is primarily sorted by time
					slot.confirmed = slot
						.inventory
						.last()
						.map(|item| item.time)
						.unwrap_or_default();
					modified = true;
					for item in &mut self.items {
						item.recent_count = 0;
					}
					// TODO: resort if sorting by recent
				}
				if (ui
					.button("Export")
					.on_hover_ui(|ui| _ = ui.label("Export CSV table of current slot items"))
					.clicked() || export)
					&& let Some(path) = dialog_filepicker_csv()
						.set_title("Export slot table")
						.set_file_name(format!("{}.csv", slot.name))
						.save_file() && let Err(err) = slot.save_table(path)
				{
					dialog_error("Failed to export table", &err);
				}
				if (ui
					.button("Import")
					.on_hover_ui(|ui| _ = ui.label("Import CSV table for slot items"))
					.clicked() || import)
					&& let Some(path) = dialog_filepicker_csv()
						.set_title("Export slot table")
						.pick_file()
				{
					// we need to get length before showing dialog
					match WorldSlot::load_table(slot.game, path) {
						Ok((new_game, new_inventory)) => {
							if slot.inventory.is_empty()
								|| dialog_overwrite_table(
									&slot.name,
									&slot.game,
									slot.inventory.len(),
									&new_game,
									new_inventory.len(),
								) {
								slot.inventory = new_inventory;
								slot.game = new_game;
								slot.ui_data_stale = true;
								modified = true;
							}
						}
						Err(err) => dialog_error("Failed to import table", &err),
					}
				}
			});
		});
		egui::CentralPanel::default().show_inside(ui, |ui| {
			if slot.game.is_empty() {
				ui.heading(&slot.name);
			} else {
				ui.heading(format!("{} ({})", slot.name, slot.game));
			}
			// TODO: column for most recent time?
			// TODO: color item by modal flag combo (create colors for all 8 types)
			#[expect(
				clippy::cast_possible_truncation,
				clippy::cast_sign_loss,
				reason = "rough estimate anyways"
			)]
			let extra_rows = (ui.available_height() / 40.0) as usize;
			egui_extras::TableBuilder::new(ui)
				.striped(true)
				.cell_layout(egui::Layout::default().with_cross_align(egui::Align::RIGHT))
				.column(egui_extras::Column::auto())
				.column(egui_extras::Column::auto())
				.column(egui_extras::Column::auto())
				.column(egui_extras::Column::auto())
				.column(egui_extras::Column::auto())
				.column(egui_extras::Column::auto())
				.column(egui_extras::Column::remainder())
				.header(20.0, |mut header| {
					header.col(|ui| {
						// match spacing when there are no items
						ui.add_visible(false, egui::Button::new("V"));
					});
					header.col(|ui| {
						ui.horizontal(|ui| {
							if ui
								.button(match self.sort_by {
									SlotViewSort::Count(false) => "U",
									SlotViewSort::Count(true) => "D",
									_ => "S",
								})
								.clicked()
							{
								self.sort_by =
									self.sort_by.select_or_flip(SlotViewSort::Count(true));
								self.filtered_indexes_stale = true;
							}
							ui.label("Count");
						});
					});
					header.col(|ui| {
						ui.horizontal(|ui| {
							if ui
								.button(if self.recent_first { "D" } else { "S" })
								.clicked()
							{
								self.recent_first = !self.recent_first;
								self.filtered_indexes_stale = true;
							}
							ui.label("New")
						});
					});
					header.col(|ui| {
						ui.horizontal(|ui| {
							if ui
								.button(match self.sort_by {
									SlotViewSort::PercentProgression(false) => "U",
									SlotViewSort::PercentProgression(true) => "D",
									_ => "S",
								})
								.clicked()
							{
								self.sort_by = self
									.sort_by
									.select_or_flip(SlotViewSort::PercentProgression(true));
								self.filtered_indexes_stale = true;
							}
							ui.label("Prog.");
						});
					});
					header.col(|ui| {
						ui.horizontal(|ui| {
							if ui
								.button(match self.sort_by {
									SlotViewSort::PercentUseful(false) => "U",
									SlotViewSort::PercentUseful(true) => "D",
									_ => "S",
								})
								.clicked()
							{
								self.sort_by = self
									.sort_by
									.select_or_flip(SlotViewSort::PercentUseful(true));
								self.filtered_indexes_stale = true;
							}
							ui.label("Useful");
						});
					});
					header.col(|ui| {
						ui.horizontal(|ui| {
							if ui
								.button(match self.sort_by {
									SlotViewSort::PercentTrap(false) => "U",
									SlotViewSort::PercentTrap(true) => "D",
									_ => "S",
								})
								.clicked()
							{
								self.sort_by =
									self.sort_by.select_or_flip(SlotViewSort::PercentTrap(true));
								self.filtered_indexes_stale = true;
							}
							ui.label("Trap.");
						});
					});
					header.col(|ui| {
						ui.with_layout(egui::Layout::default(), |ui| {
							ui.horizontal(|ui| {
								ui.label("Item name");
								if ui
									.button(match self.sort_by {
										SlotViewSort::ItemName(false) => "U",
										SlotViewSort::ItemName(true) => "D",
										_ => "S",
									})
									.clicked()
								{
									self.sort_by =
										self.sort_by.select_or_flip(SlotViewSort::ItemName(false));
									self.filtered_indexes_stale = true;
								}
								if ui.text_edit_singleline(&mut self.filter_name).changed() {
									self.filtered_indexes_stale = true;
								}
							});
						});
					});
				})
				.body(|body| {
					if self.filtered_indexes_stale {
						self.filtered_indexes = self
							.items
							.iter()
							.enumerate()
							.filter_map(|(i, item)| {
								fuzzy_matches(&self.filter_name, &item.name).then_some(i)
							})
							.collect();
						self.sort_by.apply_sort(
							self.recent_first,
							&mut self.filtered_indexes,
							&self.items,
						);
						self.filtered_indexes_stale = false;
					}
					body.rows(20.0, self.filtered_indexes.len() + extra_rows, |mut row| {
						#[expect(clippy::cast_precision_loss, reason = "interface")]
						fn percent(i: usize, t: usize) -> String {
							format!("{:.0}%", 100.0 * i as f32 / t as f32)
						}
						let Some(&index) = self.filtered_indexes.get(row.index()) else {
							return;
						};
						let item = &self.items[index];
						row.col(|ui| {
							if ui.button("V").clicked() {
								self.viewing_item = Some(index);
							}
						});
						row.col(|ui| _ = ui.label(format!("{}", item.count)));
						row.col(|ui| {
							ui.label(if item.recent_count == 0 {
								String::new()
							} else {
								format!("+{}", item.recent_count)
							});
						});
						row.col(|ui| _ = ui.label(percent(item.progression, item.count)));
						row.col(|ui| _ = ui.label(percent(item.useful, item.count)));
						row.col(|ui| _ = ui.label(percent(item.trap, item.count)));
						row.col(|ui| {
							ui.with_layout(egui::Layout::default(), |ui| {
								ui.label(&*item.name)
									.on_hover_ui(|ui| _ = ui.label(format!("ID: {}", item.id)));
							});
						});
					});
					// TODO: add half-screen overscroll like slot list
					// might require modifying table
				});
		});
		modified
	}
}

pub struct App {
	storage_dir: PathBuf,
	world_path: Option<PathBuf>,
	world: World,
	world_dirty: bool,
	world_focus: usize,
	slot_view: SlotView,
	connection: Option<Connection>,
	debug_editing_enable: bool,
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
				Ok(path) => match World::load(&path) {
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
		let mut app = Self {
			storage_dir,
			world: world.unwrap_or_default(),
			world_path,
			world_dirty: false,
			world_focus: 0,
			slot_view: SlotView::default(),
			connection: None,
			debug_editing_enable: false,
		};
		app.refresh_window_title(&cc.egui_ctx);
		app
	}
	fn save_world(&mut self, ui: &mut egui::Ui, force_save_as: bool) {
		// "save as" dialog
		if force_save_as || self.world_path.is_none() {
			if let Some(path) = self
				.dialog_filepicker_ainv()
				.set_title("Save world file")
				.set_file_name(format!("{}.ainv", self.world.name))
				.save_file()
			{
				self.world_path = Some(path);
			} else {
				return;
			}
		}
		log::info!("Saving world");
		if let Err(err) = self.world.save(self.world_path.as_ref().unwrap()) {
			dialog_error("Failed to save world", &err);
		} else {
			self.world_dirty = false;
			self.refresh_window_title(ui);
		}
	}
	fn dialog_filepicker_base(world_path: Option<&Path>, storage_dir: &Path) -> rfd::FileDialog {
		rfd::FileDialog::new()
			.set_directory(world_path.and_then(Path::parent).unwrap_or(storage_dir))
	}
	fn dialog_filepicker_ainv(&self) -> rfd::FileDialog {
		Self::dialog_filepicker_base(self.world_path.as_deref(), &self.storage_dir)
			.add_filter("archiinventory world", &["ainv", "json"])
	}
	fn dialog_filepicker_csv(world_path: Option<&Path>, storage_dir: &Path) -> rfd::FileDialog {
		Self::dialog_filepicker_base(world_path, storage_dir).add_filter("CSV table", &["csv"])
	}
	fn modified_world(&mut self, ctx: &egui::Context) {
		if !replace(&mut self.world_dirty, true) {
			self.refresh_window_title(ctx);
		}
	}
	fn refresh_window_title(&mut self, ctx: &egui::Context) {
		ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!(
			"{}{} - archiinventory",
			self.world.name,
			if self.world_dirty { "*" } else { "" }
		)));
	}
	// TODO: bring this out to its own struct and add:
	// - up/down buttons should focus the previous/next button (so you can press enter)
	// - good icons (does svg work?)
	fn slot_name_ui(
		ui: &mut egui::Ui,
		action: &mut SlotAction,
		modified: &mut bool,
		world_focus: &mut usize,
		last: usize,
		i: usize,
		slot: &mut WorldSlot,
	) {
		ui.horizontal(|ui| {
			if ui
				.button("X")
				.on_hover_ui(|ui| _ = ui.label("Delete this slot"))
				.clicked() && (slot.inventory.is_empty() || dialog_delete_slot())
			{
				*action = SlotAction::Delete(i);
				if *world_focus > i {
					*world_focus -= 1;
				}
			}
			if ui
				.add_enabled(i > 0, egui::Button::new("U"))
				.on_hover_ui(|ui| _ = ui.label("Move slot up"))
				.clicked()
			{
				*action = SlotAction::Swap(i - 1, i);
				if *world_focus == i {
					*world_focus -= 1;
				} else if *world_focus == i - 1 {
					*world_focus += 1;
				}
			}
			if ui
				.add_enabled(i < last, egui::Button::new("D"))
				.on_hover_ui(|ui| _ = ui.label("Move slot down"))
				.clicked()
			{
				*action = SlotAction::Swap(i, i + 1);
				if *world_focus == i {
					*world_focus += 1;
				} else if *world_focus == i + 1 {
					*world_focus -= 1;
				}
			}
			// TODO: change icon based on slot's unacknowledged items (how to get this info?)
			if ui
				.add_enabled(*world_focus != i, egui::Button::new("V"))
				.on_hover_ui(|ui| _ = ui.label("View slot inventory"))
				.clicked()
			{
				*world_focus = i;
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
	fn panel_hack_top_ui(&mut self, ui: &mut egui::Ui) {
		// world file management
		ui.horizontal(|ui| {
			let (new, open, save, save_as) = ui.input_mut(|input| {
				self.debug_editing_enable ^= input.consume_shortcut(&SHORTCUT_DEBUG_TOGGLE);
				(
					input.consume_shortcut(&SHORTCUT_NEW),
					input.consume_shortcut(&SHORTCUT_OPEN),
					input.consume_shortcut(&SHORTCUT_SAVE),
					input.consume_shortcut(&SHORTCUT_SAVE_AS),
				)
			});
			if (ui.button("New").clicked() || new)
				&& (!self.world_dirty || dialog_overwrite_unsaved())
			{
				log::info!("Resetting world");
				self.world_path = None;
				self.world_dirty = false;
				self.world_focus = 0;
				self.world = World::default();
				self.refresh_window_title(ui);
			}
			if (ui.button("Open").clicked() || open)
				&& let Some(path) = self
					.dialog_filepicker_ainv()
					.set_title("Open world file")
					.pick_file()
			{
				match World::load(&path) {
					Ok(new_world) => {
						// TODO: replace with quit save dialog
						if !self.world_dirty || dialog_overwrite_unsaved() {
							log::info!("Loading world");
							self.world_path = Some(path);
							self.world_dirty = false;
							self.world_focus = 0;
							self.world = new_world;
							self.refresh_window_title(ui);
						}
					}
					Err(err) => {
						dialog_error("Failed to open world", &err);
					}
				}
			}
			if ui
				.add_enabled(self.world_path.is_some(), egui::Button::new("Save"))
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
								.split_inclusive(SEPARATORS)
							{
								ui.label(segment);
							}
						});
					});
				})
				.clicked() || save
			{
				self.save_world(ui, false);
			}
			if ui.button("Save As").clicked() || save_as {
				self.save_world(ui, true);
			}
			//ui.add_space(8.0);
			//if ui.button("Help").clicked() {
			//	rfd::MessageDialog::new()
			//		.set_title("Help - archiinventory")
			//		.set_description("good luck :)")
			//		.set_level(rfd::MessageLevel::Info)
			//		.set_buttons(rfd::MessageButtons::YesNo)
			//		.show();
			//}
		});
	}
	fn panel_hack_bottom_ui(&mut self, ui: &mut egui::Ui) {
		ui.horizontal(|ui| {
			if let Some(conn) = &mut self.connection
				&& conn.is_running()
			{
				if ui.button("Cancel").clicked() {
					conn.cancel();
				}
			} else if ui.button("Connect").clicked()
				&& let Some(mut conn) = self
					.connection
					.replace(Connection::new(&self.world, ui.ctx()))
			{
				conn.cancel();
			}
			if let Some(conn) = &mut self.connection
				&& let Some(slots) = conn.ui_update(ui)
			{
				// got new slot data from connection, merge together
				if self.world.merge_interactive(slots, dialog_merge_warning) {
					self.modified_world(ui);
				}
				// TODO: delete connection after this point? idk if showing "done!" is useful
			}
		});
	}
	#[expect(clippy::too_many_lines, reason = "rendering")]
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
				rfd::MessageDialogResult::Yes => self.save_world(ui, false),
				rfd::MessageDialogResult::No => {}
				_ => ui
					.ctx()
					.send_viewport_cmd(egui::ViewportCommand::CancelClose),
			}
		}
		egui::Panel::left("world")
			.resizable(true)
			.frame(egui::Frame::central_panel(ui.style()).inner_margin(egui::Margin::ZERO))
			.show_inside(ui, |ui| {
				// make panel not expand outer panel
				struct PanelHack<'app>(&'app mut App);
				impl egui::Widget for PanelHack<'_> {
					fn ui(self, ui: &mut egui::Ui) -> egui::Response {
						let top_res = egui::Panel::top("management")
							.show_inside(ui, |ui| {
								self.0.panel_hack_top_ui(ui);
							})
							.response;
						let mut res = egui::Panel::bottom("connect")
							.show_inside(ui, |ui| {
								self.0.panel_hack_bottom_ui(ui);
							})
							.response;
						// awful result packing
						res.rect.min.x = ui.cursor().height();
						res.rect.min.y = ui.cursor().min.y;
						// TODO: this is the wrong metric! and panel oversizing still happens
						res.rect.max.x = top_res.rect.width();
						res
					}
				}
				let screen_size = ui.available_size();
				let style_item_spacing_y = ui.style().spacing.item_spacing.y;
				let response = ui.place(ui.max_rect(), PanelHack(self));
				ui.set_min_width(response.rect.max.x.min(ui.max_rect().width()));
				ui.add_space(response.rect.min.y + style_item_spacing_y);

				// world setup (connection info & slot names)
				egui::ScrollArea::vertical()
					.max_height(response.rect.min.x - 2.0 * style_item_spacing_y)
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
									self.refresh_window_title(ui);
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
										.on_hover_ui(|ui| _ = ui.label("Sort slots by name"))
										.clicked()
									{
										let focused_slot_id = self
											.world
											.slots
											.get(self.world_focus)
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
											.map(|(i, _)| i)
											.unwrap_or_default();
										modified = true;
									}
									if ui
										.button("A")
										.on_hover_ui(|ui| _ = ui.label("Add a new slot"))
										.clicked()
									{
										action = SlotAction::Focus(self.world.slots.len());
										self.world.slots.push(WorldSlot::default());
										modified = true;
									}
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
										&mut WorldSlot::default(),
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
						if self.debug_editing_enable {
							let mut data = self
								.world
								.banished_games
								.as_deref()
								.unwrap_or_default()
								.join("\n");
							if !data.is_empty() {
								data.push('\n');
							}
							ui.label("DEBUG banished_games:");
							if ui.text_edit_multiline(&mut data).changed() {
								let list = data
									.split('\n')
									.filter(|line| !line.is_empty())
									.map(String::from)
									.collect::<Vec<_>>();
								self.world.banished_games = (!list.is_empty()).then_some(list);
								modified = true;
							}
						}
						if modified {
							self.modified_world(ui);
						}
						// some overscroll for convenience
						ui.allocate_space(egui::vec2(0.0, screen_size.y / 2.0));
					});
			});
		let slot_count = self.world.slots.len();
		self.world_focus = self.world_focus.min(slot_count.saturating_sub(1));
		if let Some(slot) = self.world.slots.get_mut(self.world_focus) {
			if self
				.slot_view
				.ui(slot, &mut self.world_focus, slot_count, ui, || {
					Self::dialog_filepicker_csv(self.world_path.as_deref(), &self.storage_dir)
				}) {
				self.modified_world(ui);
			}
		} else {
			egui::CentralPanel::default().show_inside(ui, |ui| {
				ui.heading("No slot selected");
			});
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
	}
}

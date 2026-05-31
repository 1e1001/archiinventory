use std::cmp::Ordering;
use std::ffi::OsStr;
use std::fs::{File, copy as file_copy};
use std::io::{Read, Seek, Write};
use std::mem::take;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering as AtomicOrdering};

use hashbrown::HashMap;
use hashbrown::hash_map::Entry as HashMapEntry;
use lzma_rust2::{LzipOptions, LzipReader, LzipWriter};
use multiline_logger::log;
use serde::{Deserialize, Serialize};
use ustr::Ustr;

// TODO: these Ustr will leak memory over the course of the program
// consider modifying archipelago_rs to give reference-tracking strings.
// using hashset<weak<str>> to index / gc them

#[derive(Debug, Clone, Serialize, Deserialize, Eq)]
pub struct WorldItem {
	// unix timestamp of first appearance
	pub time: u64,
	pub item_id: i64,
	pub item_name: Ustr,
	pub sender_id: u32,
	// TODO: store slot alias once i have reference tracked strings
	pub sender_name: Ustr,
	pub loc_id: i64,
	pub loc_name: Ustr,
	pub loc_game: Ustr,
	pub progression: bool,
	pub useful: bool,
	pub trap: bool,
}

// items are ordered by time, item id, sender id, location id. which is also used for equality check
impl PartialEq for WorldItem {
	fn eq(&self, other: &Self) -> bool {
		self.time == other.time
			&& self.item_id == other.item_id
			&& self.sender_id == other.sender_id
			&& self.loc_id == other.loc_id
	}
}

impl Ord for WorldItem {
	fn cmp(&self, other: &Self) -> Ordering {
		self.time
			.cmp(&other.time)
			.then_with(|| self.item_id.cmp(&other.item_id))
			.then_with(|| self.sender_id.cmp(&other.sender_id))
			.then_with(|| self.loc_id.cmp(&other.loc_id))
	}
}

impl PartialOrd for WorldItem {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> { Some(self.cmp(other)) }
}

// WorldItem with extra game field
#[derive(Serialize, Deserialize)]
struct TableWorldItem {
	pub time: u64,
	pub item_id: i64,
	pub item_name: Ustr,
	pub item_game: Ustr,
	pub sender_id: u32,
	pub sender_name: Ustr,
	pub loc_id: i64,
	pub loc_name: Ustr,
	pub loc_game: Ustr,
	pub progression: bool,
	pub useful: bool,
	pub trap: bool,
}

impl TableWorldItem {
	fn new(game: Ustr, item: &WorldItem) -> Self {
		Self {
			time: item.time,
			item_id: item.item_id,
			item_name: item.item_name,
			item_game: game,
			sender_id: item.sender_id,
			sender_name: item.sender_name,
			loc_id: item.loc_id,
			loc_name: item.loc_name,
			loc_game: item.loc_game,
			progression: item.progression,
			useful: item.useful,
			trap: item.trap,
		}
	}
	fn unpack(self) -> (Ustr, WorldItem) {
		(self.item_game, WorldItem {
			time: self.time,
			item_id: self.item_id,
			item_name: self.item_name,
			sender_id: self.sender_id,
			sender_name: self.sender_name,
			loc_id: self.loc_id,
			loc_name: self.loc_name,
			loc_game: self.loc_game,
			progression: self.progression,
			useful: self.useful,
			trap: self.trap,
		})
	}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct InstanceLocalId(u32);
impl Default for InstanceLocalId {
	fn default() -> Self {
		static COUNTER: AtomicU32 = AtomicU32::new(0);
		Self(COUNTER.fetch_add(1, AtomicOrdering::Relaxed))
	}
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct WorldSlot {
	// TODO: additional setting to enable fetching slot (e.g. completed?)
	pub name: String,
	#[serde(skip)]
	pub id: InstanceLocalId,
	#[serde(skip)]
	pub ui_data_stale: bool,
	pub confirmed: u64,
	// if game changes between runs, our item list is kinda useless
	pub game: Ustr,
	// should always be sorted
	pub inventory: Vec<WorldItem>,
}

impl WorldSlot {
	pub fn clone_for_conn(&self) -> Self {
		Self {
			name: self.name.clone(),
			id: self.id,
			ui_data_stale: false,
			confirmed: self.confirmed,
			game: self.game,
			// connection reports new inventory back to main thread
			inventory: Vec::new(),
		}
	}
	pub fn save_table<A: AsRef<Path>>(&self, path: A) -> anyhow::Result<()> {
		let mut writer = csv::Writer::from_path(path)?;
		for item in &self.inventory {
			writer.serialize(TableWorldItem::new(self.game, item))?;
		}
		Ok(())
	}
	pub fn load_table<A: AsRef<Path>>(
		game: Ustr,
		path: A,
	) -> anyhow::Result<(Ustr, Vec<WorldItem>)> {
		let mut reader = csv::Reader::from_path(path)?;
		let mut current_game = None;
		let mut inventory = reader
			.deserialize::<TableWorldItem>()
			.map(|res| {
				res.map_err(anyhow::Error::from).and_then(|item| {
					let (item_game, item) = item.unpack();
					if current_game
						.replace(item_game)
						.is_none_or(|old_game| old_game == item_game)
					{
						Ok(item)
					} else {
						anyhow::bail!("not all rows have the same item_game")
					}
				})
			})
			.collect::<anyhow::Result<Vec<_>>>()?;
		inventory.sort();
		Ok((current_game.unwrap_or(game), inventory))
	}
}

#[derive(Default, Serialize, Deserialize)]
pub struct World {
	pub name: String,
	pub address: String,
	pub password: String,
	pub slots: Vec<WorldSlot>,
	// list of games to ignore locations for, until archipelago_rs can properly handle them
	pub banished_games: Option<Vec<String>>,
}

pub type MergeSlots = Vec<WorldSlot>;

impl World {
	pub fn clone_for_conn(&self) -> Self {
		Self {
			name: self.name.clone(),
			address: self.address.clone(),
			password: self.password.clone(),
			slots: self.slots.iter().map(WorldSlot::clone_for_conn).collect(),
			banished_games: self.banished_games.clone(),
		}
	}
	/// add new items, returns true if something happened
	pub fn merge_interactive<F: FnMut(String) -> bool>(
		&mut self,
		new_slots: MergeSlots,
		mut confirm: F,
	) -> bool {
		// user might've modified slots while connected, so merge based on ids instead of names
		let mut new_slots_by_id = new_slots
			.into_iter()
			.map(|slot| (slot.id, slot))
			.collect::<HashMap<_, _>>();
		let mut any_slots_modified = false;
		for slot in &mut self.slots {
			if let Some(new_slot) = new_slots_by_id.remove(&slot.id) {
				log::trace!(
					"Merging slot {} with received slot {}",
					slot.name,
					new_slot.name
				);
				if new_slot.game == slot.game {
					any_slots_modified = true;
					slot.ui_data_stale = true;
					let mut new_inventory_by_ids = new_slot
						.inventory
						.into_iter()
						.map(|item| ((item.item_id, item.sender_id, item.loc_id), item))
						.collect::<HashMap<_, _>>();
					for item in take(&mut slot.inventory) {
						match new_inventory_by_ids.entry((
							item.item_id,
							item.sender_id,
							item.loc_id,
						)) {
							// should never happen, but preserve item if it does
							HashMapEntry::Vacant(entry) => {
								entry.insert(item);
							}
							// pull back oldest seen time, but keep new item metadata
							HashMapEntry::Occupied(mut entry) => entry.get_mut().time = item.time,
						}
					}
					slot.inventory = new_inventory_by_ids
						.into_iter()
						.map(|(_, item)| item)
						.collect();
					slot.inventory.sort();
				} else {
					// game changed, so ids are incomparable, just give up and overwrite
					if slot.inventory.is_empty()
						|| confirm(format!(
							"Slot {} changed game from {} to {}. The inventory will be cleared.",
							slot.name, slot.game, new_slot.game
						)) {
						any_slots_modified = true;
						slot.ui_data_stale = true;
						slot.game = new_slot.game;
						slot.inventory = new_slot.inventory;
						slot.inventory.sort();
					} else {
						// skip slot
					}
				}
			} else {
				// we have no items for this slot
			}
		}
		any_slots_modified
	}

	pub fn load<A: AsRef<Path>>(path: A) -> anyhow::Result<Self> {
		let mut file = File::open(path)?;
		// manually check for LZIP data, since LzipReader seems to return eof on invalid data?
		let mut buf = [0; 4];
		file.read_exact(&mut buf)?;
		file.rewind()?;
		match &buf {
			b"LZIP" => {
				let reader = LzipReader::new(file);
				Ok(ciborium::from_reader(reader)?)
			}
			_ => Ok(serde_json::from_reader(file)?),
		}
	}
	pub fn save<A: AsRef<Path>>(&self, path: A) -> anyhow::Result<()> {
		let path = path.as_ref();
		let ext = path.extension().and_then(OsStr::to_str);
		if ext == Some("json") || ext == Some("ainv") {
			// TODO: does this preserve creation time?
			if let Err(err) = file_copy(path, path.with_added_extension("bk")) {
				log::warn!("Failed to backup old save: {err}");
			}
		}
		let mut file = File::create(path)?;
		// only save raw if explicitly requested
		if path.extension() == Some(OsStr::new("json")) {
			serde_json::to_writer(&mut file, self)?;
			file.flush()?;
		} else {
			// 7 might be the default preset but this probably works fine
			let mut writer = LzipWriter::new(file, LzipOptions::with_preset(6));
			ciborium::into_writer(self, &mut writer)?;
			writer.finish()?.flush()?;
		}
		Ok(())
	}
}

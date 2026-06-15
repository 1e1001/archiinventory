use std::fs::{File, rename};
use std::io::Write;
use std::path::{Path, PathBuf};

use multiline_logger::log;
use serde::{Deserialize, Serialize};

use crate::istr::Istr;

//#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
//pub struct InstanceLocalId(u32);
//impl Default for InstanceLocalId {
//	fn default() -> Self {
//		static COUNTER: AtomicU32 = AtomicU32::new(0);
//		Self(COUNTER.fetch_add(1, AtomicOrdering::Relaxed))
//	}
//}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub enum WorldConnType {
	Client,
	#[default]
	Room,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlotItemInstance {
	pub index: usize,
	pub from_id: u32,
	pub from_name: Istr,
	pub at_id: i64,
	pub at_name: Istr,
	pub at_game: Istr,
	pub type_flags: [bool; 3],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlotItemGroup {
	pub id: i64,
	pub name: Istr,
	pub instances: Vec<SlotItemInstance>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WorldFileSlot {
	pub name: String,
	pub active: bool,
	pub confirmed: usize,
	#[serde(skip)]
	pub total_items: usize,
	#[serde(skip)]
	pub inventory: Vec<SlotItemGroup>,
}

impl Default for WorldFileSlot {
	fn default() -> Self {
		Self {
			name: String::new(),
			active: true,
			confirmed: 0,
			total_items: 0,
			inventory: Vec::new(),
		}
	}
}

impl WorldFileSlot {
	fn clone_for_connection(&self) -> Self {
		Self { name: self.name.clone(), active: self.active, ..Default::default() }
	}
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct WorldFile {
	pub name: String,
	pub conn_type: WorldConnType,
	pub address: String,
	pub password: String,
	pub slots: Vec<WorldFileSlot>,
}

impl WorldFile {
	pub fn load<A: AsRef<Path>>(path: A) -> anyhow::Result<Self> {
		Ok(serde_json::from_reader(File::open(path)?)?)
	}

	pub fn save<A: AsRef<Path>>(&self, path: A) -> anyhow::Result<()> {
		let path = path.as_ref();
		// TODO: does this preserve creation time?
		if let Err(err) = rename(path, path.with_added_extension("bk")) {
			log::warn!("Failed to backup old save: {err}");
		}
		let mut file = File::create(path)?;
		serde_json::to_writer(&mut file, self)?;
		file.flush()?;
		Ok(())
	}

	pub fn clone_for_connection(&self) -> Self {
		Self {
			name: self.name.clone(),
			conn_type: self.conn_type.clone(),
			address: self.address.clone(),
			password: self.password.clone(),
			slots: self.slots.iter().map(WorldFileSlot::clone_for_connection).collect(),
		}
	}
}

/// pack path into a format storable in egui storage
pub fn encode_path(path: &Path) -> String {
	let path = path.as_os_str();
	if let Some(path) = path.to_str() {
		let mut res = path.to_owned();
		res.push('@');
		res
	} else {
		#[cfg(target_family = "unix")]
		let (marker, bytes) = {
			use std::os::unix::ffi::OsStrExt;
			('A', path.as_bytes())
		};
		// TODO: untested (is the reborrow needed?)
		#[cfg(target_family = "windows")]
		let (marker, bytes) = {
			use std::os::windows::ffi::OsStrExt;
			('B', path.encode_wide().flat_map(u16::to_le_bytes).collect::<Vec<_>>())
		};
		#[cfg(target_family = "windows")]
		let bytes = &bytes;
		// TODO: untested
		#[cfg(target_family = "wasm")]
		let (marker, bytes) = {
			use std::os::wasm::ffi::OsStrExt;
			('C', path.as_bytes())
		};
		let mut res = z85::encode(bytes);
		res.push(marker);
		res
	}
}
pub fn decode_path(path: &str) -> anyhow::Result<PathBuf> {
	if path.is_empty() {
		anyhow::bail!("empty decode path");
	}
	let tail = path.len() - 1;
	match path.as_bytes().last() {
		Some(b'@') => Ok(PathBuf::from(&path[..tail])),
		#[cfg(target_family = "unix")]
		Some(b'A') => todo!("unix"),
		#[cfg(target_family = "windows")]
		Some(b'B') => todo!("windows"),
		#[cfg(target_family = "wasm")]
		Some(b'C') => todo!("wasm"),
		t => anyhow::bail!("invalid decode type {t:?}"),
	}
}

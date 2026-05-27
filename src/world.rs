use std::sync::{Arc, Mutex, atomic::{AtomicU32, Ordering}};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct InstanceLocalId(u32);
impl Default for InstanceLocalId {
	fn default() -> Self {
		static COUNTER: AtomicU32 = AtomicU32::new(0);
		Self(COUNTER.fetch_add(1, Ordering::Relaxed))
	}
}

#[derive(Default, Serialize, Deserialize)]
pub struct WorldSlot {
	pub name: String,
	#[serde(skip)]
	pub id: InstanceLocalId,
}

/// for moving to conn
impl Clone for WorldSlot {
	fn clone(&self) -> Self {
		Self {
			name: self.name.clone(),
			id: self.id,
			// don't copy inventory
		}
	}
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct World {
	pub name: String,
	pub address: String,
	pub password: String,
	pub slots: Vec<WorldSlot>,
}

use std::borrow::Borrow;
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::{fmt, ops};

use multiline_logger::log;
use serde::de::{Error, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

static STRING_CACHE: Mutex<BTreeSet<Arc<str>>> = Mutex::new(BTreeSet::new());

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Istr(Arc<str>);

impl Istr {
	pub fn new(text: &str) -> Self {
		let mut cache = STRING_CACHE.lock().unwrap();
		if let Some(text) = cache.get(text) {
			Istr(Arc::clone(text))
		} else {
			let text = Arc::from(text);
			cache.insert(Arc::clone(&text));
			Istr(text)
		}
	}

	pub fn gc() {
		let mut cache = STRING_CACHE.lock().unwrap();
		let len = cache.len();
		// since we have the cache locked, and all cloning is done in Self::new, this should never leak a string
		cache.retain(|s| Arc::strong_count(s) > 1);
		let diff = len - cache.len();
		if diff > 0 {
			log::info!("GC collected {diff} unused string(s)");
		}
	}
}

impl ops::Deref for Istr {
	type Target = str;

	fn deref(&self) -> &Self::Target { &self.0 }
}

#[derive(Default)]
struct IstrVisitor;

impl Visitor<'_> for IstrVisitor {
	type Value = Istr;

	fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
		formatter.write_str("a &str")
	}

	fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
	where
		E: Error,
	{
		Ok(Istr::new(s))
	}
}

impl<'de> Deserialize<'de> for Istr {
	fn deserialize<D>(deserializer: D) -> Result<Istr, D::Error>
	where
		D: Deserializer<'de>,
	{
		deserializer.deserialize_str(IstrVisitor)
	}
}

impl Serialize for Istr {
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: Serializer,
	{
		serializer.serialize_str(self)
	}
}

impl AsRef<str> for Istr {
	fn as_ref(&self) -> &str { self }
}

impl Borrow<str> for Istr {
	fn borrow(&self) -> &str { self }
}

impl fmt::Debug for Istr {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result { fmt::Debug::fmt(&**self, f) }
}

impl fmt::Display for Istr {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result { fmt::Display::fmt(&**self, f) }
}

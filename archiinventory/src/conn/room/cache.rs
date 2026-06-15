// https://github.com/nex3/archipelago_rs/blob/fc91912a4ffd938f81d3fb095aa4bc93f1b3d48c/src/cache.rs
// https://github.com/nex3/archipelago_rs/blob/fc91912a4ffd938f81d3fb095aa4bc93f1b3d48c/src/util.rs
#![expect(clippy::unnecessary_debug_formatting, clippy::panic_in_result_fn, reason = "not mine")]

use std::path::{Path, PathBuf};
use std::{env, fs, io};

use hashbrown::HashMap;
use multiline_logger::log;

use crate::conn::room::GameData;
use crate::istr::Istr;

const SANITIZE_FILE_NAME_OPTIONS: sanitise_file_name::Options<Option<char>> =
	sanitise_file_name::Options {
		normalise_whitespace: false,
		replace_with: None,
		..sanitise_file_name::Options::DEFAULT
	};

fn write_file_atomic(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> io::Result<()> {
	let path = path.as_ref();
	let mut tmp_path = PathBuf::from(path);
	tmp_path.pop();

	let mut tmp_basename = path
		.file_stem()
		.unwrap_or_else(|| panic!("write_file_atomic path must have a basename, was {path:?}"))
		.to_owned();
	tmp_basename.push(format!("-tmp-{:0}", rand::random::<u32>()));
	if let Some(ext) = path.extension() {
		tmp_basename.push(ext);
	}
	tmp_path.push(tmp_basename);
	fs::write(&tmp_path, contents)?;
	fs::rename(tmp_path, path)?;

	Ok(())
}

fn sanitize_file_name(name: impl AsRef<str>) -> String {
	sanitise_file_name::sanitise_with_options(name.as_ref(), &SANITIZE_FILE_NAME_OPTIONS)
}

pub fn cache_path() -> PathBuf {
	platform_cache_dir()
		.unwrap_or_else(|| {
			env::current_dir()
				.expect("failed to determine current working directory")
				.join("Archipelago")
				.join("Cache")
		})
		.join("datapackage")
}

fn platform_cache_dir() -> Option<PathBuf> {
	#[cfg(target_os = "windows")]
	{
		env::var_os("LOCALAPPDATA").map(PathBuf::from).map(|p| p.join("Archipelago").join("Cache"))
	}

	#[cfg(target_os = "macos")]
	{
		env::var_os("HOME")
			.map(PathBuf::from)
			.map(|h| h.join("Library").join("Caches").join("Archipelago").join("Cache"))
	}

	#[cfg(target_os = "linux")]
	{
		use std::env;

		env::var_os("XDG_CACHE_HOME")
			.map(PathBuf::from)
			.or_else(|| env::home_dir().map(|h| h.join(".cache")))
			.map(|p| p.join("Archipelago").join("Cache"))
	}
}
pub fn load_data_package(
	dir: &Path,
	game: &Istr,
	checksum: &str,
	data_packages: &mut HashMap<Istr, GameData>,
) -> bool {
	let path =
		dir.join(sanitize_file_name(game)).join(sanitize_file_name(format!("{checksum}.json")));

	let file = match fs::read_to_string(&path) {
		Ok(f) => f,
		Err(err) => {
			log::error!("Missing or unreadable cache for {game}: {err}");
			return false;
		},
	};

	match serde_json::from_str::<GameData>(&file) {
		// Double-check that the checksum is accurate
		Ok(data) if data.checksum.eq(checksum) => {
			data_packages.insert(game.clone(), data);
			return true;
		},
		Ok(_) => {},
		Err(err) => {
			log::error!("Failed to deserialize cached data package for {game}: {err}");
		},
	}
	false
}

pub fn store_data_package(dir: &Path, game: &str, data: &GameData) {
	let game_dir = dir.join(sanitize_file_name(game));

	if let Err(err) = fs::create_dir_all(&game_dir) {
		log::error!("Failed to create cache directory {game_dir:?}: {err}");
		// If one directory fails to create, chances are the others will
		// as well.
		return;
	}

	let serialized = match serde_json::to_string(&data) {
		Ok(r) => r,
		Err(err) => {
			log::error!("Failed to serialize data package for {game}: {err}");
			return;
		},
	};

	let path = game_dir.join(sanitize_file_name(format!("{}.json", data.checksum)));
	if let Err(err) = write_file_atomic(&path, serialized) {
		log::error!("Failed to write cached data package to {path:?}: {err}");
	}
}

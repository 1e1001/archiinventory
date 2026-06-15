// using the webhost api to be a lot faster

use std::sync::mpsc::Sender;

use eframe::egui;
use hashbrown::{HashMap, HashSet};
use multiline_logger::log;
use serde::{Deserialize, Serialize};
use ureq::Agent;
use ureq::tls::{TlsConfig, TlsProvider};
use url::Url;

use crate::conn::ConnEvent;
use crate::data::{SlotItemGroup, SlotItemInstance, WorldFile};
use crate::istr::Istr;

mod cache;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GameData {
	item_name_to_id: HashMap<Istr, i64>,
	location_name_to_id: HashMap<Istr, i64>,
	checksum: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[repr(transparent)]
struct PartialGameData {
	checksum: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RoomStatus {
	// TODO: room connection should return the socket as a copyable field
	last_port: u16,
	tracker: String,
	players: Vec<(Istr, Istr)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StaticTracker {
	datapackage: HashMap<Istr, PartialGameData>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PlayerItemsReceived {
	player: u32,
	/// item, location, player, flags
	items: Vec<(i64, i64, u32, u8)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Tracker {
	player_items_received: Vec<PlayerItemsReceived>,
}

// TODO: cache some results in memory? refreshes aren't too frequent so i don't think it's that needed

fn connect_inner<F: Fn() -> bool>(
	world: WorldFile,
	tx: &Sender<ConnEvent>,
	ctx: egui::Context,
	cancelled: F,
) -> anyhow::Result<()> {
	// since threads can't be killed, we have to manually check for cancellation
	// tx.send also count as a cancel point
	macro_rules! cancel_point {
		() => {
			if cancelled() {
				log::info!("thread cancel received");
				return Ok(());
			}
		};
	}
	let mut emit = |event| -> anyhow::Result<()> {
		tx.send(event)?;
		ctx.request_repaint();
		Ok(())
	};

	let parsed = Url::parse(&world.address)?;
	let scheme = parsed.scheme();
	if scheme != "http" && scheme != "https" {
		anyhow::bail!("bad address scheme {scheme:?}");
	}
	let host = parsed.authority();
	if host.is_empty() {
		anyhow::bail!("address has no host");
	}
	let room_id = parsed
		.path_segments()
		.and_then(|mut iter| {
			// trim trailing slashes
			loop {
				break match iter.next_back() {
					Some("") => continue,
					next => next,
				};
			}
		})
		.ok_or_else(|| anyhow::anyhow!("address has no path"))?;
	log::info!("Room connection:\nscheme = {scheme:?}\nhost = {host:?}\nroom id = {room_id:?}");

	let agent = Agent::from(
		Agent::config_builder()
			.tls_config(TlsConfig::builder().provider(TlsProvider::NativeTls).build())
			.build(),
	);

	emit(ConnEvent::Status("Fetching room status".into()))?;
	let room_status = agent
		.get(format!("{scheme}://{host}/api/room_status/{room_id}"))
		.call()?
		.body_mut()
		.read_json::<RoomStatus>()?;
	let tracker_id = room_status.tracker;

	emit(ConnEvent::Status("Fetching data package list".into()))?;
	let static_tracker = agent
		.get(format!("{scheme}://{host}/api/static_tracker/{tracker_id}"))
		.call()?
		.body_mut()
		.read_json::<StaticTracker>()?;

	cancel_point!();

	let cache_path = cache::cache_path();
	log::debug!("cache path is {cache_path:?}");
	let data_package_len = static_tracker.datapackage.len();
	let mut data_packages = HashMap::with_capacity(data_package_len);
	let mut new_data_packages = Vec::new();

	for (i, (game, data)) in static_tracker.datapackage.into_iter().enumerate() {
		emit(ConnEvent::Status(format!("Loading cached data packages {i}/{data_package_len}")))?;
		if !cache::load_data_package(&cache_path, &game, &data.checksum, &mut data_packages) {
			new_data_packages.push((game.clone(), data.checksum));
		}
	}

	if !new_data_packages.is_empty() {
		for (i, (game, checksum)) in new_data_packages.iter().enumerate() {
			if !data_packages.contains_key(&**game) {
				emit(ConnEvent::Status(format!(
					"Downloading data package {i}/{}: {game}",
					new_data_packages.len()
				)))?;
				let datapackage = agent
					.get(format!("{scheme}://{host}/api/datapackage/{checksum}"))
					.call()?
					.body_mut()
					.read_json::<GameData>()?;
				data_packages.insert(game.clone(), datapackage);
			}
			cancel_point!();
			cache::store_data_package(&cache_path, game, &data_packages[game]);
		}
	}

	emit(ConnEvent::Status("Building lookup tables".into()))?;

	// item names, location names
	let mut id_tables = HashMap::with_capacity(data_packages.len());

	for (game, data) in data_packages {
		let mut item_names = HashMap::with_capacity(data.item_name_to_id.len());
		for (name, id) in data.item_name_to_id {
			item_names.insert(id, name);
		}
		let mut location_names = HashMap::with_capacity(data.location_name_to_id.len());
		for (name, id) in data.location_name_to_id {
			location_names.insert(id, name);
		}
		id_tables.insert(game, (item_names, location_names));
	}

	emit(ConnEvent::Status("Fetching tracker data".into()))?;
	let tracker = agent
		.get(format!("{scheme}://{host}/api/tracker/{tracker_id}"))
		.call()?
		.body_mut()
		.read_json::<Tracker>()?;

	emit(ConnEvent::Status("Resolving items".into()))?;
	let mut search_slots = world
		.slots
		.into_iter()
		.filter_map(|slot| slot.active.then_some(slot.name))
		.collect::<HashSet<_>>();

	let mut players_list = room_status.players;
	players_list.insert(0, (Istr::new("Server"), Istr::new("Archipelago")));

	for PlayerItemsReceived { player, items } in tracker.player_items_received {
		// room status list doesn't include server player
		let (player_name, player_game) = &players_list[player as usize];
		if search_slots.remove(&**player_name) {
			log::debug!("{player_name} ({player}) received {} item(s)", items.len());
			let mut item_groups = HashMap::new();
			let (item_table, _) = &id_tables[player_game];
			for (index, &(item, location, sender, flags)) in items.iter().enumerate() {
				let (sender_name, sender_game) = &players_list[sender as usize];
				let (_, location_table) = &id_tables[sender_game];
				item_groups
					.entry(item)
					.or_insert_with(|| SlotItemGroup {
						id: item,
						name: item_table[&item].clone(),
						instances: Vec::new(),
					})
					.instances
					.push(SlotItemInstance {
						index,
						from_id: sender,
						from_name: sender_name.clone(),
						at_id: location,
						at_name: location_table[&location].clone(),
						at_game: sender_game.clone(),
						type_flags: [flags & 1 != 0, flags & 2 != 0, flags & 4 != 0],
					});
			}
			emit(ConnEvent::SlotData(
				String::from(&**player_name),
				items.len(),
				item_groups.into_values().collect(),
			))?;
		}
	}

	if !search_slots.is_empty() {
		anyhow::bail!("Some slots were not found: {search_slots:?}");
	}

	let mut new_address = parsed;
	// always show a port please
	_ = new_address.set_scheme(if room_status.last_port == 80 { "wss" } else { "ws" });
	_ = new_address.set_port(Some(room_status.last_port));
	emit(ConnEvent::Done(Some(new_address.authority().into())))?;
	Ok(())
}

pub fn connect<F: Fn() -> bool>(
	world: WorldFile,
	// TODO: package these three into one thing
	tx: &Sender<ConnEvent>,
	ctx: egui::Context,
	cancelled: F,
) {
	if let Err(err) = connect_inner(world, tx, ctx.clone(), cancelled) {
		_ = tx.send(ConnEvent::Error(err.to_string()));
		ctx.request_repaint();
	}
}

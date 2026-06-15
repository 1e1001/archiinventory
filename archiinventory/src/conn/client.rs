// archipelago_rs uses a leaky string interning library so it's contained in its own process
use std::env::args;
use std::io::{Write, stdout};
use std::mem::discriminant;
use std::process::exit;

use archipelago_rs as ap;
use archipelago_rs::tags::{NO_TEXT, TRACKER};
use hashbrown::HashMap;
use multiline_logger::log;

use crate::conn::ConnEvent;
use crate::data::{SlotItemGroup, SlotItemInstance};
use crate::istr::Istr;
pub struct ClientMain {
	address: String,
	password: String,
	slots: Vec<String>,
}

fn emit(this: &ConnEvent) -> anyhow::Result<()> {
	let mut stdout = stdout().lock();
	serde_json::to_writer(&mut stdout, this)?;
	stdout.write_all(b"\n")?;
	Ok(())
}

impl ClientMain {
	pub fn from_args() -> Option<Self> {
		let mut iter = args();
		iter.next();
		(iter.next().as_deref() == Some("--client")).then(|| {
			let address = iter.next().unwrap();
			let password = iter.next().unwrap();
			let slots = iter.collect();
			Self { address, password, slots }
		})
	}

	fn run_inner(self) -> anyhow::Result<()> {
		let Self { address, password, slots } = self;
		let slots_len = slots.len();
		for (i, slot) in slots.into_iter().enumerate() {
			emit(&ConnEvent::Status(format!("{}/{slots_len}: {slot}", i + 1)))?;
			let mut client = smol::block_on(ap::Client::<()>::connect(
				&address,
				&slot,
				None::<&str>,
				ap::ConnectionOptions::new()
					.password(&password)
					.tags([TRACKER, NO_TEXT, "archiinventory"])
					.receive_items(ap::ItemHandling::OtherWorlds {
						own_world: true,
						starting_inventory: true,
					}),
			))?;
			//for game in world.banished_games.as_deref().unwrap_or_default() {
			//	// other people's worlds can have broken datapackages, a quick solution for that.
			//	client.banish_datapackage(game);
			//}
			// AP protocol documentation doesn't mention some fun behavior of RecievedItems that we exploit.
			// if the slot has at least one item, the server always sends a ReceivedItems(0) in the same message as the connection
			// if the slot has no items, the server will never send ReceivedItems.
			loop {
				match client.try_next_event() {
					None => {
						log::trace!("None items");
						break;
					},
					Some(ap::Event::Connected) => {
						log::trace!("Connected");
					},
					Some(ap::Event::ReceivedItems(0)) => {
						log::trace!("Received");
						break;
					},
					Some(ap::Event::Error(err)) => return Err(err.into()),
					// TODO: event should impl Debug
					Some(event) => log::trace!("skipping event {:?}", discriminant(&event)),
				}
			}
			log::debug!("{} items", client.received_items().len());
			let mut item_groups = HashMap::new();
			for (index, item) in client.received_items().iter().enumerate() {
				item_groups
					.entry(item.item().id())
					.or_insert_with(|| SlotItemGroup {
						id: item.item().id(),
						name: Istr::new(&item.item().name()),
						instances: Vec::new(),
					})
					.instances
					.push(SlotItemInstance {
						index,
						from_id: item.sender().slot(),
						from_name: Istr::new(&item.sender().name()),
						at_id: item.location().id(),
						at_name: Istr::new(&item.location().name()),
						at_game: Istr::new(&item.location().game()),
						type_flags: [item.is_progression(), item.is_useful(), item.is_trap()],
					});
			}
			emit(&ConnEvent::SlotData(
				slot,
				client.received_items().len(),
				item_groups.into_values().collect(),
			))?;
			drop(client);
			log::debug!("disconnected??");
		}
		log::info!("all done");
		emit(&ConnEvent::Done(None))?;
		Ok(())
	}

	pub fn run(self) -> ! {
		if let Err(err) = self.run_inner() {
			emit(&ConnEvent::Error(err.to_string())).unwrap();
		}
		exit(0);
	}
}

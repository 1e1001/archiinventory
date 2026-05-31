use std::mem::discriminant;
use std::time::SystemTime;

use ap::tags::{NO_TEXT, TRACKER};
use archipelago_rs as ap;
use eframe::egui;
use multiline_logger::log;
use smol::Task;
use smol::channel::{self, Receiver, Sender, TryRecvError};

use crate::data::{MergeSlots, World, WorldItem, WorldSlot};

// TODO: long-running connection??
// or maybe a "sync mode" that does that but doesn't save the data?

#[derive(Debug)]
enum ConnectionEvent {
	Status(String),
	Done(anyhow::Result<MergeSlots>),
}

async fn run(
	world: World,
	tx: Sender<ConnectionEvent>,
	ctx: &egui::Context,
) -> anyhow::Result<MergeSlots> {
	let status = async |text| {
		log::debug!("{text}");
		tx.send(ConnectionEvent::Status(text)).await.unwrap();
		ctx.request_repaint();
	};
	let time = SystemTime::now()
		.duration_since(SystemTime::UNIX_EPOCH)
		.unwrap_or_default()
		.as_secs();
	log::debug!("Time is {time}");
	let slot_count = world.slots.len();
	let mut result = Vec::with_capacity(slot_count);
	for (i, slot) in world.slots.into_iter().enumerate() {
		status(format!("Slot {}/{slot_count}: {}", i + 1, slot.name)).await;
		let mut client = ap::Client::<()>::connect(
			&world.address,
			&slot.name,
			None::<&str>,
			ap::ConnectionOptions::new()
				.password(&world.password)
				.tags([TRACKER, NO_TEXT, "archiinventory"])
				.receive_items(ap::ItemHandling::OtherWorlds {
					own_world: true,
					starting_inventory: true,
				}),
		)
		.await?;
		for game in world.banished_games.as_deref().unwrap_or_default() {
			// other people's worlds can have broken datapackages, a quick solution for that.
			client.banish_datapackage(game);
		}
		// AP protocol documentation doesn't mention some fun behavior of RecievedItems that we exploit.
		// if the slot has at least one item, the server always sends a ReceivedItems(0) in the same message as the connection
		// if the slot has no items, the server will never send ReceivedItems.

		//client.sync()?;
		loop {
			match client.try_next_event() {
				None => {
					log::trace!("None items");
					break;
				}
				Some(ap::Event::Connected) => {
					log::trace!("Connected");
				}
				Some(ap::Event::ReceivedItems(0)) => {
					log::trace!("Received");
					break;
				}
				Some(ap::Event::Error(err)) => return Err(err.into()),
				// TODO: event should impl Debug
				Some(event) => log::trace!("skipping event {:?}", discriminant(&event)),
			}
		}
		log::debug!("items: {:#?}", client.received_items());
		result.push(WorldSlot {
			name: slot.name,
			id: slot.id,
			ui_data_stale: false,
			confirmed: slot.confirmed,
			game: client.this_game().name(),
			inventory: client
				.received_items()
				.iter()
				.map(|item| WorldItem {
					item_id: item.item().id(),
					item_name: item.item().name(),
					sender_id: item.sender().slot(),
					sender_name: item.sender().name(),
					loc_id: item.location().id(),
					loc_name: item.location().name(),
					loc_game: item.location().game(),
					progression: item.is_progression(),
					useful: item.is_useful(),
					trap: item.is_trap(),
					time,
				})
				.collect(),
		});
		drop(client);
		log::debug!("disconnected??");
	}
	Ok(result)
}

pub struct Connection {
	rx: Receiver<ConnectionEvent>,
	task: Option<Task<()>>,
	status: (bool, String),
}

impl Connection {
	pub fn new(world: &World, ctx: &egui::Context) -> Self {
		let (tx, rx) = channel::unbounded();
		let world = world.clone_for_conn();
		let ctx = ctx.clone();
		let task = smol::spawn(async move {
			log::debug!("Connection thread");
			tx.clone()
				.send(ConnectionEvent::Done(run(world, tx, &ctx).await))
				.await
				.unwrap();
			ctx.request_repaint();
		});

		Self {
			rx,
			task: Some(task),
			status: (false, "Starting up…".to_owned()),
		}
	}
	pub fn is_running(&self) -> bool { self.task.is_some() }
	pub fn cancel(&mut self) {
		log::info!("Cancelling");
		if let Some(task) = self.task.take()
			&& smol::block_on(task.cancel()).is_none()
		{
			self.status = (true, "Cancelled".to_owned());
		}
	}
	pub fn ui_update(&mut self, ui: &mut egui::Ui) -> Option<MergeSlots> {
		let mut out = None;
		loop {
			match self
				.rx
				.try_recv()
				//.inspect(|event| log::trace!("recv {event:?}"))
			{
				Ok(ConnectionEvent::Status(status)) => self.status = (false, status),
				Ok(ConnectionEvent::Done(done)) => {
					self.status = match done {
						Ok(items) => {
							out = Some(items);
							(false, "Done!".to_owned())
						}
						Err(err) => (true, err.to_string()),
					};
					// task will be complete by this point
					self.task.take().map(Task::cancel);
				}
				Err(TryRecvError::Closed) if self.task.is_some() => {
					self.status = (true, "Task channel closed!".to_owned());
					break;
				}
				Err(_) => break,
			}
		}
		let text = egui::RichText::new(&self.status.1);
		ui.add(
			egui::Label::new(if self.status.0 {
				text.color(egui::Color32::ORANGE)
			} else {
				text
			})
			.wrap_mode(egui::TextWrapMode::Truncate),
		);
		out
	}
}

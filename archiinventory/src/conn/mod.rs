mod client;
use std::env::current_exe;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::thread::{self, JoinHandle};

pub use client::ClientMain;
use eframe::egui;
use multiline_logger::log;
use serde::{Deserialize, Serialize};

use crate::data::{SlotItemGroup, WorldConnType, WorldFile};
mod room;

#[derive(Debug, Serialize, Deserialize)]
enum ConnEvent {
	Error(String),
	Done(Option<String>),
	Status(String),
	SlotData(String, usize, Vec<SlotItemGroup>),
}

enum ConnState {
	Error(String),
	Done(Option<String>),
	Client {
		rx: Receiver<ConnEvent>,
		child: Child,
		thread: Option<JoinHandle<()>>,
		status: String,
	},
	Room {
		rx: Receiver<ConnEvent>,
		signal: Arc<AtomicBool>,
		thread: Option<JoinHandle<()>>,
		status: String,
	},
}

pub struct Connection(ConnState);

impl Connection {
	fn try_new(world: WorldFile, ctx: egui::Context) -> anyhow::Result<ConnState> {
		let (tx, rx) = channel();
		//let thread = thread::Builder::new().name("connection".into());
		Ok(match world.conn_type {
			WorldConnType::Client => {
				let mut child = Command::new(current_exe()?)
					.args(["--client", &world.address, &world.password])
					.args(world.slots.iter().filter_map(|slot| slot.active.then_some(&slot.name)))
					.stdout(Stdio::piped())
					.spawn()?;
				let stdout = BufReader::new(
					child
						.stdout
						.take()
						.ok_or_else(|| anyhow::anyhow!("Process spawned without stdout"))?,
				);
				let thread =
					thread::Builder::new().name("stdout pipe".into()).spawn(move || {
						for line in stdout.lines() {
							let event = match line {
								Ok(line) => serde_json::from_str(&line)
									.unwrap_or_else(|err| ConnEvent::Error(err.to_string())),
								Err(err) => ConnEvent::Error(err.to_string()),
							};
							let is_error = matches!(event, ConnEvent::Error(_));
							if tx.send(event).is_err() {
								log::info!("parent closed");
								break;
							}
							ctx.request_repaint();
							if is_error {
								log::info!("quitting after error");
								break;
							}
						}
					})?;
				ConnState::Client {
					rx,
					child,
					thread: Some(thread),
					status: "Starting connection…".into(),
				}
			},
			WorldConnType::Room => {
				let signal = Arc::new(AtomicBool::new(false));
				let inner_signal = Arc::clone(&signal);
				let thread =
					thread::Builder::new().name("room connect".into()).spawn(move || {
						room::connect(world, &tx, ctx, || inner_signal.load(Ordering::Relaxed));
					})?;
				ConnState::Room {
					rx,
					signal,
					thread: Some(thread),
					status: "Starting connection…".into(),
				}
			},
		})
	}

	pub fn new(world: WorldFile, ctx: egui::Context) -> Self {
		Self(Self::try_new(world, ctx).unwrap_or_else(|err| ConnState::Error(err.to_string())))
	}

	pub fn active(&self) -> bool {
		matches!(self.0, ConnState::Client { .. } | ConnState::Room { .. })
	}

	pub fn ui<F: FnMut(String, usize, Vec<SlotItemGroup>)>(
		&mut self,
		ui: &mut egui::Ui,
		mut on_complete: F,
	) {
		match &mut self.0 {
			ConnState::Error(text) => {
				ui.colored_label(egui::Color32::ORANGE, text);
			},
			ConnState::Done(addr) => {
				if let Some(addr) = addr {
					ui.label("Done, client address:");
					ui.horizontal(|ui| {
						if ui.button("Copy").clicked() {
							ui.copy_text(addr.clone());
						}
						ui.label(&*addr);
					});
				} else {
					ui.label("Done");
				}
			},
			ConnState::Client { rx, thread, status, .. }
			| ConnState::Room { rx, thread, status, .. } => {
				if thread.as_ref().is_some_and(JoinHandle::is_finished)
					&& let Err(err) = thread.take().unwrap().join()
				{
					self.0 = ConnState::Error(if let Some(&s) = err.downcast_ref::<&str>() {
						s.to_owned()
					} else if let Some(s) = err.downcast_ref::<String>() {
						s.to_owned()
					} else {
						"thread panic with non-string message".to_owned()
					});
					// re-run ui to display message
					self.ui(ui, on_complete);
					return;
				}
				while let Ok(event) = rx.try_recv() {
					match event {
						ConnEvent::Error(text) => {
							self.0 = ConnState::Error(text);
							// re-run ui to display message
							self.ui(ui, on_complete);
							return;
						},
						ConnEvent::Done(addr) => {
							self.0 = ConnState::Done(addr);
							// re-run ui to display message
							self.ui(ui, on_complete);
							return;
						},
						ConnEvent::Status(text) => *status = text,
						ConnEvent::SlotData(name, total_items, inventory) => {
							on_complete(name, total_items, inventory)
						},
					}
				}
				ui.label(&*status);
			},
		}
	}
}

impl Drop for ConnState {
	fn drop(&mut self) {
		match self {
			ConnState::Error(_) | ConnState::Done(_) => {},
			ConnState::Client { child, .. } => {
				// TODO: ensure this immediately wakes the thread
				_ = child.kill();
			},
			ConnState::Room { signal, .. } => signal.store(true, Ordering::Relaxed),
		}
	}
}

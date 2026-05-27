use archipelago_rs as ap;
use std::sync::mpsc::{channel, Sender, Receiver};

use crate::world::{World, InstanceLocalId};

enum ConnectionEvent {
}

fn run(world: World, tx: Sender<ConnectionEvent>) {

}

pub struct Connection {
	rx: Receiver<ConnectionEvent>,
}

impl Connection {
	pub fn new(world: &World) -> Self {
		//let (tx, rx) = channel();
		todo!("connection")
	}
}
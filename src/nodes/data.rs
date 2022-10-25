use super::alias::AliasInfo;
use super::fields::Field;
use super::spatial::{parse_transform, Spatial};
use super::{Alias, Node};
use crate::core::client::Client;
use crate::core::node_collections::LifeLinkedNodeMap;
use crate::core::registry::Registry;
use crate::nodes::fields::find_field;
use crate::nodes::spatial::find_spatial_parent;
use anyhow::{anyhow, ensure, Result};
use glam::vec3a;
use mint::{Quaternion, Vector3};
use nanoid::nanoid;
use serde::{Deserialize, Serialize};
use stardust_xr::schemas::flex::{deserialize, serialize};
use stardust_xr::values::Transform;
use std::sync::{Arc, Weak};

static PULSE_SENDER_REGISTRY: Registry<PulseSender> = Registry::new();
static PULSE_RECEIVER_REGISTRY: Registry<PulseReceiver> = Registry::new();

fn mask_matches(mask_map_lesser: &Mask, mask_map_greater: &Mask) -> bool {
	(|| -> Result<_> {
		for key in mask_map_lesser.get_mask()?.iter_keys() {
			let lesser_key_type = mask_map_lesser.get_mask()?.index(key)?.flexbuffer_type();
			let greater_key_type = mask_map_greater.get_mask()?.index(key)?.flexbuffer_type();
			if lesser_key_type != greater_key_type {
				return Err(flexbuffers::ReaderError::InvalidPackedType {}.into());
			}
		}
		Ok(())
	})()
	.is_ok()
}

type MaskMapGetFn = fn(&[u8]) -> Result<flexbuffers::MapReader<&[u8]>>;
pub struct Mask {
	binary: Vec<u8>,
	get_fn: MaskMapGetFn,
}
impl Mask {
	pub fn get_mask(&self) -> Result<flexbuffers::MapReader<&[u8]>> {
		(self.get_fn)(self.binary.as_slice())
	}
	// pub fn set_mask(&mut self, binary: Vec<u8>, get_fn: MaskMapGetFn) {
	// 	self.binary = binary;
	// 	self.get_fn = get_fn;
	// }
}
// impl Default for Mask {
// 	fn default() -> Self {
// 		Mask {
// 			binary: Default::default(),
// 			get_fn: mask_get_err,
// 		}
// 	}
// }
// fn mask_get_err(_binary: &[u8]) -> Result<flexbuffers::MapReader<&[u8]>> {
// 	Err(anyhow!("You need to call setMask to set the mask!"))
// }
fn mask_get_map_at_root(binary: &[u8]) -> Result<flexbuffers::MapReader<&[u8]>> {
	flexbuffers::Reader::get_root(binary)
		.map_err(|_| anyhow!("Mask is not a valid flexbuffer"))?
		.get_map()
		.map_err(|_| anyhow!("Mask is not a valid map"))
}
// pub fn mask_get_map_send_data(binary: &[u8]) -> Result<flexbuffers::MapReader<&[u8]>> {
// 	flexbuffers::Reader::get_root(binary)
// 		.map_err(|_| anyhow!("Mask is not a valid flexbuffer"))?
// 		.get_vector()?
// 		.index(1)?
// 		.get_map()
// 		.map_err(|_| anyhow!("Mask is not a valid map"))
// }

pub struct PulseSender {
	uid: String,
	node: Weak<Node>,
	mask: Mask,
	aliases: LifeLinkedNodeMap<String>,
}
impl PulseSender {
	fn add_to(node: &Arc<Node>, mask: Mask) -> Result<()> {
		ensure!(
			node.spatial.get().is_some(),
			"Internal: Node does not have a spatial attached!"
		);

		let sender = PulseSender {
			uid: nanoid!(),
			node: Arc::downgrade(node),
			mask,
			aliases: LifeLinkedNodeMap::default(),
		};
		let sender = PULSE_SENDER_REGISTRY.add(sender);
		let _ = node.pulse_sender.set(sender);
		node.add_local_signal("sendData", PulseSender::send_data_flex);
		let sender = node.pulse_sender.get().unwrap();
		for receiver in PULSE_RECEIVER_REGISTRY.get_valid_contents() {
			sender.handle_new_receiver(&receiver);
		}
		Ok(())
	}
	fn handle_new_receiver(&self, receiver: &PulseReceiver) {
		if !mask_matches(&self.mask, &receiver.mask) {
			return;
		}
		let tx_node = match self.node.upgrade() {
			Some(v) => v,
			_ => return,
		};
		let tx_client = match tx_node.get_client() {
			Some(v) => v,
			_ => return,
		};
		let rx_node = match receiver.node.upgrade() {
			Some(v) => v,
			_ => return,
		};
		// Receiver itself
		let rx_alias = Alias::create(
			&tx_client,
			tx_node.get_path(),
			receiver.uid.as_str(),
			&rx_node,
			AliasInfo {
				local_methods: vec!["sendData", "getTransform"],
				..Default::default()
			},
		);
		self.aliases.add(receiver.uid.clone(), &rx_alias);

		if let Some(rx_field_node) = receiver.field.spatial_ref().node.upgrade() {
			// Receiver's field
			let rx_field_alias = Alias::create(
				&tx_client,
				rx_alias.get_path(),
				"field",
				&rx_field_node,
				AliasInfo {
					local_methods: vec!["sendData", "getTransform"],
					..Default::default()
				},
			);
			self.aliases
				.add(receiver.uid.clone() + "-field", &rx_field_alias);
		}

		#[derive(Serialize)]
		struct NewReceiverInfo<'a> {
			uid: &'a str,
			distance: f32,
			position: Vector3<f32>,
			rotation: Quaternion<f32>,
		}

		let (_, rotation, position) = Spatial::space_to_space_matrix(
			rx_node.spatial.get().map(|s| &**s),
			tx_node.spatial.get().map(|s| &**s),
		)
		.to_scale_rotation_translation();

		let info = NewReceiverInfo {
			uid: &receiver.uid,
			distance: receiver
				.field
				.distance(tx_node.spatial.get().unwrap(), vec3a(0.0, 0.0, 0.0)),
			position: position.into(),
			rotation: rotation.into(),
		};

		let _ = tx_node.send_remote_signal("newReceiver", &serialize(info).unwrap());
	}

	fn handle_drop_receiver(&self, uid: String) {
		if let Some(tx_node) = self.node.upgrade() {
			let _ = tx_node.send_remote_signal("dropReceiver", &serialize(&uid).unwrap());
		}
		self.aliases.remove(&uid);
		self.aliases.remove(&(uid + "-field"));
	}

	fn send_data_flex(node: &Node, calling_client: Arc<Client>, data: &[u8]) -> Result<()> {
		#[derive(Serialize, Deserialize)]
		struct SendDataInfo<'a> {
			uid: &'a str,
			data: Vec<u8>,
		}
		let info: SendDataInfo = deserialize(data)?;
		let capture_path = node.path.clone() + "/" + info.uid;
		let receiver_node = calling_client.get_node("Pulse receiver", &capture_path)?;
		let receiver_mask = &receiver_node
			.get_aspect("Pulse receiver", "pulse receiver", |node| {
				&node.pulse_receiver
			})?
			.mask;
		let data_mask = Mask {
			binary: info.data,
			get_fn: mask_get_map_at_root,
		};
		data_mask.get_mask()?;
		ensure!(
			mask_matches(receiver_mask, &data_mask),
			"Message does not contain the same keys as the receiver's mask"
		);
		receiver_node.send_remote_signal(
			"data",
			&serialize(SendDataInfo {
				uid: &node.pulse_sender.get().unwrap().uid,
				data: data_mask.binary,
			})?,
		)
	}
}
impl Drop for PulseSender {
	fn drop(&mut self) {
		PULSE_SENDER_REGISTRY.remove(self);
	}
}

pub struct PulseReceiver {
	uid: String,
	node: Weak<Node>,
	field: Arc<Field>,
	mask: Mask,
}
impl PulseReceiver {
	pub fn add_to(node: &Arc<Node>, field: Arc<Field>, mask: Mask) -> Result<()> {
		ensure!(
			node.spatial.get().is_some(),
			"Internal: Node does not have a spatial attached!"
		);

		let receiver = PulseReceiver {
			uid: nanoid!(),
			node: Arc::downgrade(node),
			field,
			mask,
		};
		let receiver = PULSE_RECEIVER_REGISTRY.add(receiver);

		for sender in PULSE_SENDER_REGISTRY.get_valid_contents() {
			sender.handle_new_receiver(&receiver);
		}
		let _ = node.pulse_receiver.set(receiver);
		Ok(())
	}
}

impl Drop for PulseReceiver {
	fn drop(&mut self) {
		PULSE_RECEIVER_REGISTRY.remove(self);
		for sender in PULSE_SENDER_REGISTRY.get_valid_contents() {
			sender.handle_drop_receiver(self.uid.clone());
		}
	}
}

pub fn create_interface(client: &Arc<Client>) {
	let node = Node::create(client, "", "data", false);
	node.add_local_signal("createPulseSender", create_pulse_sender_flex);
	node.add_local_signal("createPulseReceiver", create_pulse_receiver_flex);
	node.add_to_scenegraph();
}

// pub fn mask_get_map_pulse_sender(binary: &[u8]) -> Result<flexbuffers::MapReader<&[u8]>> {
// 	flexbuffers::Reader::get_root(binary)
// 		.map_err(|_| anyhow!("Mask is not a valid flexbuffer"))?
// 		.get_vector()?
// 		.index(3)?
// 		.get_map()
// 		.map_err(|_| anyhow!("Mask is not a valid map"))
// }
pub fn create_pulse_sender_flex(
	_node: &Node,
	calling_client: Arc<Client>,
	data: &[u8],
) -> Result<()> {
	#[derive(Deserialize)]
	struct CreatePulseSenderInfo<'a> {
		name: &'a str,
		parent_path: &'a str,
		transform: Transform,
		mask: Vec<u8>,
	}
	let info: CreatePulseSenderInfo = deserialize(data)?;
	let node = Node::create(&calling_client, "/data/sender", info.name, true);
	let parent = find_spatial_parent(&calling_client, info.parent_path)?;
	let transform = parse_transform(info.transform, true, true, false)?;

	let mask = Mask {
		binary: info.mask,
		get_fn: mask_get_map_at_root,
	};
	mask.get_mask()?;

	let node = node.add_to_scenegraph();
	Spatial::add_to(&node, Some(parent), transform, false)?;
	PulseSender::add_to(&node, mask)?;
	Ok(())
}

// pub fn mask_get_map_pulse_receiver(binary: &[u8]) -> Result<flexbuffers::MapReader<&[u8]>> {
// 	flexbuffers::Reader::get_root(binary)
// 		.map_err(|_| anyhow!("Mask is not a valid flexbuffer"))?
// 		.get_vector()?
// 		.index(4)?
// 		.get_map()
// 		.map_err(|_| anyhow!("Mask is not a valid map"))
// }
pub fn create_pulse_receiver_flex(
	_node: &Node,
	calling_client: Arc<Client>,
	data: &[u8],
) -> Result<()> {
	#[derive(Deserialize)]
	struct CreatePulseReceiverInfo<'a> {
		name: &'a str,
		parent_path: &'a str,
		transform: Transform,
		field_path: &'a str,
		mask: Vec<u8>,
	}
	let info: CreatePulseReceiverInfo = deserialize(data)?;
	let node = Node::create(&calling_client, "/data/receiver", info.name, true);
	let parent = find_spatial_parent(&calling_client, info.parent_path)?;
	let transform = parse_transform(info.transform, true, true, false)?;
	let field = find_field(&calling_client, info.field_path)?;
	let mask = Mask {
		binary: info.mask,
		get_fn: mask_get_map_at_root,
	};
	mask.get_mask()?;

	let node = node.add_to_scenegraph();
	Spatial::add_to(&node, Some(parent), transform, false)?;
	PulseReceiver::add_to(&node, field, mask)?;
	Ok(())
}

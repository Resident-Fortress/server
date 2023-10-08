use crate::nodes::Node;
use parking_lot::Mutex;
use rustc_hash::FxHashMap;
use std::{
	borrow::Borrow,
	hash::Hash,
	sync::{Arc, Weak},
};

// #[derive(Default)]
// pub struct LifeLinkedNodeList {
// 	nodes: Mutex<Vec<Weak<Node>>>,
// }
// impl LifeLinkedNodeList {
// 	pub fn add(&self, node: Weak<Node>) {
// 		self.nodes.lock().push(node);
// 	}
// 	pub fn clear(&self) {
// 		self.nodes
// 			.lock()
// 			.iter()
// 			.filter_map(|node| node.upgrade())
// 			.for_each(|node| {
// 				node.destroy();
// 			});
// 		self.nodes.lock().clear();
// 	}
// }
// impl Drop for LifeLinkedNodeList {
// 	fn drop(&mut self) {
// 		self.clear();
// 	}
// }

#[derive(Default, Debug)]
pub struct LifeLinkedNodeMap<K: Hash + Eq> {
	nodes: Mutex<FxHashMap<K, Weak<Node>>>,
}
#[allow(dead_code)]
impl<K: Hash + Eq> LifeLinkedNodeMap<K> {
	pub fn add(&self, key: K, node: &Arc<Node>) {
		self.nodes.lock().insert(key, Arc::downgrade(node));
	}
	pub fn get<Q>(&self, key: &Q) -> Option<Arc<Node>>
	where
		Q: ?Sized,
		K: Borrow<Q>,
		Q: Hash + Eq,
	{
		self.nodes.lock().get(key).and_then(|n| n.upgrade())
	}
	pub fn nodes(&self) -> Vec<Arc<Node>> {
		self.nodes
			.lock()
			.values()
			.filter_map(|v| v.upgrade())
			.collect()
	}
	pub fn remove<Q>(&self, key: &Q) -> Option<Arc<Node>>
	where
		Q: ?Sized,
		K: Borrow<Q>,
		Q: Hash + Eq,
	{
		self.nodes.lock().remove(key).and_then(|n| n.upgrade())
	}

	pub fn clear(&self) {
		let mut nodes = self.nodes.lock();
		nodes
			.values()
			.filter_map(|node| node.upgrade())
			.for_each(|node| {
				node.destroy();
			});
		nodes.clear();
	}
}
impl<K: Hash + Eq> Drop for LifeLinkedNodeMap<K> {
	fn drop(&mut self) {
		self.clear();
	}
}

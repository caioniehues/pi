//! `check-dag`: the dependency rules that must hold forever (rust/docs/slices/C0.md), checked on the graph of path
//! dependencies between member crates.

use crate::workspace::{dependency_tables, members, path_target, read_toml};
use anyhow::Result;
use std::collections::{BTreeMap, VecDeque};
use std::path::Path;

/// `(crate, crate it must never reach)`: the layering the port's design depends on.
const FORBIDDEN: &[(&str, &str)] = &[
	("pi-core", "pi-interactive"),
	("pi-sdk", "pi-core"),
	("pi-durable", "pi-core"),
	("pi-fake-provider", "pi-ai"),
];

/// One line per broken rule, with the dependency chain that breaks it; empty when every rule holds.
pub fn check_dag(workspace: &Path) -> Result<Vec<String>> {
	let graph = dependency_graph(workspace)?;
	let mut violations = Vec::new();
	for &(from, to) in FORBIDDEN {
		if !graph.contains_key(from) || !graph.contains_key(to) {
			// A renamed crate must not switch a rule off silently.
			violations.push(format!(
				"  rule {from} -/-> {to} names a crate that is not a workspace member"
			));
		} else if let Some(chain) = find_chain(&graph, from, to) {
			violations.push(format!(
				"  {from} must not depend on {to}, found {}",
				chain.join(" -> ")
			));
		}
	}
	if !violations.is_empty() {
		violations.insert(0, "Crate dependency rules are broken:".into());
	}
	Ok(violations)
}

/// Package name to the member crates it depends on through normal and build dependencies; dev-dependencies do not
/// make a crate's library depend on anything.
fn dependency_graph(workspace: &Path) -> Result<BTreeMap<String, Vec<String>>> {
	let members = members(workspace)?;
	let mut graph = BTreeMap::new();
	for (directory, name) in &members {
		let manifest_path = directory.join("Cargo.toml");
		let manifest = read_toml(&manifest_path)?;
		let mut edges = Vec::new();
		for table in dependency_tables(&manifest)
			.iter()
			.filter(|table| table.section != "dev-dependencies")
		{
			for spec in table.entries.values() {
				let target = spec.get("path").and_then(toml::Value::as_str);
				if let Some(dependency) = target.and_then(|target| path_target(&manifest_path, target, &members)) {
					edges.push(dependency.clone());
				}
			}
		}
		graph.insert(name.clone(), edges);
	}
	Ok(graph)
}

/// The shortest chain of crates from `from` to `to`, both included, if `to` is reachable.
fn find_chain(graph: &BTreeMap<String, Vec<String>>, from: &str, to: &str) -> Option<Vec<String>> {
	let mut reached_from: BTreeMap<&str, &str> = BTreeMap::new();
	let mut queue = VecDeque::from([from]);
	while let Some(current) = queue.pop_front() {
		if current == to {
			let mut chain = vec![to.to_owned()];
			let mut step = to;
			while let Some(&previous) = reached_from.get(step) {
				chain.push(previous.to_owned());
				step = previous;
			}
			chain.reverse();
			return Some(chain);
		}
		for next in graph.get(current).into_iter().flatten() {
			if next != from && !reached_from.contains_key(next.as_str()) {
				reached_from.insert(next, current);
				queue.push_back(next);
			}
		}
	}
	None
}

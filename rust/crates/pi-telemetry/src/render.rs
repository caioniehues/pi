//! Stored spans as a readable forest (ports `packages/telemetry/src/render.ts`).

use crate::context::{SpanAttributes, SpanStatus};
use crate::memory::microseconds;
use crate::store::StoredSpan;
use pi_json::{js_number_to_string, to_js_string};
use std::collections::{HashMap, HashSet};
use std::fmt::Write;

/// Renders spans as a chronological forest, treating absent parents as roots and keeping the last of spans with the
/// same id. Durations are milliseconds to the microsecond. Control characters in ids, names, attribute keys and error
/// messages are escaped (attribute values are JSON). Deep trees render without recursion.
pub fn render_trace(spans: &[StoredSpan]) -> String {
	let unique = deduplicated(spans);
	let mut by_start: Vec<&StoredSpan> = unique.clone();
	// A stable sort: equal starts keep their first-seen order, as `Array.prototype.sort` does.
	by_start.sort_by(|left, right| left.started_ms.total_cmp(&right.started_ms));
	let known: HashSet<&str> = unique.iter().map(|span| span.id.as_str()).collect();
	let mut roots = Vec::new();
	let mut children: HashMap<&str, Vec<&StoredSpan>> = HashMap::new();
	for span in by_start {
		match span.parent_id.as_deref().filter(|parent| known.contains(parent)) {
			Some(parent) => children.entry(parent).or_default().push(span),
			None => roots.push(span),
		}
	}
	let mut lines = Vec::new();
	let mut pending: Vec<(&StoredSpan, usize)> = roots.into_iter().rev().map(|span| (span, 0)).collect();
	while let Some((span, depth)) = pending.pop() {
		render_span(span, depth, &mut lines);
		for child in children.get(span.id.as_str()).into_iter().flatten().rev() {
			pending.push((child, depth + 1));
		}
	}
	lines.join("\n")
}

/// The spans with distinct ids in first-seen order, each id holding its last span (a JS `Map` built from them).
fn deduplicated(spans: &[StoredSpan]) -> Vec<&StoredSpan> {
	let mut positions: HashMap<&str, usize> = HashMap::new();
	let mut unique: Vec<&StoredSpan> = Vec::new();
	for span in spans {
		if let Some(&position) = positions.get(span.id.as_str()) {
			unique[position] = span;
		} else {
			positions.insert(&span.id, unique.len());
			unique.push(span);
		}
	}
	unique
}

fn render_span(span: &StoredSpan, depth: usize, lines: &mut Vec<String>) {
	let indent = "  ".repeat(depth);
	let mut error = String::new();
	if let SpanStatus::Error(details) = &span.status {
		error.push_str(" error");
		if let Some(details) = details {
			let _ = write!(error, " {}: {}", escaped(&details.name), escaped(&details.message));
		}
	}
	lines.push(format!(
		"{indent}{}{} ({}ms){error}",
		escaped(&span.name),
		format_attributes(&span.attributes),
		js_number_to_string(microseconds(span.ended_ms - span.started_ms))
	));
	for event in &span.events {
		lines.push(format!(
			"{indent}  · {}{} (+{}ms)",
			escaped(&event.name),
			format_attributes(&event.attributes),
			js_number_to_string(microseconds(event.time_ms))
		));
	}
	for link in &span.links {
		let attributes = link.attributes.as_ref().map(format_attributes).unwrap_or_default();
		lines.push(format!(
			"{indent}  → link trace {} span {}{attributes}",
			escaped(&link.trace_id),
			escaped(&link.span_id)
		));
	}
}

fn format_attributes(attributes: &SpanAttributes) -> String {
	let mut out = String::new();
	for (key, value) in attributes.iter() {
		let _ = write!(out, " {}={}", escaped(key), to_js_string(&value.to_json()));
	}
	out
}

/// `text` with every C0 and C1 control character and DEL written as a `\uXXXX` escape: a stored id, name or message
/// may come from another process (a carrier), so none reaches a terminal raw.
fn escaped(text: &str) -> String {
	let mut out = String::with_capacity(text.len());
	for character in text.chars() {
		if matches!(character, '\u{0}'..='\u{1f}' | '\u{7f}'..='\u{9f}') {
			let _ = write!(out, "\\u{:04x}", u32::from(character));
		} else {
			out.push(character);
		}
	}
	out
}

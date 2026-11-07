//! Session file lines (`session_entry.rs`): the two TS fixtures, every entry kind, the header, unknown kinds and
//! tree nodes.

mod common;

use common::{USAGE, assistant, round_trip, write};
use pi_json::{JsonObject, JsonValue, to_js_string};
use pi_types::{FileEntry, KnownAgentMessage, KnownSessionEntry, SessionEntry, SessionTreeNode};

/// What one fixture's lines amount to.
#[derive(Debug, PartialEq, Eq)]
struct FixtureCounts {
	lines: usize,
	/// Lines that write back byte for byte, as read.
	identical: usize,
	/// Lines that are a known entry (or the header) in their migrated shape (see `migrated`).
	known_migrated: usize,
	/// Lines whose migrated shape, read typed and written, equals TS's migrated object byte for byte.
	migrated_identical: usize,
	/// The same, comparing without the tree fields `id` and `parentId`.
	migrated_identical_without_tree_fields: usize,
}

fn fixture_counts(name: &str) -> FixtureCounts {
	let path = format!(
		"{}/../../../packages/coding-agent/test/fixtures/{name}",
		env!("CARGO_MANIFEST_DIR")
	);
	let text = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path}: {error}"));
	let mut counts = FixtureCounts {
		lines: 0,
		identical: 0,
		known_migrated: 0,
		migrated_identical: 0,
		migrated_identical_without_tree_fields: 0,
	};
	for line in text.lines().filter(|line| !line.is_empty()) {
		counts.lines += 1;
		let entry: FileEntry = common::read(line).unwrap_or_else(|error| panic!("{error}: {line}"));
		let written = write(&entry);
		assert_eq!(json(&written), json(line), "{line}");
		counts.identical += usize::from(written == line);
		let expected = migrated(json(line));
		let typed: FileEntry = serde_json::from_value(expected.clone()).unwrap_or_else(|error| panic!("{error}"));
		counts.known_migrated += usize::from(!matches!(typed, FileEntry::Entry(SessionEntry::Other(_))));
		let typed = write(&typed);
		assert_eq!(json(&typed), expected, "{line}");
		counts.migrated_identical += usize::from(typed == to_js_string(&expected));
		let without = |value: JsonValue| to_js_string(&JsonValue::Object(without_tree_fields(value)));
		counts.migrated_identical_without_tree_fields += usize::from(without(json(&typed)) == without(expected));
	}
	counts
}

fn json(text: &str) -> JsonValue {
	common::read(text).unwrap_or_else(|error| panic!("{error}: {text}"))
}

/// A v1 line in the shape TS `migrateV1ToV2` leaves it: placeholder `id` and `parentId` appended at the end, and a
/// compaction's `firstKeptEntryIndex` replaced by a `firstKeptEntryId` appended after them. Both fixtures are v1
/// sessions, whose entries read as `SessionEntry::Other` until migrated (C8); the header is left as read (TS also
/// appends `version`, which the header type does not need to be typed).
fn migrated(value: JsonValue) -> JsonValue {
	let JsonValue::Object(mut object) = value else {
		unreachable!("a session line is an object")
	};
	if object.get("type").and_then(JsonValue::as_str) == Some("session") {
		return JsonValue::Object(object);
	}
	object.insert("id".into(), "0000v1id".into());
	object.insert("parentId".into(), JsonValue::Null);
	if object.shift_remove("firstKeptEntryIndex").is_some() {
		object.insert("firstKeptEntryId".into(), "0000kept".into());
	}
	JsonValue::Object(object)
}

fn without_tree_fields(value: JsonValue) -> JsonObject {
	let JsonValue::Object(mut object) = value else {
		unreachable!("a session line is an object")
	};
	object.shift_remove("id");
	object.shift_remove("parentId");
	object
}

#[test]
fn large_session_fixture_round_trips() {
	// Every line writes back byte for byte as read, and every migrated entry reads typed. Typed, an entry writes
	// `id`/`parentId` after `type`, where TS's migration appends them at the end, so only the header is
	// byte-identical to TS's migrated form; without the tree fields every line is (the messages' writers used the
	// field orders of `pi-types`). C8 owns writing migrated entries in TS's order.
	assert_eq!(
		fixture_counts("large-session.jsonl"),
		FixtureCounts {
			lines: 1019,
			identical: 1019,
			known_migrated: 1019,
			migrated_identical: 1,
			migrated_identical_without_tree_fields: 1019,
		}
	);
}

#[test]
fn before_compaction_fixture_round_trips() {
	// As above; the two compactions also move `firstKeptEntryId` (appended by TS, in field order when typed).
	assert_eq!(
		fixture_counts("before-compaction.jsonl"),
		FixtureCounts {
			lines: 1003,
			identical: 1003,
			known_migrated: 1003,
			migrated_identical: 1,
			migrated_identical_without_tree_fields: 1001,
		}
	);
}

#[test]
fn header_round_trips_with_and_without_optional_fields() {
	let entry: FileEntry = round_trip(
		r#"{"type":"session","version":3,"id":"019a","timestamp":"2024-12-03T14:00:00.000Z","cwd":"/work","parentSession":"/s/a.jsonl"}"#,
	);
	assert!(matches!(entry, FileEntry::Header(header) if header.version == Some(3)));
	let entry: FileEntry = round_trip(
		r#"{"type":"session","id":"019a","timestamp":"2024-12-03T14:00:00.000Z","cwd":"/work","provider":"anthropic"}"#,
	);
	assert!(matches!(entry, FileEntry::Header(header) if header.extra.contains_key("provider")));
}

const TREE: &str = r#""id":"a1b2c3d4","parentId":"00ff00ff","timestamp":"2024-12-03T14:00:01.000Z""#;

fn known(text: &str) -> KnownSessionEntry {
	match round_trip::<SessionEntry>(text) {
		SessionEntry::Known(entry) => entry,
		SessionEntry::Other(object) => panic!("not known: {object:?}"),
	}
}

#[test]
fn every_entry_kind_round_trips() {
	let assistant = assistant();
	let cases = [
		format!(r#"{{"type":"message",{TREE},"message":{assistant}}}"#),
		r#"{"type":"message","id":"a1b2c3d4","parentId":null,"timestamp":"2024-12-03T14:00:01.000Z","message":{"role":"user","content":"hi","timestamp":1}}"#.to_owned(),
		format!(r#"{{"type":"thinking_level_change",{TREE},"thinkingLevel":"high"}}"#),
		format!(r#"{{"type":"model_change",{TREE},"provider":"openai","modelId":"gpt-5"}}"#),
		format!(r#"{{"type":"usage",{TREE},"kind":"cache_warm","provider":"anthropic","model":"claude","usage":{USAGE},"note":"warm"}}"#),
		format!(r#"{{"type":"usage",{TREE},"kind":"cache_warm","provider":"anthropic","model":"claude","usage":{USAGE}}}"#),
		format!(
			r#"{{"type":"compaction",{TREE},"summary":"S","firstKeptEntryId":"0000aaaa","tokensBefore":1200,"details":{{"readFiles":[]}},"usage":{USAGE},"fromHook":false,"systemMessage":{{"role":"system","content":"You are pi.","timestamp":1}}}}"#
		),
		format!(r#"{{"type":"compaction",{TREE},"summary":"S","firstKeptEntryId":"0000aaaa","tokensBefore":1200}}"#),
		format!(r#"{{"type":"branch_summary",{TREE},"fromId":"root","summary":"B","details":null,"usage":{USAGE},"fromHook":true}}"#),
		format!(r#"{{"type":"custom","customType":"todo",{TREE}}}"#).replace(r#""todo","#, r#""todo","data":{"items":[1]},"#),
		format!(r#"{{"type":"custom","customType":"todo",{TREE}}}"#),
		format!(r#"{{"type":"custom_message","customType":"note","content":"text","display":true,{TREE}}}"#),
		format!(
			r#"{{"type":"custom_message","customType":"note","content":[{{"type":"image","data":"AA==","mimeType":"image/png"}}],"display":false,"details":{{"k":1}},{TREE}}}"#
		),
		format!(r#"{{"type":"context_edit",{TREE},"targetId":"0000aaaa","replacement":null}}"#),
		format!(r#"{{"type":"context_edit",{TREE},"targetId":"0000aaaa","replacement":{{"content":"shorter"}}}}"#),
		format!(
			r#"{{"type":"context_edit",{TREE},"targetId":"0000aaaa","replacement":{{"content":[{{"type":"thinking","thinking":"t"}}]}}}}"#
		),
		format!(r#"{{"type":"label",{TREE},"targetId":"0000aaaa","label":"checkpoint"}}"#),
		format!(r#"{{"type":"label",{TREE},"targetId":"0000aaaa"}}"#),
		format!(r#"{{"type":"session_info",{TREE},"name":"Refactor"}}"#),
		format!(r#"{{"type":"session_info",{TREE}}}"#),
	];
	for case in &cases {
		known(case);
	}
}

#[test]
fn entry_fields_keep_null_and_absent_apart() {
	let KnownSessionEntry::BranchSummary(summary) = known(&format!(
		r#"{{"type":"branch_summary",{TREE},"fromId":"root","summary":"B","details":null}}"#
	)) else {
		panic!("a branch summary")
	};
	assert_eq!(summary.details, Some(JsonValue::Null));
	let KnownSessionEntry::ContextEdit(edit) = known(&format!(
		r#"{{"type":"context_edit",{TREE},"targetId":"x","replacement":null}}"#
	)) else {
		panic!("a context edit")
	};
	assert_eq!(edit.replacement, None);
}

#[test]
fn unknown_entry_types_and_malformed_known_entries_are_kept() {
	let entry: SessionEntry =
		round_trip(r#"{"type":"bookmark_v9","id":"a1","parentId":null,"timestamp":"t","pinned":[1,{"a":null}]}"#);
	assert!(matches!(entry, SessionEntry::Other(_)));
	// TS parity: a known type without its required fields is read without validation, so it is kept too.
	let entry: SessionEntry = round_trip(r#"{"type":"model_change","id":"a1","parentId":null}"#);
	assert!(matches!(entry, SessionEntry::Other(_)));
}

#[test]
fn a_null_field_that_is_missing_keeps_the_object_unchanged() {
	// `parentId: string | null` is required: a label without it is kept as `Other`, not rewritten with `null`.
	let entry: SessionEntry = round_trip(r#"{"type":"label","id":"a1","timestamp":"t","targetId":"b2","label":"x"}"#);
	assert!(matches!(entry, SessionEntry::Other(_)));
}

#[test]
fn entries_keep_unknown_fields_at_the_end() {
	let KnownSessionEntry::Message(entry) = known(&format!(
		r#"{{"type":"message",{TREE},"message":{{"role":"user","content":"hi","timestamp":1,"source":"rpc"}},"origin":"remote"}}"#
	)) else {
		panic!("a message entry")
	};
	assert_eq!(entry.extra.get("origin"), Some(&JsonValue::from("remote")));
	assert!(
		matches!(entry.message, pi_types::AgentMessage::Known(KnownAgentMessage::User(user)) if user.extra.contains_key("source"))
	);
}

#[test]
fn tree_nodes_round_trip() {
	let node: SessionTreeNode = round_trip(&format!(
		r#"{{"entry":{{"type":"label",{TREE},"targetId":"x","label":"l"}},"children":[{{"entry":{{"type":"session_info",{TREE}}},"children":[]}}],"label":"l","labelTimestamp":"2024-12-03T14:00:02.000Z"}}"#
	));
	assert_eq!(node.children.len(), 1);
}

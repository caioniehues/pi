//! The TS-generated delta corpus (`rust/fixtures/chord-delta/gen.mjs`): appliers, wire codec and `overlap` must give
//! what the TS implementation gave, results byte-identical after `to_js_string` and errors message-identical.

use anyhow::{Context, Result, bail, ensure};
use pi_chord::delta::{
	Decoder, Encoder, Op, Snapshot, WireOp, apply, apply_immutable, assert_valid_op, assert_valid_wire_op, overlap,
};
use pi_json::{JsonValue, to_js_string};

fn records(name: &str) -> Result<Vec<JsonValue>> {
	let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/chord-delta/").to_owned() + name;
	let text = std::fs::read_to_string(&path).with_context(|| format!("reading {path}"))?;
	text.lines()
		.enumerate()
		.map(|(index, line)| pi_json::parse(line).with_context(|| format!("{name}:{}", index + 1)))
		.collect()
}

fn field<'a>(record: &'a JsonValue, key: &str) -> Result<&'a JsonValue> {
	record.get(key).with_context(|| format!("record without {key}"))
}

fn array<'a>(record: &'a JsonValue, key: &str) -> Result<&'a Vec<JsonValue>> {
	field(record, key)?
		.as_array()
		.with_context(|| format!("{key} is not an array"))
}

fn text<'a>(record: &'a JsonValue, key: &str) -> Result<&'a str> {
	field(record, key)?
		.as_str()
		.with_context(|| format!("{key} is not a string"))
}

fn usize_field(record: &JsonValue, key: &str) -> Result<usize> {
	let value = field(record, key)?
		.as_u64()
		.with_context(|| format!("{key} is not an integer"))?;
	Ok(usize::try_from(value)?)
}

/// TS `apply(base, ops)`: each op validated, then applied, so the first failing op gives the message.
fn apply_in_place(base: &JsonValue, ops: &[JsonValue]) -> Result<JsonValue, String> {
	let mut value = base.clone();
	for op in ops {
		let op = assert_valid_op(op.clone()).map_err(|error| error.to_string())?;
		apply(&mut value, &[op]).map_err(|error| error.to_string())?;
	}
	Ok(value)
}

/// TS `applyImmutable(base, ops)`, the same way.
fn apply_persistent(base: &JsonValue, ops: &[JsonValue]) -> Result<JsonValue, String> {
	let mut value = Snapshot::from(base);
	for op in ops {
		let op = assert_valid_op(op.clone()).map_err(|error| error.to_string())?;
		value = apply_immutable(&value, &[op]).map_err(|error| error.to_string())?;
	}
	Ok(value.to_value())
}

#[test]
fn apply_cases_match_ts() -> Result<()> {
	let cases = records("apply-cases.jsonl")?;
	ensure!(cases.len() == 2000, "expected 2000 apply cases, found {}", cases.len());
	for (index, record) in cases.iter().enumerate() {
		let line = index + 1;
		let base = field(record, "base")?;
		let ops = array(record, "ops")?;
		let in_place = apply_in_place(base, ops);
		let persistent = apply_persistent(base, ops);
		let checked = match record.get("expected") {
			Some(expected) => check_result(base, ops, &to_js_string(expected), in_place, persistent),
			None => check_errors(record, &in_place, &persistent),
		};
		checked.with_context(|| format!("apply-cases:{line}"))?;
	}
	Ok(())
}

/// Both appliers reached the TS value, op by op and as one batch.
fn check_result(
	base: &JsonValue,
	ops: &[JsonValue],
	expected: &str,
	in_place: Result<JsonValue, String>,
	persistent: Result<JsonValue, String>,
) -> Result<()> {
	let in_place = in_place.map_err(anyhow::Error::msg)?;
	ensure!(
		to_js_string(&in_place) == expected,
		"apply: {}",
		to_js_string(&in_place)
	);
	let persistent = persistent.map_err(anyhow::Error::msg)?;
	ensure!(
		to_js_string(&persistent) == expected,
		"apply_immutable: {}",
		to_js_string(&persistent)
	);
	check_one_batch(base, ops, expected).context("as one batch")
}

/// Both appliers failed with the TS messages.
fn check_errors(
	record: &JsonValue,
	in_place: &Result<JsonValue, String>,
	persistent: &Result<JsonValue, String>,
) -> Result<()> {
	let error = text(record, "error")?;
	ensure!(
		in_place.as_ref().err().map(String::as_str) == Some(error),
		"apply: {in_place:?}"
	);
	let immutable_error = text(record, "immutableError")?;
	ensure!(
		persistent.as_ref().err().map(String::as_str) == Some(immutable_error),
		"apply_immutable: {persistent:?}"
	);
	Ok(())
}

/// The ops as one batch give what op-by-op application gave, in place and persistently.
fn check_one_batch(base: &JsonValue, ops: &[JsonValue], expected: &str) -> Result<()> {
	let ops: Vec<Op> = ops
		.iter()
		.map(|op| assert_valid_op(op.clone()))
		.collect::<Result<_, _>>()?;
	let mut value = base.clone();
	apply(&mut value, &ops)?;
	ensure!(to_js_string(&value) == expected, "apply");
	let persistent = apply_immutable(&Snapshot::from(base), &ops)?;
	ensure!(to_js_string(&persistent.to_value()) == expected, "apply_immutable");
	Ok(())
}

#[test]
fn wire_cases_match_ts() -> Result<()> {
	let cases = records("wire-cases.jsonl")?;
	ensure!(cases.len() == 1000, "expected 1000 wire cases, found {}", cases.len());
	for (index, record) in cases.iter().enumerate() {
		let line = index + 1;
		match record.get("ops") {
			Some(_) => check_round_trip(record).with_context(|| format!("wire-cases:{line}"))?,
			None => check_decode_error(record).with_context(|| format!("wire-cases:{line}"))?,
		}
	}
	Ok(())
}

/// One encoder over the batches writes the TS wire bytes; one decoder over those bytes restores the ops.
fn check_round_trip(record: &JsonValue) -> Result<()> {
	let batches = array(record, "ops")?;
	let wires = array(record, "wire")?;
	ensure!(batches.len() == wires.len(), "batch counts differ");
	let mut sender = Encoder::new();
	let mut receiver = Decoder::new();
	for (batch, wire) in batches.iter().zip(wires) {
		let ops: Vec<Op> = serde_json::from_value(batch.clone())?;
		let encoded = sender.encode(&ops);
		let written = to_js_string(&JsonValue::Array(encoded.iter().map(WireOp::to_json).collect()));
		ensure!(written == to_js_string(wire), "encode wrote {written}");
		let parsed: Vec<WireOp> = serde_json::from_value(wire.clone())?;
		ensure!(parsed == encoded, "the wire bytes parse to other ops than were encoded");
		ensure!(receiver.decode(&parsed)? == ops, "decode");
	}
	Ok(())
}

/// One decoder over the batches as received fails with the TS message.
fn check_decode_error(record: &JsonValue) -> Result<()> {
	let expected = text(record, "error")?;
	let mut decoder = Decoder::new();
	for batch in array(record, "wire")? {
		let values = batch.as_array().context("a batch is an array")?.clone();
		if let Err(error) = decoder.decode_values(values) {
			ensure!(error.to_string() == expected, "decode failed with {error}");
			return Ok(());
		}
	}
	bail!("decoded without the error {expected}")
}

#[test]
fn wire_errors_match_ts_through_serde_too() -> Result<()> {
	// A frame deserialized as a whole validates every tuple before decoding; where the first fault is a validation
	// fault, the message is the TS one.
	for record in records("wire-cases.jsonl")?
		.iter()
		.filter(|record| record.get("error").is_some())
	{
		for batch in array(record, "wire")? {
			let Err(error) = batch
				.as_array()
				.context("batch")?
				.iter()
				.cloned()
				.map(assert_valid_wire_op)
				.collect::<Result<Vec<_>, _>>()
			else {
				continue;
			};
			let serde_error = serde_json::from_value::<Vec<WireOp>>(batch.clone()).unwrap_err();
			ensure!(serde_error.to_string() == error.to_string(), "{serde_error} != {error}");
		}
	}
	Ok(())
}

#[test]
fn overlap_cases_match_ts() -> Result<()> {
	let cases = records("overlap-cases.jsonl")?;
	ensure!(cases.len() == 500, "expected 500 overlap cases, found {}", cases.len());
	for (index, record) in cases.iter().enumerate() {
		let n = overlap(
			text(record, "a")?,
			text(record, "b")?,
			usize_field(record, "scan")?,
			usize_field(record, "probe")?,
			usize_field(record, "max")?,
		);
		ensure!(n == usize_field(record, "n")?, "overlap-cases:{}: {n}", index + 1);
	}
	Ok(())
}

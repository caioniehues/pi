//! The Node-generated corpus (`rust/fixtures/json/gen.mjs`): every record must come out byte-identical.

use anyhow::{Context, Result, ensure};
use pi_json::{JsonValue, iso_ms, js_number_to_string, parse_iso_ms, to_js_string, to_js_string_pretty};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn records(name: &str) -> Result<Vec<JsonValue>> {
	let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/json/").to_owned() + name;
	let text = std::fs::read_to_string(&path).with_context(|| format!("reading {path}"))?;
	text.lines()
		.enumerate()
		.map(|(index, line)| serde_json::from_str(line).with_context(|| format!("{name}:{}", index + 1)))
		.collect()
}

fn field<'a>(record: &'a JsonValue, key: &str) -> Result<&'a JsonValue> {
	record
		.get(key)
		.with_context(|| format!("record without {key}: {record}"))
}

fn text<'a>(record: &'a JsonValue, key: &str) -> Result<&'a str> {
	field(record, key)?
		.as_str()
		.with_context(|| format!("{key} is not a string: {record}"))
}

#[test]
fn cases_stringify_like_node() -> Result<()> {
	let cases = records("cases.jsonl")?;
	ensure!(cases.len() == 5000, "expected 5000 cases, found {}", cases.len());
	for (index, record) in cases.iter().enumerate() {
		let value = field(record, "value")?;
		let line = index + 1;
		ensure!(
			to_js_string(value) == text(record, "compact")?,
			"cases.jsonl:{line} compact"
		);
		ensure!(
			to_js_string_pretty(value, 2) == text(record, "pretty2")?,
			"cases.jsonl:{line} pretty2"
		);
	}
	Ok(())
}

#[test]
fn numbers_print_like_node() -> Result<()> {
	let numbers = records("numbers.jsonl")?;
	ensure!(numbers.len() == 200, "expected 200 numbers, found {}", numbers.len());
	for record in &numbers {
		let n = field(record, "n")?.as_f64().context("n is a number")?;
		let printed = js_number_to_string(n);
		ensure!(printed == text(record, "s")?, "{record}: printed {printed}");
	}
	Ok(())
}

#[test]
fn dates_print_and_parse_like_node() -> Result<()> {
	let records = records("dates.jsonl")?;
	let (parses, dates): (Vec<_>, Vec<_>) = records.iter().partition(|record| record.get("parse").is_some());
	ensure!(dates.len() == 169, "expected 169 dates, found {}", dates.len());
	for record in dates {
		let ms = field(record, "ms")?.as_i64().context("ms is an integer")?;
		let time = from_epoch_ms(ms).context("time in range")?;
		let iso = text(record, "iso")?;
		ensure!(iso_ms(time) == iso, "{record}: printed {}", iso_ms(time));
		ensure!(
			parse_iso_ms(iso) == Some(time),
			"{record}: parsed {:?}",
			parse_iso_ms(iso)
		);
	}
	ensure!(parses.len() == 60, "expected 60 parse cases, found {}", parses.len());
	for record in parses {
		let expected = field(record, "ms")?.as_i64().map(from_epoch_ms);
		let result = parse_iso_ms(text(record, "parse")?);
		ensure!(result == expected.flatten(), "{record}: parsed {result:?}");
	}
	Ok(())
}

fn from_epoch_ms(ms: i64) -> Option<SystemTime> {
	let offset = Duration::from_millis(ms.unsigned_abs());
	if ms < 0 {
		UNIX_EPOCH.checked_sub(offset)
	} else {
		UNIX_EPOCH.checked_add(offset)
	}
}

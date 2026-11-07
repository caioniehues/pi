//! Readable differences between two artefact sets: a unified diff per differing file, missing files included.

use crate::artefacts::Artefacts;
use similar::TextDiff;
use std::fmt::Write;

/// The unified diffs of every file that differs between `expected` and `actual`; empty when they are equal.
pub fn diff_artefacts(expected: &Artefacts, actual: &Artefacts, expected_label: &str, actual_label: &str) -> String {
	let mut names: Vec<&String> = expected.keys().chain(actual.keys()).collect();
	names.sort();
	names.dedup();
	let mut out = String::new();
	for name in names {
		let left = expected.get(name).map_or("", String::as_str);
		let right = actual.get(name).map_or("", String::as_str);
		if expected.contains_key(name) && actual.contains_key(name) && left == right {
			continue;
		}
		let diff = TextDiff::from_lines(left, right);
		let unified = diff
			.unified_diff()
			.context_radius(2)
			.header(&format!("{expected_label}/{name}"), &format!("{actual_label}/{name}"))
			.to_string();
		if unified.is_empty() {
			let _ = writeln!(
				out,
				"--- {expected_label}/{name}\n+++ {actual_label}/{name}\n(file missing on one side)"
			);
		} else {
			out.push_str(&unified);
		}
	}
	out
}

#[cfg(test)]
mod tests {
	use super::*;

	fn set(entries: &[(&str, &str)]) -> Artefacts {
		entries
			.iter()
			.map(|(name, text)| ((*name).to_owned(), (*text).to_owned()))
			.collect()
	}

	#[test]
	fn equal_sets_have_no_diff() {
		let artefacts = set(&[("events.jsonl", "a\n")]);
		assert_eq!(diff_artefacts(&artefacts, &artefacts, "golden", "actual"), "");
	}

	#[test]
	fn changed_lines_show_as_a_unified_diff() {
		let diff = diff_artefacts(&set(&[("e", "a\nb\n")]), &set(&[("e", "a\nc\n")]), "golden", "actual");
		assert_eq!(diff, "--- golden/e\n+++ actual/e\n@@ -1,2 +1,2 @@\n a\n-b\n+c\n");
	}

	#[test]
	fn a_missing_file_is_reported() {
		let diff = diff_artefacts(&set(&[("e", "")]), &set(&[]), "golden", "actual");
		assert!(diff.contains("(file missing on one side)"));
	}
}

//! `verify-vendor`: every file listed in a `vendor/*/SHA256SUMS` has the listed SHA-256, as `sha256sum -c` checks.

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Component, Path};

/// One line per failed or malformed entry; no vendored directories is a pass.
pub fn verify_vendor(workspace: &Path) -> Result<Vec<String>> {
	let vendor = workspace.join("vendor");
	let mut failures = Vec::new();
	if !vendor.is_dir() {
		return Ok(failures);
	}
	let mut sums = Vec::new();
	for entry in fs::read_dir(&vendor)? {
		let path = entry?.path().join("SHA256SUMS");
		if path.is_file() {
			sums.push(path);
		}
	}
	sums.sort();
	for path in sums {
		verify_sums(&path, &mut failures)?;
	}
	Ok(failures)
}

/// Checks each `<64 hex>  <file>` (or `<64 hex> *<file>`) line; the file is relative to the `SHA256SUMS` directory
/// and stays inside it. `sha256sum`'s backslash-escaped lines (names with `\` or a newline) are not supported.
fn verify_sums(path: &Path, failures: &mut Vec<String>) -> Result<()> {
	let directory = path.parent().context("SHA256SUMS has a parent directory")?;
	let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
	for (index, line) in text.lines().enumerate().filter(|(_, line)| !line.trim().is_empty()) {
		let at = format!("{}:{}", path.display(), index + 1);
		// Trust boundary: SHA256SUMS is a file on disk, written by whoever vendored the directory.
		let Some((expected, name)) = parse_line(line) else {
			failures.push(format!(
				"{at}: malformed line, expected `<64 hex digits>  <relative path>`"
			));
			continue;
		};
		let file = directory.join(name);
		match fs::read(&file) {
			Ok(bytes) if hex(&Sha256::digest(&bytes)) == expected.to_ascii_lowercase() => {}
			Ok(_) => failures.push(format!("{}: FAILED (checksum mismatch)", file.display())),
			Err(error) => failures.push(format!("{}: FAILED ({error})", file.display())),
		}
	}
	Ok(())
}

/// The digest and the file name of a line, when the digest is 64 hex digits and the name a plain relative path.
fn parse_line(line: &str) -> Option<(&str, &Path)> {
	if line.starts_with('\\') {
		return None;
	}
	let (expected, rest) = line.split_once(' ')?;
	let name = Path::new(rest.strip_prefix([' ', '*'])?);
	let digest_ok = expected.len() == 64 && expected.bytes().all(|byte| byte.is_ascii_hexdigit());
	let relative = name
		.components()
		.all(|component| matches!(component, Component::Normal(_) | Component::CurDir));
	(digest_ok && relative && !name.as_os_str().is_empty()).then_some((expected, name))
}

fn hex(bytes: &[u8]) -> String {
	const DIGITS: &[u8; 16] = b"0123456789abcdef";
	bytes
		.iter()
		.flat_map(|byte| [DIGITS[usize::from(byte >> 4)], DIGITS[usize::from(byte & 0x0f)]])
		.map(char::from)
		.collect()
}

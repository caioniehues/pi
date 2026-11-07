//! The official JSON-Schema-Test-Suite, drafts 2020-12 and 7 (TS `test/conformance.test.ts` and `test/suite.ts`):
//! every test file found under the vendored fixtures, including the optional and format tests, with the remote
//! documents and meta-schemas the tests reference. A test listed in `KNOWN_FAILURES` must fail and every other test
//! must pass, so the list stays exact. Each test also checks that `errors()` agrees with `check()`.

use anyhow::{Context, Result, bail};
use pi_json::JsonValue;
use pi_schema::{CompileOptions, Draft, FormatSet, compile, formats};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

const KNOWN_FAILURES: [&str; 9] = [
	// Vocabularies switched off by a custom meta-schema are not supported.
	"draft2020-12/vocabulary > schema that uses custom metaschema with with no validation vocabulary > no validation: invalid number, but it still validates",
	"draft2020-12/optional/format-assertion > schema that uses custom metaschema with format-assertion: false > format-assertion: false: invalid string",
	"draft2020-12/optional/format-assertion > schema that uses custom metaschema with format-assertion: true > format-assertion: true: invalid string",
	// A $ref to a document of another draft is evaluated with the compile's draft.
	"draft2020-12/optional/cross-draft > refs to historic drafts are processed as historic drafts > first item not a string is valid",
	"draft7/optional/cross-draft > refs to future drafts are processed as future drafts > missing bar is invalid",
	// contentMediaType and contentEncoding are annotations.
	"draft7/optional/content > validation of string-encoded content based on media type > an invalid JSON document",
	"draft7/optional/content > validation of binary string-encoding > an invalid base64 string (% is not a valid character)",
	"draft7/optional/content > validation of binary-encoded media type documents > a validly-encoded invalid JSON document",
	"draft7/optional/content > validation of binary-encoded media type documents > an invalid base64 string that is valid JSON",
];

fn fixtures() -> PathBuf {
	Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../packages/schema/test/fixtures")
}

fn read_json(path: &Path) -> Result<JsonValue> {
	let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
	pi_json::parse(&text).with_context(|| format!("parsing {}", path.display()))
}

/// Every `.json` file below `dir`, sorted by name at each level (as `suite.ts` walks them).
fn json_files(dir: &Path) -> Result<Vec<PathBuf>> {
	let mut entries: Vec<PathBuf> = fs::read_dir(dir)?
		.map(|e| e.map(|e| e.path()))
		.collect::<Result<_, _>>()?;
	entries.sort();
	let mut out = Vec::new();
	for path in entries {
		if path.is_dir() {
			out.extend(json_files(&path)?);
		} else if path.extension().is_some_and(|e| e == "json") {
			out.push(path);
		}
	}
	Ok(out)
}

/// The remote documents and meta-schemas the tests reference, by URI.
fn suite_schemas() -> Result<Vec<(String, JsonValue)>> {
	let root = fixtures().join("json-schema-test-suite/remotes");
	let mut schemas = Vec::new();
	for path in json_files(&root)? {
		let relative = path.strip_prefix(&root)?.to_string_lossy().replace('\\', "/");
		schemas.push((format!("http://localhost:1234/{relative}"), read_json(&path)?));
	}
	let meta = fixtures().join("meta-schemas");
	schemas.push((
		"http://json-schema.org/draft-07/schema".to_owned(),
		read_json(&meta.join("draft-07-schema.json"))?,
	));
	schemas.push((
		"https://json-schema.org/draft/2020-12/schema".to_owned(),
		read_json(&meta.join("draft2020-12-schema.json"))?,
	));
	for vocabulary in [
		"core",
		"applicator",
		"unevaluated",
		"validation",
		"meta-data",
		"format-annotation",
		"content",
	] {
		schemas.push((
			format!("https://json-schema.org/draft/2020-12/meta/{vocabulary}"),
			read_json(&meta.join(format!("draft2020-12-meta-{vocabulary}.json")))?,
		));
	}
	Ok(schemas)
}

/// Run counts of one draft.
#[derive(Default)]
struct Tally {
	run: usize,
	passed: usize,
	known: usize,
}

#[test]
fn official_suite() -> Result<()> {
	let schemas = suite_schemas()?;
	let known: BTreeSet<&str> = KNOWN_FAILURES.into_iter().collect();
	let tests = fixtures().join("json-schema-test-suite/tests");
	let mut problems = Vec::new();
	for (dir, draft) in [("draft2020-12", Draft::V2020_12), ("draft7", Draft::V7)] {
		let mut tally = Tally::default();
		for path in json_files(&tests.join(dir))? {
			let name = path
				.strip_prefix(&tests)?
				.with_extension("")
				.to_string_lossy()
				.replace('\\', "/");
			// Format tests assert formats; everywhere else `format` is an annotation, as draft 2020-12 specifies.
			let formats = if name.contains("/optional/format/") {
				formats::standard()
			} else {
				FormatSet::new()
			};
			let options = CompileOptions {
				draft,
				formats,
				schemas: schemas.clone(),
				..CompileOptions::default()
			};
			run_file(&name, &read_json(&path)?, &options, &known, &mut tally, &mut problems)?;
		}
		println!(
			"{dir}: {} tests, {} passed, {} known failures",
			tally.run, tally.passed, tally.known
		);
	}
	if !problems.is_empty() {
		bail!("{} problems:\n{}", problems.len(), problems.join("\n"));
	}
	Ok(())
}

fn run_file(
	name: &str,
	groups: &JsonValue,
	options: &CompileOptions,
	known: &BTreeSet<&str>,
	tally: &mut Tally,
	problems: &mut Vec<String>,
) -> Result<()> {
	for group in groups.as_array().context("a test file is an array")? {
		let description = group["description"].as_str().context("group description")?;
		let compiled = compile(&group["schema"], options.clone());
		for test in group["tests"].as_array().context("group tests")? {
			let case = format!(
				"{name} > {description} > {}",
				test["description"].as_str().context("test description")?
			);
			let expected = test["valid"].as_bool().context("test valid")?;
			tally.run += 1;
			let outcome = match &compiled {
				Ok(validator) => {
					let check = validator.check(&test["data"]);
					let errors = validator.errors(&test["data"]);
					if errors.is_empty() != check {
						problems.push(format!("{case}: errors {errors:?} disagree with check {check}"));
					}
					Ok(check)
				}
				Err(error) => Err(error.to_string()),
			};
			let passed = outcome.as_ref().is_ok_and(|check| *check == expected);
			match (passed, known.contains(case.as_str())) {
				(true, false) => tally.passed += 1,
				(false, true) => tally.known += 1,
				(true, true) => problems.push(format!("{case}: a known failure passes, remove it from the list")),
				(false, false) => problems.push(format!("{case}: expected {expected}, got {outcome:?}")),
			}
		}
	}
	Ok(())
}

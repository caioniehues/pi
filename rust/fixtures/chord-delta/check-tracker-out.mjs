// Checks what the Rust tracker prepared for the cross-apply corpus (rust/docs/slices/D0b.md, "Decided rule for the
// tracker"): every op of `tracker-out.jsonl` is valid under the TS `assertValidOp`, and the TS `apply` and
// `applyImmutable` replay `{base, ops}` to `value`, byte for byte after JSON.stringify (so key order too).
// Run with `node rust/fixtures/chord-delta/check-tracker-out.mjs [tracker-out.jsonl]`; the Rust test
// `crates/pi-chord/tests/tracker_corpus.rs` writes the file and runs this. Exits 1 with the first failures.

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { apply, applyImmutable, assertValidOp } from "../../../packages/chord/src/delta/index.ts";

const here = dirname(fileURLToPath(import.meta.url));
const file = process.argv[2] ?? join(here, "tracker-out.jsonl");
const lines = readFileSync(file, "utf8").split("\n").filter((line) => line !== "");
const failures = [];
let ops = 0;
for (const [index, line] of lines.entries()) {
	const { base, ops: batch, value } = JSON.parse(line);
	const expected = JSON.stringify(value);
	try {
		for (const op of batch) assertValidOp(op);
		ops += batch.length;
		const inPlace = JSON.stringify(apply(structuredClone(base), structuredClone(batch)));
		if (inPlace !== expected) throw new Error(`apply gave ${inPlace}`);
		const persistent = JSON.stringify(applyImmutable(structuredClone(base), structuredClone(batch)));
		if (persistent !== expected) throw new Error(`applyImmutable gave ${persistent}`);
	} catch (error) {
		failures.push(`line ${index + 1}: ${error.message}\n  expected ${expected}`);
		if (failures.length >= 10) break;
	}
}
if (failures.length > 0) {
	console.error(failures.join("\n"));
	process.exit(1);
}
console.log(`checked ${lines.length} prepared changes, ${ops} ops: TS apply and applyImmutable reach every value`);

// Generates coerce-typebox.json from packages/schema/test/coerce-typebox.test.ts (TypeBox 1.3.27's Value.Convert
// tests, ported to pi-schema's conversion pass): every `it` case with the JSON of its schemas, inputs and asserted
// results, for the Rust port's src/coerce/typebox_tests.rs. TypeBox builds the schemas, so its JSON is what the Rust
// port compiles. A case whose schema, input or expected value has no JSON form (bigint, undefined, symbol) is kept
// with `skipped` and the reason, as is every `it.skip` case.
//
// Usage (Node 26; TypeBox is not a dependency of this repo, install it anywhere):
//   npm install --prefix /tmp/typebox --ignore-scripts typebox@1.3.27
//   TYPEBOX=/tmp/typebox/node_modules/typebox node rust/fixtures/schema/gen-coerce-typebox.mjs

import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const schemaPackage = resolve(here, "../../../packages/schema");
const typeboxDir = process.env.TYPEBOX;
if (!typeboxDir) throw new Error("set TYPEBOX to the typebox 1.3.27 package directory");
const typebox = await import(pathToFileURL(join(typeboxDir, "build/index.mjs")).href);
const src = (path) => import(pathToFileURL(join(schemaPackage, "src", path)).href);
const { Coercer } = await src("coerce.ts");
const { Compiler } = await src("compiler.ts");
const { formats } = await src("formats/index.ts");
const { compile } = await src("index.ts");

/** Whether `value` survives JSON unchanged: no undefined, bigint, symbol, function or non-finite number (-0 becomes 0, as JSON.stringify writes it). */
function jsonable(value) {
	if (value === null || typeof value === "string" || typeof value === "boolean") return true;
	if (typeof value === "number") return Number.isFinite(value);
	if (Array.isArray(value)) return value.every((item) => item !== undefined && jsonable(item));
	if (typeof value === "object" && Object.getPrototypeOf(value) === Object.prototype) {
		return Object.values(value).every((item) => item !== undefined && jsonable(item));
	}
	return false;
}

const cases = [];
let current;
const names = [];

class RecordingCoercer extends Coercer {
	constructor(compiler, schema) {
		super(compiler, schema);
		this.recordedSchema = schema;
	}
	convertOnly(value) {
		const result = super.convertOnly(value);
		current.steps.push({ op: "convert", schema: this.recordedSchema, value, result });
		return result;
	}
}

function recordingCompile(schema, options) {
	const validator = compile(schema, options);
	return {
		check(value) {
			const result = validator.check(value);
			current.steps.push({ op: "check", schema, value, result });
			return result;
		},
	};
}

function expect(actual) {
	return {
		toEqual(expected) {
			current.asserts.push({ kind: "equal", actual, expected });
		},
		toBe(expected) {
			current.asserts.push({ kind: "bool", actual, expected });
		},
	};
}

function describe(name, body) {
	names.push(name);
	body();
	names.pop();
}

function it(name, body) {
	current = { name: [...names, name].join(" > "), steps: [], asserts: [] };
	body();
	cases.push(finish(current));
}
it.skip = (name) => cases.push({ name: [...names, name].join(" > "), skipped: "it.skip in the TS port" });

/** The case's operations, each with the value its assertion expects. */
function finish(record) {
	const steps = [];
	for (const assert of record.asserts) {
		if (assert.kind === "equal") {
			const step = record.steps.findLast((s) => s.op === "convert" && Object.is(s.result, assert.actual));
			if (step === undefined) throw new Error(`${record.name}: an assertion without its conversion`);
			steps.push({ op: "convert", schema: step.schema, value: step.value, expected: assert.expected });
		} else {
			const step = record.steps.findLast((s) => s.op === "check");
			if (step === undefined || step.result !== assert.actual) {
				throw new Error(`${record.name}: an assertion without its check`);
			}
			steps.push({ op: "check", schema: step.schema, value: step.value, expected: assert.expected });
		}
	}
	if (steps.length === 0) throw new Error(`${record.name}: no assertion`);
	const json = steps.map((s) => ({ ...s, schema: schemaJson(s.schema) }));
	if (!json.every((s) => s.schema !== undefined && jsonable(s.value) && jsonable(s.expected))) {
		return { name: record.name, skipped: "the schema, an input or an expected value has no JSON form" };
	}
	return { name: record.name, steps: json };
}

/** A schema's JSON (TypeBox's symbol keys drop out), undefined when it holds a bigint. */
function schemaJson(schema) {
	try {
		return JSON.parse(JSON.stringify(schema));
	} catch {
		return undefined;
	}
}

const test = readFileSync(join(schemaPackage, "test/coerce-typebox.test.ts"), "utf8")
	.replace(/^import \{ Type \} from "typebox";$/m, "const { Type } = globalThis.__gen;")
	.replace(/^import \{ describe, expect, it \} from "vitest";$/m, "const { describe, expect, it } = globalThis.__gen;")
	.replace(/^import \{ Coercer \} from "..\/src\/coerce.ts";$/m, "const { Coercer } = globalThis.__gen;")
	.replace(/^import \{ Compiler \} from "..\/src\/compiler.ts";$/m, "const { Compiler } = globalThis.__gen;")
	.replace(/^import \{ formats \} from "..\/src\/formats\/index.ts";$/m, "const { formats } = globalThis.__gen;")
	.replace(/^import \{ compile \} from "..\/src\/index.ts";$/m, "const { compile } = globalThis.__gen;");
if (/^import /m.test(test)) throw new Error("an import of the test file was not replaced");
globalThis.__gen = {
	Type: typebox.Type,
	describe,
	expect,
	it,
	Coercer: RecordingCoercer,
	Compiler,
	formats,
	compile: recordingCompile,
};
const dir = mkdtempSync(join(tmpdir(), "gen-coerce-typebox-"));
try {
	const file = join(dir, "test.ts");
	writeFileSync(file, test);
	await import(pathToFileURL(file).href);
} finally {
	rmSync(dir, { recursive: true });
}

writeFileSync(join(here, "coerce-typebox.json"), `${JSON.stringify(cases, null, "\t")}\n`);
const skipped = cases.filter((c) => c.skipped !== undefined).length;
console.log(`${cases.length} cases, ${cases.length - skipped} with JSON, ${skipped} skipped`);

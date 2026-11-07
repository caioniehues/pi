// Generates the pi-chord delta corpus from the TS implementation in packages/chord/src/delta/index.ts.
// Run from anywhere with `node rust/fixtures/chord-delta/gen.mjs` (Node 26 strips the module's types; the delta module
// has no package dependencies). The seed is fixed, so the output is reproducible. Writes, next to this file:
//   apply-cases.jsonl    {"base", "ops", "expected"}, where apply and applyImmutable agree, or
//                        {"base", "ops", "error", "immutableError"}: the messages apply and applyImmutable throw
//   wire-cases.jsonl     {"ops": [batch, ...], "wire": [batch, ...]}: one encoder over the batches in order, or
//                        {"wire": [batch, ...], "error"}: the message one decoder throws on the batches in order
//   overlap-cases.jsonl  {"a", "b", "scan", "probe", "max", "n"}: n = overlap(a, b, scan, probe, max)
// Strings hold no lone surrogates and no `t` count splits a surrogate pair: a Rust string cannot hold half a pair.

import { writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { apply, applyImmutable, decoder, encoder, overlap } from "../../../packages/chord/src/delta/index.ts";

const APPLY_CASES = 2000;
const WIRE_CASES = 1000;
const OVERLAP_CASES = 500;
const here = dirname(fileURLToPath(import.meta.url));

// sfc32: a small seeded generator; Math.random cannot be seeded.
let sa = 0x2545f491;
let sb = 0x9e3779b9;
let sc = 0x243f6a88;
let sd = 1;
function nextU32() {
	sa >>>= 0;
	sb >>>= 0;
	sc >>>= 0;
	sd >>>= 0;
	const t = (((sa + sb) | 0) + sd) | 0;
	sd = (sd + 1) | 0;
	sa = sb ^ (sb >>> 9);
	sb = (sc + (sc << 3)) | 0;
	sc = (sc << 21) | (sc >>> 11);
	sc = (sc + t) | 0;
	return t >>> 0;
}
for (let i = 0; i < 20; i++) nextU32();

const int = (n) => nextU32() % n;
const chance = (p) => nextU32() / 2 ** 32 < p;
const pick = (list) => list[int(list.length)];
const clone = (value) => (value === undefined ? undefined : JSON.parse(JSON.stringify(value)));

// Strings with multi-byte and astral characters, escapes, separators and the empty string.
const TEXTS = ["", "a", "bc", "hello", "é", "😀", "a😀b", "日本語", " ", '"q"', "\\", "\n\t", "x".repeat(20)];
// Keys that JS orders first (canonical indices), look-alikes that it does not, separators and a NUL.
const KEYS = ["a", "b", "c", "text", "list", "0", "1", "10", "01", "a.b", "x y", "a\u0000b", "é", "length"];
const NUMBERS = [0, 1, -1, 7, 42, 1.5, -2.25, 0.1, 1e21, 1e-7, 2 ** 53, 123456.789];

function primitive() {
	switch (int(5)) {
		case 0:
			return pick(NUMBERS);
		case 1:
			return int(100);
		case 2:
			return pick(TEXTS);
		case 3:
			return chance(0.5);
		default:
			return null;
	}
}

function value(depth = 0) {
	const roll = int(10);
	if (depth > 2 || roll < 4) return primitive();
	if (roll < 7) return Array.from({ length: int(4) }, () => value(depth + 1));
	const object = {};
	for (let count = int(4); count > 0; count--) object[pick(KEYS)] = value(depth + 1);
	return object;
}

function root() {
	const roll = int(20);
	if (roll === 0) return primitive();
	if (roll < 4) return Array.from({ length: 1 + int(4) }, () => value(1));
	const object = {};
	for (let count = 1 + int(5); count > 0; count--) object[pick(KEYS)] = value(1);
	return object;
}

// ─── apply-cases ─────────────────────────────────────────────────────────────

/** Every container under `node` with its path, the root first. */
function containers(node, path = [], out = []) {
	if (node === null || typeof node !== "object") return out;
	out.push([path, node]);
	const keys = Array.isArray(node) ? node.map((_, index) => index) : Object.keys(node);
	for (const key of keys) containers(node[key], [...path, key], out);
	return out;
}

/** An object key as a path segment: a canonical index sometimes as a number, which names the same property. */
function objectSegment(key) {
	return /^(0|[1-9][0-9]*)$/.test(key) && chance(0.4) ? Number(key) : key;
}

/** A count for `t` that does not leave half a surrogate pair at the front. */
function truncateCount(text, count) {
	const code = text.charCodeAt(count);
	return code >= 0xdc00 && code <= 0xdfff && count > 0 ? count + 1 : count;
}

/** One valid op against `current`, or undefined when the picked container offers none. */
function validOp(current) {
	if (chance(0.03)) return ["r", root()];
	const all = containers(current);
	if (all.length === 0) return ["r", root()];
	const [path, node] = pick(all);
	if (Array.isArray(node)) {
		const length = node.length;
		switch (int(6)) {
			case 0:
				return ["s", [...path, int(length + 1)], value(1)];
			case 1:
				return length > 0 ? ["d", [...path, int(length)]] : undefined;
			case 2:
				return ["p", path, int(length + 3), int(length + 3), Array.from({ length: int(4) }, () => value(1))];
			case 3: {
				const permutation = node.map((_, index) => index);
				for (let index = permutation.length - 1; index > 0; index--) {
					const other = int(index + 1);
					[permutation[index], permutation[other]] = [permutation[other], permutation[index]];
				}
				return ["m", path, permutation];
			}
			default:
				return stringOp(node, path, node.map((_, index) => index));
		}
	}
	const keys = Object.keys(node);
	switch (int(5)) {
		case 0:
			return ["s", [...path, objectSegment(chance(0.6) && keys.length > 0 ? pick(keys) : pick(KEYS))], value(1)];
		case 1:
			return ["d", [...path, objectSegment(chance(0.8) && keys.length > 0 ? pick(keys) : pick(KEYS))]];
		default:
			return stringOp(node, path, keys);
	}
}

function stringOp(node, path, keys) {
	const strings = keys.filter((key) => typeof node[key] === "string");
	if (strings.length === 0) return undefined;
	const key = pick(strings);
	const segment = typeof key === "number" ? key : objectSegment(key);
	if (chance(0.6)) return ["a", [...path, segment], pick(TEXTS)];
	const text = node[key];
	return ["t", [...path, segment], truncateCount(text, int(text.length + 3))];
}

/** A batch of valid ops from `base`, each picked against the value the ops before it produced. */
function validBatch(base) {
	let current = clone(base);
	const ops = [];
	for (let attempts = 0, length = 1 + int(8); ops.length < length && attempts < 40; attempts++) {
		const op = validOp(current);
		if (op === undefined) continue;
		current = apply(current, [clone(op)]);
		ops.push(op);
	}
	return ops;
}

/** One way to break an op: each class of a validator, path or index error. */
const CORRUPTIONS = [
	(op) => [...op, 1],
	(op) => op.slice(0, 1),
	(op) => [pick(["ZZZ", "S", "", 5, null, true, [], {}]), ...op.slice(1)],
	(op) => (op.length > 1 ? [op[0], pick(["a", 5, null, {}]), ...op.slice(2)] : op),
	(op) => (Array.isArray(op[1]) ? [op[0], [...op[1], pick(["__proto__", "constructor", "prototype"])], ...op.slice(2)] : op),
	(op) => (Array.isArray(op[1]) ? [op[0], [pick([-1, 1.5, null, true, {}, [1, 2]]), ...op[1]], ...op.slice(2)] : op),
	(op) => (Array.isArray(op[1]) ? [op[0], ["missing", ...op[1]], ...op.slice(2)] : op),
	(op) => (Array.isArray(op[1]) && op[1].length > 0 ? [op[0], [...op[1], "deeper"], ...op.slice(2)] : op),
	(op) => (Array.isArray(op[1]) ? [op[0], [...op[1].slice(0, -1), 1_000_000], ...op.slice(2)] : op),
	(op) => (op[0] === "p" ? [op[0], op[1], pick([-1, 0.5, "0"]), op[3], op[4]] : op),
	(op) => (op[0] === "p" ? [op[0], op[1], op[2], pick([-1, 0.5, null]), op[4]] : op),
	(op) => (op[0] === "p" ? [op[0], op[1], op[2], op[3], pick(["x", {}, null, 1])] : op),
	(op) => (op[0] === "m" ? [op[0], op[1], pick(["x", [0, 0], [...op[2], op[2].length], [-1], [0.5]])] : op),
	(op) => (op[0] === "t" ? [op[0], op[1], pick([-1, 0.5, "1", null])] : op),
	(op) => (op[0] === "a" ? [op[0], op[1], pick([1, null, ["x"]])] : op),
	(op) => (op.length > 1 && Array.isArray(op[1]) ? [op[0], [], ...op.slice(2)] : op),
];

/** Hand-written cases for every branch: validator messages, their order, path and index errors, JS quirks. */
function targetedCases() {
	const doc = () => ({ a: "text", n: 5, o: { x: 1, "1": "one" }, arr: [1, "two", { k: "v" }], e: [], s: "😀é" });
	const out = [];
	const add = (base, ...ops) => out.push({ base, ops });
	for (const op of [null, 5, "s", {}, { op: "s" }, [], true]) add(doc(), op);
	for (const verb of ["ZZZ", "S", "", 5, null, true, [1, [2, 3]], [null], {}, "#"]) add(doc(), [verb, ["a"], 1]);
	const arity = [
		["r"],
		["r", 1, 2],
		["s", ["a"]],
		["s", ["a"], 1, 2],
		["d"],
		["d", ["a"], 1],
		["a", ["a"]],
		["a", ["a"], 1],
		["a", ["a"], "x", 1],
		["t", ["a"]],
		["t", ["a"], -1],
		["t", ["a"], 1.5],
		["t", ["a"], "1"],
		["t", ["a"], null],
		["t", ["a"], 1, 2],
		["p", ["arr"], 0, 0],
		["p", ["arr"], 0, 0, [], 1],
		["p", ["arr"], -1, 0, []],
		["p", ["arr"], 0.5, 0, []],
		["p", ["arr"], "0", 0, []],
		["p", ["arr"], 0, -1, []],
		["p", ["arr"], 0, "1", []],
		["p", ["arr"], 0, 0, {}],
		["p", ["arr"], 0, 0, "x"],
		["p", ["arr"], 0, 0, null],
		["m", ["arr"]],
		["m", ["arr"], [0, 1, 2], 1],
		["m", ["arr"], "x"],
		["m", ["arr"], [0, 0, 1]],
		["m", ["arr"], [0, 1, 3]],
		["m", ["arr"], [-1, 0, 1]],
		["m", ["arr"], [0.5, 1, 2]],
		["m", ["arr"], [null, 1, 2]],
		["m", ["arr"], ["0", 1, 2]],
		["m", ["arr"], [1, 0]],
		["m", ["arr"], []],
	];
	for (const op of arity) add(doc(), op);
	for (const path of ["a", 5, null, {}, true]) {
		add(doc(), ["s", path, 1]);
		add(doc(), ["d", path]);
		add(doc(), ["a", path, "x"]);
		add(doc(), ["t", path, 1]);
		add(doc(), ["p", path, 0, 0, []]);
		add(doc(), ["m", path, [0]]);
	}
	for (const verb of ["s", "d", "a", "t"]) {
		const tail = verb === "s" ? [1] : verb === "a" ? ["x"] : verb === "t" ? [0] : [];
		add(doc(), [verb, [], ...tail]);
		for (const seg of ["__proto__", "constructor", "prototype", -1, 1.5, -0.5, null, true, {}, [1], [], [null, 2]]) {
			add(doc(), [verb, [seg], ...tail]);
			add(doc(), [verb, ["o", seg], ...tail]);
			add(doc(), [verb, [seg, "x"], ...tail]);
		}
	}
	for (const seg of ["__proto__", "constructor", -1, 1.5, null]) {
		add(doc(), ["p", [seg], 0, 0, []]);
		add(doc(), ["m", ["arr", seg], [0]]);
	}
	// Check order: shape before path for `t` and `a`, path before index for `p`, path before permutation for `m`.
	add(doc(), ["t", ["__proto__"], -1]);
	add(doc(), ["a", ["__proto__"], 1]);
	add(doc(), ["a", "x", 1]);
	add(doc(), ["p", ["__proto__"], -1, 0, []]);
	add(doc(), ["p", "arr", -1, 0, []]);
	add(doc(), ["m", ["__proto__"], [0, 0]]);
	add(doc(), ["m", "arr", "x"]);
	add(doc(), ["s", [], 1, 2]);
	// Unresolvable paths.
	add(doc(), ["s", ["missing", "x"], 1]);
	add(doc(), ["s", ["n", "x"], 1]);
	add(doc(), ["s", ["a", "x", "y"], 1]);
	add(doc(), ["s", ["arr", 0, "x"], 1]);
	add(doc(), ["s", ["arr", 9, "x"], 1]);
	add(doc(), ["s", ["arr", 3, "x"], 1]);
	add(doc(), ["s", ["o", "x", "y"], 1]);
	add(5, ["s", ["a"], 1]);
	add(null, ["s", ["a"], 1]);
	add("text", ["a", ["0"], "x"]);
	add(doc(), ["d", ["arr", 3]]);
	add(doc(), ["d", ["arr", 9]]);
	add(doc(), ["d", ["missing", "x"]]);
	add(doc(), ["a", ["missing"], "x"]);
	add(doc(), ["a", ["n"], "x"]);
	add(doc(), ["a", ["o"], "x"]);
	add(doc(), ["a", ["arr", 3], "x"]);
	add(doc(), ["a", ["arr", 0], "x"]);
	add(doc(), ["t", ["n"], 1]);
	add(doc(), ["t", ["missing"], 1]);
	add(doc(), ["t", ["arr", 3], 1]);
	add(doc(), ["p", ["o"], 0, 0, []]);
	add(doc(), ["p", ["missing"], 0, 0, []]);
	add(doc(), ["p", ["a"], 0, 0, []]);
	add(doc(), ["p", [], 0, 0, []]);
	add(doc(), ["p", ["arr", 2], 0, 0, []]);
	add(doc(), ["m", ["o"], [0]]);
	add(doc(), ["m", [], [0]]);
	add(doc(), ["m", ["arr"], [0, 1]]);
	add(doc(), ["m", ["e"], [0]]);
	add(5, ["p", [], 0, 0, []]);
	add(null, ["m", [], []]);
	add(doc(), ["r", 5], ["s", ["a"], 1]);
	add(doc(), ["r", [1]], ["a", [0], "x"]);
	// Array parents: string keys, indices past the end, huge indices.
	for (const key of ["0", "1", "x", "length", "01"]) {
		add(doc(), ["s", ["arr", key], 1]);
		add(doc(), ["s", ["arr", key, "k"], 1]);
		add(doc(), ["a", ["arr", key], "x"]);
		add(doc(), ["d", ["arr", key]]);
		add(doc(), ["p", ["arr", key], 0, 0, []]);
		add(doc(), ["m", ["arr", key], [0]]);
	}
	for (const index of [4, 5, 9, 4_294_967_290, 2 ** 53]) {
		add(doc(), ["s", ["arr", index], 1]);
		add(doc(), ["s", ["e", index], 1]);
		add(doc(), ["t", ["arr", index], 1]);
		add(doc(), ["a", ["arr", index], "x"]);
	}
	// An error after valid ops, and a valid op after an invalid one.
	add(doc(), ["s", ["n"], 6], ["a", ["a"], "!"], ["s", ["arr", 7], 1]);
	add(doc(), ["s", ["arr", 7], 1], ["s", ["n"], 6]);
	add(doc(), ["s", ["n"], 6], ["t", ["a"], -1]);
	add(doc(), ["d", ["o"]], ["s", ["o", "x"], 1]);
	// Valid edges: appends one past the end, clamped splices, root edits, index keys of objects, reserved data.
	add(doc(), ["s", ["arr", 3], 9]);
	add(doc(), ["s", ["e", 0], 9], ["s", ["e", 1], 8]);
	add(doc(), ["p", ["arr"], 9, 0, [7, 8]]);
	add(doc(), ["p", ["arr"], 1, 1e9, []]);
	add(doc(), ["p", ["arr"], 3, 0, [[1, 2]]]);
	add(doc(), ["p", ["e"], 0, 0, Array.from({ length: 50 }, (_, index) => index)]);
	add([1, 2, 3], ["p", [], 1, 1, [4]], ["m", [], [2, 0, 1]]);
	add([1, 2, 3], ["p", [], 0, 3, []], ["s", [0], "x"]);
	add(doc(), ["m", ["e"], []]);
	add(doc(), ["a", ["o", 1], "!"], ["s", ["o", 2], "two"], ["d", ["o", "1"]]);
	add(doc(), ["s", ["o", "10"], 1], ["s", ["o", "9"], 2], ["s", ["o", "b"], 3], ["d", ["o", "x"]], ["s", ["o", "x"], 4]);
	add(doc(), ["d", ["missing"]], ["d", ["o", "missing"]]);
	add(doc(), ["s", ["v"], JSON.parse('{"__proto__":{"z":1},"constructor":2}')]);
	add(JSON.parse('{"__proto__":{"z":1},"n":1}'), ["s", ["n"], 2]);
	add(JSON.parse('{"__proto__":{"z":1},"n":1}'), ["s", ["__proto__", "z"], 2]);
	add(doc(), ["t", ["s"], 2], ["a", ["s"], "😀"]);
	add(doc(), ["t", ["s"], 9]);
	add(doc(), ["t", ["a"], 0], ["t", ["a"], 4]);
	add(doc(), ["a", ["arr", 1], "!"], ["t", ["arr", 1], 1]);
	add(doc(), ["r", { fresh: [] }], ["p", ["fresh"], 0, 0, [1]], ["s", ["fresh", 1], 2]);
	add(null, ["r", "x"], ["r", null]);
	add(doc(), ["s", ["o", "x"], 1.5], ["s", ["o", "y"], -0], ["s", ["o", "z"], 1e21]);
	return out;
}

/** The result of `apply` and `applyImmutable` on detached copies, as a corpus record. */
function applyRecord({ base, ops }) {
	let expected;
	let error;
	let immutable;
	let immutableError;
	try {
		expected = apply(clone(base), clone(ops));
	} catch (thrown) {
		error = thrown.message;
	}
	try {
		immutable = applyImmutable(clone(base), clone(ops));
	} catch (thrown) {
		immutableError = thrown.message;
	}
	if ((error === undefined) !== (immutableError === undefined)) {
		throw new Error(`apply and applyImmutable disagree: ${JSON.stringify({ base, ops, error, immutableError })}`);
	}
	if (error === undefined) {
		if (JSON.stringify(expected) !== JSON.stringify(immutable)) throw new Error(`results differ: ${JSON.stringify(ops)}`);
		return { base, ops, expected: expected === undefined ? null : expected };
	}
	return { base, ops, error, immutableError };
}

function applyCases() {
	const cases = targetedCases();
	while (cases.length < APPLY_CASES + 20) {
		const base = root();
		const ops = validBatch(base);
		if (ops.length > 0 && chance(0.25)) {
			const at = int(ops.length);
			ops[at] = pick(CORRUPTIONS)(ops[at]);
		}
		cases.push({ base, ops });
	}
	const records = cases.map(applyRecord).filter((record) => wellFormed(record.expected));
	return records.slice(0, APPLY_CASES);
}

/** False when a corrupted op's `t` count split a surrogate pair: half a pair has no Rust string. */
function wellFormed(value) {
	if (typeof value === "string") return value.isWellFormed();
	if (value === null || typeof value !== "object") return true;
	return Object.entries(value).every(([key, child]) => key.isWellFormed() && wellFormed(child));
}

// ─── wire-cases ──────────────────────────────────────────────────────────────

/** A pool of paths for one stream: a few, so paths repeat, or many, so ids climb. */
function pathPool() {
	const segment = () => (chance(0.3) ? int(5) : pick(KEYS));
	const size = chance(0.15) ? 30 + int(30) : 1 + int(6);
	return Array.from({ length: size }, () => Array.from({ length: 1 + int(3) }, segment));
}

/** Payloads that make short forms look like long ones: numbers where a path id goes, arrays where a path goes. */
function lookalike() {
	return pick([int(5), [int(5)], ["a"], [], [0, 1], value(1)]);
}

function wireOp(pool) {
	const path = pick(pool);
	switch (int(8)) {
		case 0:
			return ["s", path, chance(0.5) ? lookalike() : value(1)];
		case 1:
			return ["d", path];
		case 2:
			return ["a", path, pick(TEXTS)];
		case 3:
			return ["t", path, int(5)];
		case 4:
			return ["p", chance(0.2) ? [] : path, int(5), int(3), Array.from({ length: int(3) }, lookalike)];
		case 5:
			return ["m", chance(0.2) ? [] : path, pick([[], [0], [1, 0], [2, 0, 1]])];
		case 6:
			return chance(0.3) ? ["r", value(1)] : ["s", path, int(3)];
		default:
			return ["a", path, "x"];
	}
}

function wireRecord() {
	const pool = pathPool();
	const batches = Array.from({ length: 1 + int(5) }, () => {
		// A large pool gets long batches, so most of its paths come back and the ids climb.
		const ops = Array.from({ length: pool.length > 10 ? 2 * pool.length : 1 + int(8) }, () => wireOp(pool));
		// Runs of one path: the short forms.
		if (chance(0.5)) {
			const path = pick(pool);
			for (let count = 1 + int(4); count > 0; count--) ops.push(["a", path, pick(TEXTS)], ["t", path, int(3)]);
		}
		return ops;
	});
	const enc = encoder();
	const dec = decoder();
	const wire = batches.map((batch) => JSON.parse(JSON.stringify(enc.encode(clone(batch)))));
	const decoded = wire.map((batch) => dec.decode(clone(batch)));
	if (JSON.stringify(decoded) !== JSON.stringify(batches)) throw new Error("wire round trip failed");
	return { ops: batches, wire };
}

/** Invalid wire batches: every decoder error, after valid batches that define ids or reset them. */
function invalidWire() {
	const bad = [
		[null],
		[5],
		[[]],
		[{}],
		[["ZZZ", ["a"]]],
		[[7]],
		[["r"]],
		[["r", 1, 2]],
		[["s"]],
		[["s", 1, 2, 3]],
		[["s", -1, 1]],
		[["s", 1.5, 1]],
		[["s", "a", 1]],
		[["s", null, 1]],
		[["s", ["__proto__"], 1]],
		[["s", [-1], 1]],
		[["s", [], 1]],
		[["d", 1, 2]],
		[["d", "a"]],
		[["d", []]],
		[["a"]],
		[["a", 1]],
		[["a", ["a"], 1]],
		[["a", ["a"], "x", 1]],
		[["a", "x", "y"]],
		[["t"]],
		[["t", -1]],
		[["t", 1.5]],
		[["t", ["a"], -1]],
		[["t", ["a"], "1"]],
		[["t", ["a"], 1, 2]],
		[["p", 0, 0]],
		[["p", 0, 0, [], 1, 2]],
		[["p", -1, 0, []]],
		[["p", 0, -1, []]],
		[["p", 0, 0, "x"]],
		[["p", ["a"], 0.5, 0, []]],
		[["p", "a", 0, 0, []]],
		[["p", ["constructor"], 0, 0, []]],
		[["m"]],
		[["m", [0], [0], 1]],
		[["m", [0, 0]]],
		[["m", ["a"], "x"]],
		[["m", "a", [0]]],
		[["m", ["__proto__"], [0]]],
		[["#"]],
		[["#", 0]],
		[["#", -1, ["a"]]],
		[["#", 1.5, ["a"]]],
		[["#", 0, "a"]],
		[["#", 0, ["prototype"]]],
		[["#", 0, [null]]],
		[["#", 0, ["a"], 1]],
		[["s", 0, 1]],
		[["a", "x"]],
		[["d"]],
		[["#", 0, []], ["s", 0, 1]],
		[["#", 0, []], ["p", 0, 0, 0, []], ["s", 1]],
		[["#", 0, ["a"]], ["s", 1, 1]],
		[["s", ["a"], 1], ["r", {}], ["s", 2]],
	];
	const out = [];
	// A spliced-in batch that happens to decode (an id defined before it, a short form after a path) is skipped.
	const record = (wire) => {
		const dec = decoder();
		try {
			for (const batch of wire) dec.decode(clone(batch));
		} catch (thrown) {
			out.push({ wire, error: thrown.message });
		}
	};
	for (const batch of bad) record([batch]);
	// Ids from an earlier batch resolve; a base batch clears them; the short form never spans batches.
	for (const batch of bad) record([[["#", 0, ["a"]], ["s", 0, 1]], batch]);
	record([[["#", 0, ["a"]], ["s", 0, 1]], [["r", {}]], [["s", 0, 2]]]);
	record([[["#", 0, ["a"]], ["s", 0, 1]], [["r", {}], ["s", 0, 2]]]);
	record([[["s", ["a"], 1]], [["s", 2]]]);
	while (out.length < 170) {
		const pool = pathPool();
		const ops = Array.from({ length: 1 + int(5) }, () => wireOp(pool));
		const wire = encoder().encode(ops);
		const at = int(wire.length + 1);
		record([[...wire.slice(0, at), ...pick(bad), ...wire.slice(at)]]);
	}
	return out;
}

function wireCases() {
	const out = [];
	while (out.length < WIRE_CASES - 170) out.push(wireRecord());
	return [...out, ...invalidWire()];
}

// ─── overlap-cases ───────────────────────────────────────────────────────────

const ALPHABETS = ["ab", "abc", "a😀", "xy日", "aaaa", "ab😀é", "0123456789"];

function randomText(alphabet, length) {
	const chars = [...alphabet];
	return Array.from({ length }, () => pick(chars)).join("");
}

function overlapCases() {
	const out = [];
	while (out.length < OVERLAP_CASES) {
		const alphabet = pick(ALPHABETS);
		const a = randomText(alphabet, int(4) === 0 ? int(4) : int(120));
		let b;
		if (chance(0.6) && a.length > 0) {
			// A suffix of `a` and more: what a rolling window or a re-sent chunk produces.
			const chars = [...a];
			b = chars.slice(chars.length - 1 - int(chars.length)).join("") + randomText(alphabet, int(20));
		} else b = randomText(alphabet, int(40));
		const scan = pick([0, 1, 3, 10, 64, 65_536, int(150)]);
		const probe = pick([1, 2, 4, 8, 64, 1 + int(100)]);
		const max = pick([0, 1, 2, 8, 1 + int(10)]);
		out.push({ a, b, scan, probe, max, n: overlap(a, b, scan, probe, max) });
	}
	return out;
}

function write(name, records) {
	writeFileSync(join(here, name), records.map((record) => `${JSON.stringify(record)}\n`).join(""));
}

write("apply-cases.jsonl", applyCases());
write("wire-cases.jsonl", wireCases());
write("overlap-cases.jsonl", overlapCases());

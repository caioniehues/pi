// Generates the tracker cross-apply corpus from the TS tracker in packages/chord/src/delta/tracker.ts.
// Run from anywhere with `node rust/fixtures/chord-delta/gen-tracker.mjs` (Node 26 strips the modules' types). The seed
// is fixed, so the output is reproducible. Writes `tracker-scripts.jsonl` next to this file, one record per script:
//   {"base", "changes": [{"steps": [step, ...], "value"}, ...]}
// where `value` is what the TS tracker's prepared value was after the change's steps (JSON.stringify, so key order is
// JS order). A step is one draft edit on the change's draft; `error` is the message the TS draft threw for it (the draft
// is unchanged then). Steps: set, unset (assign undefined), delete, append (appendText), push, pop, shift, unshift,
// splice, reverse, sort (by JSON.stringify, UTF-16 order), fill, copyWithin, length; a change may instead be one
// replace step (prepareReplace). The Rust test `crates/pi-chord/tests/tracker_corpus.rs` runs every script and writes
// what its tracker prepared to `tracker-out.jsonl`, which `check-tracker-out.mjs` replays with the TS appliers.
// Strings hold no lone surrogates: a Rust string cannot hold half a pair.

import { writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { appendText, track } from "../../../packages/chord/src/delta/index.ts";

const SCRIPTS = 200;
const here = dirname(fileURLToPath(import.meta.url));

// sfc32, seeded apart from gen.mjs.
let sa = 0x6a09e667;
let sb = 0xbb67ae85;
let sc = 0x3c6ef372;
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
const snapshot = (value) => JSON.parse(JSON.stringify(value));

const TEXTS = ["", "a", "bc", "hello", "é", "😀", "a😀b", "日本語", " ", '"q"', "\\", "\n\t", "x".repeat(20)];
// Keys JS orders first (canonical indices), look-alikes, separators, a NUL and reserved names.
const KEYS = ["a", "b", "c", "text", "list", "0", "1", "10", "01", "a.b", "x y", "a\u0000b", "é", "length", "__proto__"];
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
	// JSON.parse, so `__proto__` is an own key as it is for every document the tracker holds.
	const keys = Array.from({ length: int(4) }, () => pick(KEYS));
	return JSON.parse(`{${keys.map((key) => `${JSON.stringify(key)}:${JSON.stringify(value(depth + 1))}`).join(",")}}`);
}

function root() {
	const object = {};
	for (let count = 1 + int(5); count > 0; count--) object[pick(KEYS.slice(0, -1))] = value(1);
	if (chance(0.3)) object.list = Array.from({ length: int(8) }, () => value(1));
	if (chance(0.3)) object.text = pick(TEXTS) + pick(TEXTS);
	return object;
}

/** Every container of `node` with its path, the root first. */
function containers(node, path = [], out = []) {
	if (node === null || typeof node !== "object") return out;
	out.push([path, node]);
	const keys = Array.isArray(node) ? node.map((_, index) => index) : Object.keys(node);
	for (const key of keys) containers(node[key], [...path, key], out);
	return out;
}

/** Every string member of an object of `node`, as the path of the string. */
function stringMembers(node) {
	const out = [];
	for (const [path, container] of containers(node)) {
		if (Array.isArray(container)) continue;
		for (const key of Object.keys(container)) if (typeof container[key] === "string") out.push([...path, key]);
	}
	return out;
}

const items = () => Array.from({ length: int(3) }, () => value(1));
const relative = (length) => int(length + 3) - 1 - (chance(0.2) ? length : 0);

/** One step against the current draft contents, mostly valid, sometimes one the TS draft refuses. */
function step(current) {
	const strings = stringMembers(current);
	if (strings.length > 0 && chance(0.15)) {
		return { op: "append", path: pick(strings), text: pick(TEXTS.slice(1)) };
	}
	const [path, node] = pick(containers(current));
	if (!Array.isArray(node)) {
		const keys = Object.keys(node);
		const key = chance(0.6) && keys.length > 0 ? pick(keys) : pick(KEYS);
		switch (int(6)) {
			case 0:
				return { op: "delete", path: [...path, key] };
			case 1:
				return { op: "unset", path: [...path, key] };
			case 2:
				return { op: "append", path: [...path, key], text: pick(TEXTS.slice(1)) };
			default:
				return { op: "set", path: [...path, key], value: value(1) };
		}
	}
	const length = node.length;
	switch (int(15)) {
		case 0:
			return { op: "set", path: [...path, int(length + 2)], value: value(1) };
		case 1:
			return { op: "push", path, items: items() };
		case 2:
			return { op: "pop", path };
		case 3:
			return { op: "shift", path };
		case 4:
			return { op: "unshift", path, items: items() };
		case 5:
			// `remove: null` is the one-argument form, which inserts nothing.
			return chance(0.2)
				? { op: "splice", path, start: relative(length), remove: null, items: [] }
				: { op: "splice", path, start: relative(length), remove: int(3), items: items() };
		case 6:
			return { op: "reverse", path };
		case 7:
			return { op: "sort", path };
		case 8:
			return { op: "fill", path, value: value(1), start: relative(length), end: chance(0.3) ? null : relative(length) };
		case 9:
			return {
				op: "copyWithin",
				path,
				target: relative(length),
				start: relative(length),
				end: chance(0.3) ? null : relative(length),
			};
		case 10:
			return { op: "length", path, length: int(length + 3) };
		case 11:
			return { op: "delete", path: [...path, int(length + 1)] };
		default:
			return length > 0 ? { op: "set", path: [...path, int(length)], value: value(1) } : { op: "push", path, items: [1] };
	}
}

function resolve(draft, path) {
	let node = draft;
	for (const segment of path) node = node[segment];
	return node;
}

const byJson = (left, right) => {
	const a = JSON.stringify(left);
	const b = JSON.stringify(right);
	return a < b ? -1 : a > b ? 1 : 0;
};

/** Run `step` on the TS draft. */
function run(draft, { op, path, ...args }) {
	const parent = () => resolve(draft, path.slice(0, -1));
	const leaf = path.at(-1);
	switch (op) {
		case "set":
			parent()[leaf] = args.value;
			return;
		case "unset":
			parent()[leaf] = undefined;
			return;
		case "delete":
			delete parent()[leaf];
			return;
		case "append":
			appendText(parent(), leaf, args.text);
			return;
		case "push":
			resolve(draft, path).push(...args.items);
			return;
		case "pop":
			resolve(draft, path).pop();
			return;
		case "shift":
			resolve(draft, path).shift();
			return;
		case "unshift":
			resolve(draft, path).unshift(...args.items);
			return;
		case "splice":
			if (args.remove === null) resolve(draft, path).splice(args.start);
			else resolve(draft, path).splice(args.start, args.remove, ...args.items);
			return;
		case "reverse":
			resolve(draft, path).reverse();
			return;
		case "sort":
			resolve(draft, path).sort(byJson);
			return;
		case "fill":
			resolve(draft, path).fill(args.value, args.start, args.end ?? undefined);
			return;
		case "copyWithin":
			resolve(draft, path).copyWithin(args.target, args.start, args.end ?? undefined);
			return;
		case "length":
			resolve(draft, path).length = args.length;
			return;
	}
	throw new Error(`unknown step ${op}`);
}

const lines = [];
for (let script = 0; script < SCRIPTS; script++) {
	const base = root();
	const tracker = track(snapshot(base));
	const changes = [];
	for (let count = 1 + int(4); count > 0; count--) {
		if (chance(0.08)) {
			const replacement = root();
			tracker.adopt(tracker.prepareReplace(snapshot(replacement)));
			changes.push({ steps: [{ op: "replace", value: replacement }], value: snapshot(tracker.value) });
			continue;
		}
		const change = tracker.beginChange();
		const steps = [];
		for (let count = 1 + int(8); count > 0; count--) {
			const next = step(snapshot(change.state));
			try {
				run(change.state, next);
			} catch (error) {
				next.error = error.message;
			}
			steps.push(next);
		}
		const prepared = change.prepare();
		tracker.adopt(prepared);
		changes.push({ steps, value: snapshot(prepared.value) });
	}
	lines.push(JSON.stringify({ base, changes }));
}
writeFileSync(join(here, "tracker-scripts.jsonl"), `${lines.join("\n")}\n`);
console.log(`wrote ${lines.length} tracker scripts`);

// Generates the pi-json fixture corpus from Node's own JSON.stringify, String(n) and Date.prototype.toISOString.
// Run from anywhere with `node rust/fixtures/json/gen.mjs` (plain Node, no dependencies); the seed is fixed, so the
// output is reproducible. Writes, next to this file:
//   cases.jsonl    {"value": <v>, "compact": JSON.stringify(v), "pretty2": JSON.stringify(v, null, 2)}
//   numbers.jsonl  {"n": <number>, "s": String(n)}
//   dates.jsonl    {"ms": <epoch ms>, "iso": new Date(ms).toISOString()}, then
//                  {"parse": <text>, "ms": Date.parse(text) or null}
// `value` is written as raw JSON text in generation order, so it can hold what a JS object reorders (integer-like
// keys), duplicate keys and number lexemes a double cannot hold exactly; the expected strings are what Node prints
// after `JSON.parse` of that same text, which is what the TS implementation would write.

import { writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const CASES = 5000;
const RANDOM_NUMBERS = 120;
const RANDOM_DATES = 150;
const here = dirname(fileURLToPath(import.meta.url));

// sfc32: a small seeded generator; Math.random cannot be seeded.
let a = 0x9e3779b9;
let b = 0x243f6a88;
let c = 0xb7e15162;
let d = 1;
function nextU32() {
	a >>>= 0;
	b >>>= 0;
	c >>>= 0;
	d >>>= 0;
	const t = (((a + b) | 0) + d) | 0;
	d = (d + 1) | 0;
	a = b ^ (b >>> 9);
	b = (c + (c << 3)) | 0;
	c = (c << 21) | (c >>> 11);
	c = (c + t) | 0;
	return t >>> 0;
}
for (let i = 0; i < 20; i++) nextU32();

function int(n) {
	return nextU32() % n;
}

function pick(list) {
	return list[int(list.length)];
}

const doubleView = new DataView(new ArrayBuffer(8));

function randomBitsDouble() {
	for (;;) {
		doubleView.setUint32(0, nextU32());
		doubleView.setUint32(4, nextU32());
		const value = doubleView.getFloat64(0);
		if (Number.isFinite(value)) return value;
	}
}

// Every branch of Number::toString (ECMA-262 6.1.6.1.20), with k digits and exponent n:
// integer (k <= n <= 21), point inside (0 < n <= 21), leading zeros (-6 < n <= 0), and the exponent forms with one
// digit and with several, positive and negative exponents.
const SPECIAL_NUMBERS = [
	0,
	-0,
	1,
	-1,
	7,
	42,
	-273,
	100,
	1e20,
	1e21,
	-1e21,
	1e22,
	999999999999999900000,
	123456789012345680000,
	2 ** 53 - 1,
	2 ** 53,
	2 ** 53 + 2,
	-(2 ** 53),
	2 ** 60,
	2 ** 64,
	2 ** 63,
	1.5,
	-2.25,
	0.1,
	0.2,
	0.1 + 0.2,
	1 / 3,
	2 / 3,
	Math.PI,
	Math.E,
	123.456,
	123456789.12345679,
	1234567890123456.8,
	12345678901234567000,
	0.5,
	0.001,
	0.000001,
	0.0000015,
	-0.000001,
	1e-6,
	1e-7,
	1.5e-7,
	-1e-7,
	1.2345e-10,
	5e-324,
	-5e-324,
	1e-323,
	2.2250738585072014e-308,
	2.225073858507201e-308,
	1.7976931348623157e308,
	-1.7976931348623157e308,
	1e300,
	1.5e300,
	1e100,
	4.35e100,
	1e-100,
	9.5367431640625e-7,
	1.7e21,
	1.23e25,
	2 ** -1074 * 3,
	0.30000000000000004,
	100.5,
	4294967295,
	4294967296,
	-4294967297,
	1e15,
	1e16,
	1e17,
	9.999999999999999e20,
	1.0000000000000001e21,
];

function randomNumber() {
	switch (int(6)) {
		case 0:
			return randomBitsDouble();
		case 1:
			// Small integers, the common case.
			return int(2001) - 1000;
		case 2:
			// Integers around 2^53 and 1e21.
			return pick([2 ** 53, 1e21, 2 ** 64]) + (int(2001) - 1000) * pick([1, 2, 1024, 65536]);
		case 3: {
			// Mantissa of a few digits at an exponent anywhere in range.
			const digits = int(10 ** (1 + int(9)));
			const value = Number(`${digits}e${int(640) - 330}`);
			return Number.isFinite(value) ? value : 0;
		}
		case 4:
			return (nextU32() / 2 ** 32) * 10 ** (int(30) - 10) * (int(2) === 0 ? 1 : -1);
		default:
			return pick(SPECIAL_NUMBERS);
	}
}

// Lexemes JSON.parse reads as a different double than written: past 2^53, past u64, past i64, and other spellings.
const ODD_NUMBER_LEXEMES = [
	"-0",
	"-0.0",
	"0.0",
	"1.0",
	"1E21",
	"1e+21",
	"1E-7",
	"9007199254740993",
	"9007199254740995",
	"-9007199254740993",
	"18446744073709551615",
	"18446744073709551616",
	"9223372036854775807",
	"-9223372036854775808",
	"-9223372036854775809",
	"100000000000000000000",
	"1000000000000000000000",
	"123456789012345678901234567890",
	"0.1000000000000000055511151231257827",
	"2.4703282292062328e-324",
	"1e-400",
	"12.50",
];

function numberLexeme() {
	if (int(10) === 0) return pick(ODD_NUMBER_LEXEMES);
	const value = randomNumber();
	return Object.is(value, -0) ? "-0" : String(value);
}

// Code points by escape class: what JSON.stringify escapes short, as \u00XX, and leaves raw.
const SHORT_ESCAPES = ['"', "\\", "\b", "\f", "\n", "\r", "\t"];
const RAW_SPECIALS = ["/", "\u007f", "\u0080", " ", " ", " ", "﻿", "�", "￿", ""];
const NON_ASCII = ["é", "ß", "中", "文", "Ω", "ﬁ", "́", "ŉ"];
const NON_BMP = ["😀", "𝄞", "🏳️‍🌈", "\u{10ffff}", "\u{10000}", "𠜎"];

function randomChar() {
	switch (int(8)) {
		case 0:
			return pick(SHORT_ESCAPES);
		case 1:
			return String.fromCharCode(int(0x20));
		case 2:
			return pick(RAW_SPECIALS);
		case 3:
			return pick(NON_ASCII);
		case 4:
			return pick(NON_BMP);
		default:
			return String.fromCharCode(0x20 + int(0x5f));
	}
}

function randomString() {
	const length = pick([0, 1, 2, 3, 5, 8, 13, 30]);
	let text = "";
	for (let i = 0; i < length; i++) text += randomChar();
	return text;
}

// Keys a JS object reorders (canonical array indices first, ascending) and look-alikes it keeps in insertion order.
const INDEX_LIKE_KEYS = ["0", "1", "2", "10", "9", "100", "4294967294", "4294967295", "4294967296", "01", "-1", "1.5", "1e3", " 1"];

function randomKey() {
	return int(4) === 0 ? pick(INDEX_LIKE_KEYS) : randomString();
}

function randomValue(depth) {
	const kinds = depth >= 5 ? 5 : 9;
	switch (int(kinds)) {
		case 0:
			return "null";
		case 1:
			return pick(["true", "false"]);
		case 2:
		case 3:
			return numberLexeme();
		case 4:
			return JSON.stringify(randomString());
		case 5:
		case 6:
			return randomArray(depth);
		default:
			return randomObject(depth);
	}
}

function randomArray(depth) {
	const length = pick([0, 0, 1, 2, 3, 4, 7]);
	const items = [];
	for (let i = 0; i < length; i++) items.push(randomValue(depth + 1));
	return `[${items.join(",")}]`;
}

function randomObject(depth) {
	const length = pick([0, 0, 1, 2, 3, 4, 6]);
	const keys = [];
	for (let i = 0; i < length; i++) {
		const key = randomKey();
		// Duplicate keys only now and then: JSON.parse keeps the first position and the last value.
		if (keys.includes(key) && int(8) !== 0) continue;
		keys.push(key);
	}
	const entries = keys.map((key) => `${JSON.stringify(key)}:${randomValue(depth + 1)}`);
	return `{${entries.join(",")}}`;
}

function caseLine(raw) {
	const value = JSON.parse(raw);
	return `{"value":${raw},"compact":${JSON.stringify(JSON.stringify(value))},"pretty2":${JSON.stringify(JSON.stringify(value, null, 2))}}`;
}

function numberLine(n) {
	const lexeme = Object.is(n, -0) ? "-0" : String(n);
	return `{"n":${lexeme},"s":${JSON.stringify(String(n))}}`;
}

// The Date range is +-8.64e15 ms; years 0..9999 print with four digits, others as a signed six-digit year.
const SPECIAL_DATES = [
	0,
	-1,
	1,
	999,
	-999,
	1730851200000,
	1730851200123,
	951782400000,
	-62167219200000,
	-62167219200001,
	-62198755200000,
	253402300799999,
	253402300800000,
	-8.64e15,
	8.64e15,
	-2208988800000,
	4102444800000,
	-377705116800000,
	68169600000,
];

function randomDate() {
	switch (int(3)) {
		case 0:
			return Math.floor((nextU32() / 2 ** 32) * 2 * 8.64e15 - 8.64e15);
		case 1:
			return 1.6e12 + nextU32() * 100 + int(1000);
		default:
			return -62198755200000 + nextU32() * 80000 + int(1000);
	}
}

function dateLine(ms) {
	return `{"ms":${ms},"iso":${JSON.stringify(new Date(ms).toISOString())}}`;
}

// Date.parse: texts it reads (half) and rejects (half). No offset-less date-times: Node reads those as local time,
// so their result would depend on the generating machine's time zone.
const PARSE_VALID = [
	"2024",
	"2024-02",
	"2024-11-06",
	"2024-02-30",
	"2024-02-31",
	"2023-02-29T00:00Z",
	"2024-01-01T24:00:00Z",
	"2024-01-01T24:00Z",
	"2024-12-31T24:00:00.000Z",
	"2024-01-01T12:00Z",
	"2024-01-01T12:00:00.1Z",
	"2024-01-01T12:00:00.12Z",
	"2024-01-01T12:00:00.123456789Z",
	"1969-12-31T23:59:59.9999Z",
	"2024-01-01T12:00:00+01:00",
	"2024-01-01T12:00:00-23:59",
	"2024-01-01T00:00:00.000+23:59",
	"2024-01-01T12:00:00+0100",
	"2024-01-01t12:00:00z",
	"2024-01-01 12:00:00Z",
	"2024-01-01z",
	"2024T12:00Z",
	"2024-01T12:00+0100",
	"+002024-01-01T00:00:00Z",
	"-000001-01-01T00:00:00Z",
	"+000000-01-01T00:00:00Z",
	"+275760-09-13T00:00:00.000Z",
	"-271821-04-19T23:00:00-01:00",
	"+010000-01-01T00:00Z",
	"9999-12-31T23:59:59.999Z",
];
const PARSE_INVALID = [
	"2024-02-32",
	"2024-13-01",
	"2024-00-01",
	"2024-01-00",
	"2024-01-01T24:00:01Z",
	"2024-01-01T24:00:00.001Z",
	"2024-01-01T23:59:60Z",
	"2024-01-01T25:00Z",
	"2024-01-01T23:60Z",
	"2024-01-01T12:00:00.Z",
	"2024-01-01T12:00:00,5Z",
	"2024-01-01T12:00:00+01",
	"2024-01-01T12:00:00+24:00",
	"2024-01-01T12:00:00+23:60",
	"20240101",
	"20240101T120000Z",
	"2024-01-01T12:00:00Z[UTC]",
	"-000000-01-01T00:00:00Z",
	"+275760-09-13T00:00:00.001Z",
	"-271821-04-19T23:59:59.999Z",
	"+275760-09-13T00:00:00-01:00",
	"2024-01-01T12:00:00 Z",
	"2024-01-01T12:00:00+1:00",
	"2024-01-01T12:00:00Z ",
	"2024-01-01T12Z",
	"2024-01-01T1:00Z",
	"2024-01-01T12:00:5Z",
	"2024-01-01+01:00",
	"2024-01-01T12:00:00+01:00:00",
	"2024ZT12:00Z",
];

function parseLine(text, valid) {
	const ms = Date.parse(text);
	// The lists are claims about Node; a wrong claim stops the generator instead of landing in the corpus.
	if (Number.isNaN(ms) === valid) throw new Error(`Date.parse(${JSON.stringify(text)}) is ${ms}`);
	return `{"parse":${JSON.stringify(text)},"ms":${Number.isNaN(ms) ? "null" : ms}}`;
}

const cases = [];
for (let i = 0; i < CASES; i++) cases.push(caseLine(randomValue(0)));
writeFileSync(join(here, "cases.jsonl"), `${cases.join("\n")}\n`);

const numbers = [...SPECIAL_NUMBERS];
for (let i = 0; i < RANDOM_NUMBERS; i++) numbers.push(randomNumber());
const extraNumbers = 200 - numbers.length;
for (let i = 0; i < extraNumbers; i++) numbers.push(randomBitsDouble());
writeFileSync(join(here, "numbers.jsonl"), `${numbers.map(numberLine).join("\n")}\n`);

const dates = [...SPECIAL_DATES];
for (let i = 0; i < RANDOM_DATES; i++) dates.push(randomDate());
const parses = [...PARSE_VALID.map((text) => parseLine(text, true)), ...PARSE_INVALID.map((text) => parseLine(text, false))];
writeFileSync(join(here, "dates.jsonl"), `${[...dates.map(dateLine), ...parses].join("\n")}\n`);

console.log(`cases ${cases.length}, numbers ${numbers.length}, dates ${dates.length}, parses ${parses.length}`);

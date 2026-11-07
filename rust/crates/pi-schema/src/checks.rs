//! Check factories (TS `checks.ts`): each returns a check that captures only its arguments, never the compiler, so a
//! compiled schema keeps no compile-time state alive. A check runs on a borrowed value with a `Ctx` and allocates
//! nothing on its own; the regex engine behind `pattern`, `propertyNames` (whose check needs the key as a value), the
//! annotations of `unevaluated*` and the dynamic scope of `$dynamicRef` are the exceptions.
//!
//! Decided differences from TS: the value model is JSON, so `bytes`, `bigint` and `undefined` match no value, numbers
//! are always finite, and there are no `Object.prototype` members to guard against. TS's leaf codes (a V8 inlining
//! device) become plain closures. Checks walk an object's members in insertion order, not JS key order: a boolean
//! result does not depend on it, and JS order would allocate for objects with index keys; the explainer and the
//! coercer, whose results do depend on it, walk JS order (`pi_json::js_key_order`).

use crate::annotations::AnnotatingCheck;
use pi_json::JsonValue;
use regress::Regex;
use serde_json::{Map, Number};
use std::collections::HashSet;
use std::sync::Arc;

/// The state of one check call: the slots recursive references go through and the dynamic scope.
pub(crate) struct Ctx<'a> {
	/// The checks of recursive references, by slot.
	pub(crate) slots: &'a [Check],
	/// The annotating checks of recursive references, by slot.
	pub(crate) annotating: &'a [AnnotatingCheck],
	/// The base URIs of the schema resources entered, outermost first (only schemas with `$dynamicRef` push).
	pub(crate) scope: Vec<Arc<str>>,
}

impl<'a> Ctx<'a> {
	/// A context over the slots of one compile; the scope starts empty and allocates only when pushed.
	pub(crate) fn new(slots: &'a [Check], annotating: &'a [AnnotatingCheck]) -> Self {
		Self {
			slots,
			annotating,
			scope: Vec::new(),
		}
	}
}

/// The closure of a check that is not one of the shared shapes.
pub(crate) type CheckFn = dyn Fn(&JsonValue, &mut Ctx<'_>) -> bool + Send + Sync;

/// A compiled boolean check.
#[derive(Clone)]
pub(crate) enum Check {
	/// Every value passes.
	Always,
	/// No value passes.
	Never,
	/// The value has one of the types of the mask (`TYPE_BITS`).
	Types(u16),
	/// A recursive reference: the check in that slot of the context.
	Slot(usize),
	/// Any other check.
	Fn(Arc<CheckFn>),
}

impl Check {
	/// A check from a closure.
	pub(crate) fn new(check: impl Fn(&JsonValue, &mut Ctx<'_>) -> bool + Send + Sync + 'static) -> Self {
		Self::Fn(Arc::new(check))
	}

	/// Whether `value` passes.
	pub(crate) fn run(&self, value: &JsonValue, ctx: &mut Ctx<'_>) -> bool {
		match self {
			Self::Always => true,
			Self::Never => false,
			Self::Types(mask) => type_bits(value) & mask != 0,
			Self::Slot(index) => {
				let slots = ctx.slots;
				slots[*index].run(value, ctx)
			}
			Self::Fn(check) => check(value, ctx),
		}
	}
}

/// The type bits of `type` names. `bytes`, `bigint` and `undefined` (TS extensions for values that never pass
/// through JSON) are known names without bits: no JSON value has those types.
const TYPE_BITS: [(&str, u16); 10] = [
	("string", 1),
	("number", 2),
	("integer", 4),
	("boolean", 8),
	("null", 16),
	("array", 32),
	("object", 64),
	("bytes", 0),
	("bigint", 0),
	("undefined", 0),
];

/// The bits of a known `type` name; `None` for a name JSON Schema does not define.
pub(crate) fn type_bit(name: &str) -> Option<u16> {
	TYPE_BITS.iter().find(|(known, _)| *known == name).map(|&(_, bit)| bit)
}

/// The check of one `type` name, `None` for an unknown name (TS `TYPE_CHECKS.get`).
pub(crate) fn bare_type_check(name: &str) -> Option<Check> {
	type_bit(name).map(mask_check)
}

fn mask_check(mask: u16) -> Check {
	if mask == 0 { Check::Never } else { Check::Types(mask) }
}

/// The type bits of `value`: an integer has the bits of `number` and `integer`.
pub(crate) fn type_bits(value: &JsonValue) -> u16 {
	match value {
		JsonValue::String(_) => 1,
		JsonValue::Number(n) if is_integer(n) => 6,
		JsonValue::Number(_) => 2,
		JsonValue::Bool(_) => 8,
		JsonValue::Null => 16,
		JsonValue::Array(_) => 32,
		JsonValue::Object(_) => 64,
	}
}

/// Whether a JSON number is an integer, as `Number.isInteger` sees the double it parses to (`1.0` is one).
pub(crate) fn is_integer(n: &Number) -> bool {
	n.is_i64() || n.is_u64() || n.as_f64().is_some_and(|f| f.is_finite() && f.fract() == 0.0)
}

/// JS `===` on two numbers: the doubles they parse to, compared exactly.
#[expect(clippy::float_cmp, reason = "JS equality of numbers is exact equality of doubles")]
pub(crate) fn same_number(a: &Number, b: &Number) -> bool {
	number_value(a) == number_value(b)
}

/// A length or a count as a double, to compare with the bounds of a schema (which JSON gives as doubles).
#[expect(clippy::cast_precision_loss, reason = "lengths of JSON values are far below 2^52")]
pub(crate) fn count(n: usize) -> f64 {
	n as f64
}

/// A JSON number as the double JS parses it to.
pub(crate) fn number_value(n: &Number) -> f64 {
	// invariant: without serde_json's `arbitrary_precision` every Number is representable as an f64.
	n.as_f64().unwrap_or(f64::NAN)
}

/// Any of the named types (TS `typeCheck`); one unknown name matches nothing, unknown names in a list add nothing.
pub(crate) fn type_check(types: &[&JsonValue]) -> Check {
	if let [single] = types {
		return single.as_str().and_then(bare_type_check).unwrap_or(Check::Never);
	}
	mask_check(type_mask(types))
}

/// The bits of the named types; unknown names add none.
pub(crate) fn type_mask(types: &[&JsonValue]) -> u16 {
	types
		.iter()
		.filter_map(|t| t.as_str().and_then(type_bit))
		.fold(0, |mask, bit| mask | bit)
}

/// All of `parts`.
pub(crate) fn and(parts: Vec<Check>) -> Check {
	let mut parts: Vec<Check> = parts.into_iter().filter(|p| !matches!(p, Check::Always)).collect();
	match parts.len() {
		0 => Check::Always,
		1 => parts.swap_remove(0),
		2 => {
			let (a, b) = (parts[0].clone(), parts[1].clone());
			Check::new(move |v, ctx| a.run(v, ctx) && b.run(v, ctx))
		}
		_ => Check::new(move |v, ctx| parts.iter().all(|p| p.run(v, ctx))),
	}
}

/// `check` with `uri` pushed onto the dynamic scope while it runs (only for schemas with `$dynamicRef`).
pub(crate) fn scoped_check(uri: &str, check: Check) -> Check {
	let uri: Arc<str> = Arc::from(uri);
	Check::new(move |v, ctx| {
		ctx.scope.push(uri.clone());
		let ok = check.run(v, ctx);
		ctx.scope.pop();
		ok
	})
}

/// `anyOf` whose bare-type branches have the types of `mask`: those by one type test, the others by their checks.
pub(crate) fn masked_any_check(mask: u16, checks: Vec<Check>) -> Check {
	if checks.is_empty() {
		return mask_check(mask);
	}
	let rest = if checks.len() == 1 {
		checks.into_iter().next().unwrap_or(Check::Never)
	} else {
		any_check(checks)
	};
	Check::new(move |v, ctx| type_bits(v) & mask != 0 || rest.run(v, ctx))
}

/// Whether a string's length in code points is within the bounds; JSON Schema counts code points, and a Rust
/// string's characters are exactly those (it cannot hold the unpaired surrogates TS counts one by one).
pub(crate) fn length_ok(s: &str, min: f64, max: f64) -> bool {
	// A UTF-8 string has between len / 4 and len code points, so most strings need no count.
	let bytes = count(s.len());
	if bytes < min {
		return false;
	}
	if bytes <= max && bytes / 4.0 >= min {
		return true;
	}
	let n = count(s.chars().count());
	n >= min && n <= max
}

/// Whether a regular expression matches somewhere in `s` (`RegExp.prototype.test`).
pub(crate) fn regex_test(re: &Regex, s: &str) -> bool {
	re.find(s).is_some()
}

/// The string keywords: length bounds, pattern and format; other types pass unless `strict`.
pub(crate) fn string_check(
	strict: bool,
	min: f64,
	max: f64,
	re: Option<Arc<Regex>>,
	format: Option<crate::types::FormatCheck>,
) -> Check {
	Check::new(move |v, _| {
		let JsonValue::String(s) = v else { return !strict };
		length_ok(s, min, max)
			&& re.as_ref().is_none_or(|re| regex_test(re, s))
			&& format.as_ref().is_none_or(|format| format(s))
	})
}

/// What a number check requires of the value's type (TS `numberCheck`'s mode).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum NumberMode {
	/// Other types pass.
	Any,
	/// A number.
	Number,
	/// An integer.
	Integer,
	/// A bigint, which no JSON value is.
	Bigint,
}

/// Bounds of a number: inclusive and exclusive, infinite when absent.
#[derive(Clone, Copy)]
pub(crate) struct Bounds {
	pub(crate) min: f64,
	pub(crate) max: f64,
	pub(crate) xmin: f64,
	pub(crate) xmax: f64,
}

/// The number keywords.
pub(crate) fn number_check(mode: NumberMode, bounds: Bounds, multiple_of: Option<f64>) -> Check {
	if mode == NumberMode::Bigint {
		return Check::Never;
	}
	Check::new(move |v, _| {
		let JsonValue::Number(n) = v else {
			return mode == NumberMode::Any;
		};
		if mode == NumberMode::Integer && !is_integer(n) {
			return false;
		}
		let x = number_value(n);
		x >= bounds.min
			&& x <= bounds.max
			&& x > bounds.xmin
			&& x < bounds.xmax
			&& multiple_of.is_none_or(|d| is_multiple(x, d))
	})
}

/// Whether `v` is a multiple of `d`, exactly for decimals: 0.0075 / 0.0001 is 74.99999999999999 in floating point,
/// so the numbers are compared as the decimals JS prints them as.
pub(crate) fn is_multiple(v: f64, d: f64) -> bool {
	let q = v / d;
	if q.is_finite() && q.fract() == 0.0 {
		return true;
	}
	if !q.is_finite() {
		// TS parity: JS `%` is fmod, as Rust's.
		return v % d == 0.0;
	}
	let (vc, ve) = decimal(v);
	let (dc, de) = decimal(d);
	if dc == 0 {
		return false;
	}
	if ve >= de {
		// (vc * 10^(ve - de)) % dc, without the big integer TS builds.
		let power = pow_mod(10, (ve - de).unsigned_abs(), dc);
		mul_mod(vc % dc, power, dc) == 0
	} else {
		// vc % (dc * 10^(de - ve)): a divisor larger than vc leaves vc, which is not 0 (v is not 0 here).
		let mut divisor = dc;
		for _ in 0..(de - ve) {
			match divisor.checked_mul(10) {
				Some(next) => divisor = next,
				None => return vc == 0,
			}
		}
		vc % divisor == 0
	}
}

/// The digits and the decimal exponent of `|x|`'s shortest round-trip form (the digits JS prints): 0.0075 is
/// (75, -4). Formatted on the stack, so checking `multipleOf` allocates nothing.
fn decimal(x: f64) -> (u128, i32) {
	let mut buffer = ryu::Buffer::new();
	let text = buffer.format_finite(x.abs());
	let (mantissa, exponent) = text.split_once('e').unwrap_or((text, "0"));
	// invariant: ryu writes exponents of at most three digits with a sign.
	let exponent: i32 = exponent.parse().unwrap_or(0);
	let mut coefficient: u128 = 0;
	let mut fraction = 0;
	let mut after_point = false;
	for digit in mantissa.bytes() {
		if digit == b'.' {
			after_point = true;
		} else {
			coefficient = coefficient * 10 + u128::from(digit - b'0');
			fraction += i32::from(after_point);
		}
	}
	(coefficient, exponent - fraction)
}

/// `(a * b) % m` for `a, b < m < 2^126`, by doubling (no overflow).
fn mul_mod(a: u128, b: u128, m: u128) -> u128 {
	let (mut a, mut b, mut out) = (a % m, b, 0u128);
	while b > 0 {
		if b & 1 == 1 {
			out = (out + a) % m;
		}
		a = (a + a) % m;
		b >>= 1;
	}
	out
}

/// `base^exp % m`.
fn pow_mod(base: u128, exp: u32, m: u128) -> u128 {
	let (mut base, mut exp, mut out) = (base % m, exp, 1 % m);
	while exp > 0 {
		if exp & 1 == 1 {
			out = mul_mod(out, base, m);
		}
		base = mul_mod(base, base, m);
		exp >>= 1;
	}
	out
}

/// Whether a combined object or array check also checks the type: not at all, strictly, or passing other types.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Guard {
	/// The caller has checked the type.
	None,
	/// Other types fail.
	Strict,
	/// Other types pass.
	Loose,
}

/// `inner` for objects; other values pass unless `strict`.
pub(crate) fn guard_object(inner: Check, strict: bool) -> Check {
	Check::new(move |v, ctx| if v.is_object() { inner.run(v, ctx) } else { !strict })
}

/// `inner` for arrays; other values pass unless `strict`.
pub(crate) fn guard_array(inner: Check, strict: bool) -> Check {
	Check::new(move |v, ctx| if v.is_array() { inner.run(v, ctx) } else { !strict })
}

/// One declared property of an object check.
pub(crate) struct Entry {
	pub(crate) key: String,
	pub(crate) check: Check,
	pub(crate) required: bool,
}

/// A closed or open object with declared properties only: the value's keys are walked once, each found among the
/// declared names (keys usually come in the declared order, so the entry after the last match is tried first).
pub(crate) fn declared_object(entries: Vec<Entry>, required_count: usize, closed: bool, guard: Guard) -> Check {
	Check::new(move |v, ctx| {
		let Some(o) = v.as_object() else {
			return guard != Guard::Strict;
		};
		let mut required = 0;
		let mut next = 0;
		for (k, x) in o {
			let index = if entries.get(next).is_some_and(|e| e.key == *k) {
				next
			} else if let Some(found) = entries.iter().position(|e| e.key == *k) {
				found
			} else if closed {
				return false;
			} else {
				continue;
			};
			next = index + 1;
			let entry = &entries[index];
			if !entry.check.run(x, ctx) {
				return false;
			}
			if entry.required {
				required += 1;
			}
		}
		required == required_count
	})
}

/// The members of an object check besides its declared properties.
pub(crate) struct OpenMembers {
	/// Required keys that are not declared.
	pub(crate) extra_required: Vec<String>,
	/// `patternProperties`, in order.
	pub(crate) patterns: Vec<(Arc<Regex>, Check)>,
	/// `additionalProperties`, unless it is absent or `true`.
	pub(crate) additional: Option<Check>,
}

/// An object with declared properties, required keys not declared, pattern properties and additional properties.
pub(crate) fn open_object(entries: Vec<Entry>, members: OpenMembers, guard: Guard) -> Check {
	let walks_keys = !members.patterns.is_empty() || members.additional.is_some();
	let known: HashSet<String> = if walks_keys {
		entries.iter().map(|e| e.key.clone()).collect()
	} else {
		HashSet::new()
	};
	Check::new(move |v, ctx| {
		let Some(o) = v.as_object() else {
			return guard != Guard::Strict;
		};
		for entry in &entries {
			let ok = match o.get(&entry.key) {
				Some(x) => entry.check.run(x, ctx),
				None => !entry.required,
			};
			if !ok {
				return false;
			}
		}
		if members.extra_required.iter().any(|k| !o.contains_key(k)) {
			return false;
		}
		if !walks_keys {
			return true;
		}
		for (k, x) in o {
			let mut matched = known.contains(k);
			for (re, check) in &members.patterns {
				if regex_test(re, k) {
					matched = true;
					if !check.run(x, ctx) {
						return false;
					}
				}
			}
			if !matched && members.additional.as_ref().is_some_and(|extra| !extra.run(x, ctx)) {
				return false;
			}
		}
		true
	})
}

/// Every property name passes `check` (`v` is an object).
pub(crate) fn property_names_check(check: Check) -> Check {
	Check::new(move |v, ctx| {
		let Some(o) = v.as_object() else { return true };
		o.keys().all(|k| check.run(&JsonValue::String(k.clone()), ctx))
	})
}

/// `minProperties` and `maxProperties` (`v` is an object).
pub(crate) fn property_count_check(min: f64, max: f64) -> Check {
	Check::new(move |v, _| {
		let n = count(v.as_object().map_or(0, Map::len));
		n >= min && n <= max
	})
}

/// `dependentRequired` (`v` is an object).
pub(crate) fn dependent_required_check(entries: Vec<(String, Vec<String>)>) -> Check {
	Check::new(move |v, _| {
		let Some(o) = v.as_object() else { return true };
		entries
			.iter()
			.all(|(k, list)| !o.contains_key(k) || list.iter().all(|r| o.contains_key(r)))
	})
}

/// `dependentSchemas` (`v` is an object).
pub(crate) fn dependent_schemas_check(entries: Vec<(String, Check)>) -> Check {
	Check::new(move |v, ctx| {
		let Some(o) = v.as_object() else { return true };
		entries.iter().all(|(k, check)| !o.contains_key(k) || check.run(v, ctx))
	})
}

/// The common array: guard, length bounds, prefix items and the items after them, in one closure.
pub(crate) fn list_check(guard: Guard, min: f64, max: f64, prefix: Vec<Check>, rest: Option<Check>) -> Check {
	Check::new(move |v, ctx| {
		let Some(items) = v.as_array() else {
			return guard == Guard::Loose;
		};
		let length = count(items.len());
		if length < min || length > max {
			return false;
		}
		if !items.iter().zip(&prefix).all(|(item, check)| check.run(item, ctx)) {
			return false;
		}
		match &rest {
			None => true,
			Some(rest) => items.iter().skip(prefix.len()).all(|item| rest.run(item, ctx)),
		}
	})
}

/// Prefix items and the items after them (`v` is an array).
pub(crate) fn items_check(prefix: Vec<Check>, rest: Option<Check>) -> Check {
	list_check(Guard::None, 0.0, f64::INFINITY, prefix, rest)
}

/// `minItems` and `maxItems` (`v` is an array).
pub(crate) fn length_check(min: f64, max: f64) -> Check {
	Check::new(move |v, _| {
		let n = count(v.as_array().map_or(0, Vec::len));
		n >= min && n <= max
	})
}

/// `contains` with its bounds (`v` is an array).
pub(crate) fn contains_check(check: Check, min: f64, max: f64) -> Check {
	Check::new(move |v, ctx| contains_ok(&check, v, min, max, ctx))
}

/// Whether the number of items `check` accepts is within the bounds (stops once it is above `max`).
pub(crate) fn contains_ok(check: &Check, v: &JsonValue, min: f64, max: f64, ctx: &mut Ctx<'_>) -> bool {
	let Some(items) = v.as_array() else { return true };
	let mut n = 0.0;
	for item in items {
		if check.run(item, ctx) {
			n += 1.0;
			if n > max {
				return false;
			}
		}
	}
	n >= min
}

/// `uniqueItems` (`v` is an array).
pub(crate) fn unique_check() -> Check {
	Check::new(|v, _| unique_ok(v))
}

/// Whether no two items of an array are equal.
pub(crate) fn unique_ok(v: &JsonValue) -> bool {
	let Some(items) = v.as_array() else { return true };
	items
		.iter()
		.enumerate()
		.all(|(i, item)| items[..i].iter().all(|earlier| !deep_equal(item, earlier)))
}

/// The members of an `enum`: primitives in sets, objects and arrays compared by deep equality.
struct EnumSet {
	strings: HashSet<String>,
	numbers: HashSet<u64>,
	booleans: [bool; 2],
	null: bool,
	compound: Vec<JsonValue>,
}

impl EnumSet {
	fn new(values: &[JsonValue]) -> Self {
		let mut set = Self {
			strings: HashSet::new(),
			numbers: HashSet::new(),
			booleans: [false; 2],
			null: false,
			compound: Vec::new(),
		};
		for value in values {
			match value {
				JsonValue::String(s) => {
					set.strings.insert(s.clone());
				}
				JsonValue::Number(n) => {
					set.numbers.insert(number_key(number_value(n)));
				}
				JsonValue::Bool(b) => set.booleans[usize::from(*b)] = true,
				JsonValue::Null => set.null = true,
				compound => set.compound.push(compound.clone()),
			}
		}
		set
	}

	fn contains(&self, v: &JsonValue) -> bool {
		match v {
			JsonValue::String(s) => self.strings.contains(s.as_str()),
			JsonValue::Number(n) => self.numbers.contains(&number_key(number_value(n))),
			JsonValue::Bool(b) => self.booleans[usize::from(*b)],
			JsonValue::Null => self.null,
			compound => self.compound.iter().any(|member| deep_equal(compound, member)),
		}
	}
}

/// A double as a set key with JS equality: `0` and `-0` are one number.
fn number_key(x: f64) -> u64 {
	if x == 0.0 { 0 } else { x.to_bits() }
}

/// `enum`.
pub(crate) fn enum_check(values: &[JsonValue]) -> Check {
	let set = EnumSet::new(values);
	Check::new(move |v, _| set.contains(v))
}

/// `const`.
pub(crate) fn const_check(value: &JsonValue) -> Check {
	let value = value.clone();
	Check::new(move |v, _| deep_equal(v, &value))
}

/// `anyOf`.
pub(crate) fn any_check(checks: Vec<Check>) -> Check {
	Check::new(move |v, ctx| checks.iter().any(|c| c.run(v, ctx)))
}

/// `oneOf`.
pub(crate) fn one_check(checks: Vec<Check>) -> Check {
	Check::new(move |v, ctx| {
		let mut n = 0;
		for check in &checks {
			if check.run(v, ctx) {
				n += 1;
				if n > 1 {
					return false;
				}
			}
		}
		n == 1
	})
}

/// `not`.
pub(crate) fn not_check(check: Check) -> Check {
	Check::new(move |v, ctx| !check.run(v, ctx))
}

/// `if`, `then` and `else`.
pub(crate) fn if_check(cond: Check, then: Check, otherwise: Check) -> Check {
	Check::new(move |v, ctx| {
		if cond.run(v, ctx) {
			then.run(v, ctx)
		} else {
			otherwise.run(v, ctx)
		}
	})
}

/// A union of objects tagged by a const on `key`: the tag selects the one branch that can match.
pub(crate) fn tagged_check(key: String, tags: Vec<JsonValue>, checks: Vec<Check>) -> Check {
	Check::new(move |v, ctx| {
		let Some(tag) = v.as_object().and_then(|o| o.get(&key)) else {
			return false;
		};
		match tags.iter().position(|t| strict_equal(t, tag)) {
			Some(index) => checks[index].run(v, ctx),
			None => false,
		}
	})
}

/// JS `===` on JSON values: primitives by value (numbers as doubles), never two objects or arrays (the values this
/// compares come from different documents, so none is identical to another).
pub(crate) fn strict_equal(a: &JsonValue, b: &JsonValue) -> bool {
	match (a, b) {
		(JsonValue::Number(x), JsonValue::Number(y)) => same_number(x, y),
		(JsonValue::Array(_) | JsonValue::Object(_), _) => false,
		_ => a == b,
	}
}

/// `String(value)` in JS: what a value becomes as a property key or in a joined list (`[1, null]` is `"1,"`).
pub(crate) fn js_string(value: &JsonValue) -> String {
	match value {
		JsonValue::String(s) => s.clone(),
		JsonValue::Number(n) => pi_json::js_number_to_string(number_value(n)),
		JsonValue::Bool(b) => b.to_string(),
		JsonValue::Null => "null".to_owned(),
		JsonValue::Array(items) => js_join(items, ","),
		JsonValue::Object(_) => "[object Object]".to_owned(),
	}
}

/// `Array.prototype.join`: `null` items join as empty strings.
pub(crate) fn js_join(items: &[JsonValue], separator: &str) -> String {
	let parts: Vec<String> = items
		.iter()
		.map(|item| if item.is_null() { String::new() } else { js_string(item) })
		.collect();
	parts.join(separator)
}

/// Whether a JSON value is a primitive (not an object or an array).
pub(crate) fn is_primitive(value: &JsonValue) -> bool {
	!matches!(value, JsonValue::Array(_) | JsonValue::Object(_))
}

/// JSON equality, as `const`, `enum` and `uniqueItems` compare: the same primitives (numbers as doubles, so `1` and
/// `1.0` are equal), arrays with equal items in order, objects with the same keys and equal values in any order.
pub fn deep_equal(a: &JsonValue, b: &JsonValue) -> bool {
	match (a, b) {
		(JsonValue::Number(x), JsonValue::Number(y)) => same_number(x, y),
		(JsonValue::Array(x), JsonValue::Array(y)) => {
			x.len() == y.len() && x.iter().zip(y).all(|(a, b)| deep_equal(a, b))
		}
		(JsonValue::Object(x), JsonValue::Object(y)) => {
			x.len() == y.len() && x.iter().all(|(k, a)| y.get(k).is_some_and(|b| deep_equal(a, b)))
		}
		_ => a == b,
	}
}

#[cfg(test)]
mod tests {
	use super::{deep_equal, is_multiple};
	use serde_json::json;

	#[test]
	fn multiples_are_exact_for_decimals() {
		assert!(is_multiple(0.0075, 0.0001));
		assert!(!is_multiple(0.00751, 0.0001));
		assert!(is_multiple(19.99, 0.01));
		assert!(is_multiple(1e308, 0.5));
		assert!(!is_multiple(7.0, 0.0));
		assert!(is_multiple(4.0, 2.0));
	}

	#[test]
	fn numbers_compare_as_doubles() {
		assert!(deep_equal(&json!(1), &json!(1.0)));
		assert!(deep_equal(&json!({"a": [1, 2.0]}), &json!({"a": [1.0, 2]})));
		assert!(!deep_equal(&json!([1]), &json!({"0": 1})));
	}
}

//! Coercion (TS `coerce.ts`): turns a loosely typed value (a model's tool call arguments) into the value a schema
//! most likely meant, before it is checked. Only validators compiled with `coerce` have it.
//!
//! Three passes, in this order:
//! 1. Optional nulls: a `null` in an optional property whose schema rejects null is dropped, as if omitted.
//! 2. Conversion rules ported from TypeBox 1.3.27's `Value.Convert` (MIT), keyed on JSON Schema structure (`$ref`,
//!    `const`, `enum`, `anyOf`/`oneOf`, `allOf`, `type`, `items`/`prefixItems`, `properties`/`patternProperties`):
//!    `null` becomes "null" for a string, `"3.5"` becomes 3 for an integer, a single value becomes a one-item array,
//!    and a union takes the first branch whose conversion passes.
//! 3. Primitive coercion by `type` (also type arrays), as Ajv's `coerceTypes`: what pass 2 left.
//!
//! A value that cannot be coerced is left as it is; the check then reports it. Every pass returns `None` for an
//! unchanged value and copies only the changed paths. Members are walked by `properties`, then `patternProperties`,
//! then `additionalProperties` for keys neither names; tuple items by `prefixItems` (or a draft-7 `items` array), the
//! rest by `items`. Draft 7's `additionalItems` is not coerced.
//!
//! Decided differences from TS: the `bigint` and `undefined` types convert nothing (no JSON value has them), and
//! pass 2's results through a `$ref` are reused by equal value (keys in the same order) instead of by object identity.
//! Members are walked in JS key order (`pi_json::js_key_order`), as TS's `Object.keys`.

use crate::checks::{is_integer, number_value, regex_test, strict_equal};
use crate::compiler::Compiler;
use crate::resolver::{DEFAULT_BASE, identity};
use pi_json::{JsonValue, js_key_order};
use regress::Regex;
use serde_json::{Map, Number};
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// The result of coercing one value: `Some` with the new value, `None` when it stays as it is.
type Changed = Option<JsonValue>;

/// Coerces values for one compiled schema, within one `coerce()` call.
pub(crate) struct Coercer<'c, 'd> {
	c: &'c mut Compiler<'d>,
	schema: &'d JsonValue,
	/// The (schema node, value) pairs pass 2 is converting through a re-entry point, as a stack. Conversion can enter
	/// a node again with the same value only through a `$ref` or by wrapping a single value into an array: a
	/// recursive schema that meets a value no branch accepts would convert it without end. A pair already on the
	/// stack (primitives by value, objects and arrays by address, alive while on the stack) leaves the value as is.
	entered: Vec<(usize, Identity)>,
	/// How often a re-entry was cut short in this call; results computed across one are not memoized.
	cuts: usize,
	/// Results of `$ref` targets by node and value: a recursive union whose branches all convert the same children
	/// (and all fail) would convert them once per branch, exponentially in the depth. Keyed by value in key order:
	/// `{"b":1,"a":2}` must not take the result of `{"a":2,"b":1}`, whose conversion has the other order.
	memo: HashMap<usize, HashMap<Ordered, Changed>>,
}

/// A JSON value compared and hashed with its object members in sequence (`serde_json`'s `Map` equality and hash
/// ignore the order of the keys).
struct Ordered(JsonValue);

impl PartialEq for Ordered {
	fn eq(&self, other: &Self) -> bool {
		ordered_eq(&self.0, &other.0)
	}
}

impl Eq for Ordered {}

impl Hash for Ordered {
	fn hash<H: Hasher>(&self, state: &mut H) {
		ordered_hash(&self.0, state);
	}
}

fn ordered_eq(a: &JsonValue, b: &JsonValue) -> bool {
	match (a, b) {
		(JsonValue::Array(x), JsonValue::Array(y)) => {
			x.len() == y.len() && x.iter().zip(y).all(|(a, b)| ordered_eq(a, b))
		}
		(JsonValue::Object(x), JsonValue::Object(y)) => {
			x.len() == y.len() && x.iter().zip(y).all(|((ka, a), (kb, b))| ka == kb && ordered_eq(a, b))
		}
		_ => a == b,
	}
}

fn ordered_hash<H: Hasher>(value: &JsonValue, state: &mut H) {
	std::mem::discriminant(value).hash(state);
	match value {
		JsonValue::Null => {}
		JsonValue::Bool(b) => b.hash(state),
		JsonValue::Number(n) => n.hash(state),
		JsonValue::String(s) => s.hash(state),
		JsonValue::Array(items) => {
			items.len().hash(state);
			for item in items {
				ordered_hash(item, state);
			}
		}
		JsonValue::Object(map) => {
			map.len().hash(state);
			for (k, v) in map {
				k.hash(state);
				ordered_hash(v, state);
			}
		}
	}
}

/// How the re-entry stack tells values apart: JS `Object.is` on primitives, identity on objects and arrays.
enum Identity {
	Primitive(JsonValue),
	Address(usize),
}

impl Identity {
	fn of(value: &JsonValue) -> Self {
		match value {
			JsonValue::Array(_) | JsonValue::Object(_) => Self::Address(identity(value)),
			primitive => Self::Primitive(primitive.clone()),
		}
	}

	fn is(&self, other: &Self) -> bool {
		match (self, other) {
			(Self::Address(a), Self::Address(b)) => a == b,
			(Self::Primitive(a), Self::Primitive(b)) => same_value(a, b),
			_ => false,
		}
	}
}

impl<'c, 'd> Coercer<'c, 'd> {
	/// A coercer of `schema`, the root of `compiler`.
	pub(crate) fn new(compiler: &'c mut Compiler<'d>, schema: &'d JsonValue) -> Self {
		Self {
			c: compiler,
			schema,
			entered: Vec::new(),
			cuts: 0,
			memo: HashMap::new(),
		}
	}

	/// `value` coerced by the three passes.
	pub(crate) fn coerce(mut self, value: &JsonValue) -> JsonValue {
		let without_nulls = self.drop_optional_nulls(value, self.schema, DEFAULT_BASE);
		let first = without_nulls.as_ref().unwrap_or(value);
		let converted = self.convert(self.schema, DEFAULT_BASE, first);
		let second = converted.as_ref().unwrap_or(first);
		let typed = self.coerce_types(second, self.schema, DEFAULT_BASE);
		typed.or(converted).or(without_nulls).unwrap_or_else(|| value.clone())
	}

	/// Pass 2 alone, as TypeBox's `Value.Convert` (the TypeBox conformance tests run it).
	#[cfg(test)]
	pub(crate) fn convert_only(mut self, value: &JsonValue) -> JsonValue {
		self.convert(self.schema, DEFAULT_BASE, value)
			.unwrap_or_else(|| value.clone())
	}

	/// `node` converted for `value` at a re-entry point: unchanged when that pair is already being converted.
	fn reenter(&mut self, node: &'d JsonValue, base: &str, value: &JsonValue, memoize: bool) -> Changed {
		let key = identity(node);
		let id = Identity::of(value);
		if self.entered.iter().any(|(n, v)| *n == key && v.is(&id)) {
			self.cuts += 1;
			return None;
		}
		let ordered = memoize.then(|| Ordered(value.clone()));
		if let Some(known) = ordered.as_ref().and_then(|o| self.memo.get(&key)?.get(o)) {
			return known.clone();
		}
		let cuts = self.cuts;
		self.entered.push((key, id));
		let result = self.convert(node, base, value);
		self.entered.pop();
		if let Some(ordered) = ordered.filter(|_| self.cuts == cuts) {
			self.memo.entry(key).or_default().insert(ordered, result.clone());
		}
		result
	}

	/// Whether `value` passes the schema `node`; `None` when `node` cannot be compiled.
	fn passes(&mut self, node: &'d JsonValue, base: &str, value: &JsonValue) -> Option<bool> {
		let base = self.base_of(node, base);
		let check = self.c.check_finished(node, &base).ok()?;
		Some(check.run(value, &mut self.c.ctx()))
	}

	fn base_of(&mut self, node: &'d JsonValue, base: &str) -> String {
		match node.as_object() {
			Some(map) => self.c.r.base_for(map, base),
			None => base.to_owned(),
		}
	}

	fn patterns(&mut self, map: &'d Map<String, JsonValue>) -> Vec<(Arc<Regex>, &'d JsonValue)> {
		let patterns = map.get("patternProperties").and_then(JsonValue::as_object);
		// invariant: the compile that came first accepted every pattern, so none fails to compile here.
		patterns
			.into_iter()
			.flat_map(js_key_order)
			.filter_map(|(p, s)| self.c.regex(p).ok().map(|re| (re, s)))
			.collect()
	}

	// Pass 1: optional nulls.

	fn drop_optional_nulls(&mut self, value: &JsonValue, schema: &'d JsonValue, base: &str) -> Changed {
		let map = schema.as_object()?;
		let b = self.c.r.base_for(map, base);
		if let Some(items) = value.as_array() {
			return map_items(items, |item, i| {
				item_schema_at(map, i).and_then(|item_schema| self.drop_optional_nulls(item, item_schema, &b))
			});
		}
		let o = value.as_object()?;
		let properties = map.get("properties").and_then(JsonValue::as_object);
		let patterns = self.patterns(map);
		let additional = map.get("additionalProperties").filter(|a| a.is_object());
		if patterns.is_empty() && additional.is_none() && properties.is_none() {
			return None;
		}
		let required: HashSet<&str> = map
			.get("required")
			.and_then(JsonValue::as_array)
			.map(|list| list.iter().filter_map(JsonValue::as_str).collect())
			.unwrap_or_default();
		let mut out: Option<Map<String, JsonValue>> = None;
		for (key, property) in properties.into_iter().flat_map(js_key_order) {
			let Some(x) = o.get(key) else { continue };
			let is_ref = property.get("$ref").is_some_and(JsonValue::is_string);
			if x.is_null() && !required.contains(key.as_str()) && !is_ref && self.passes(property, &b, x) == Some(false)
			{
				out.get_or_insert_with(|| o.clone()).shift_remove(key);
				continue;
			}
			let next = self.drop_optional_nulls(x, property, &b);
			set(&mut out, o, key, next);
		}
		// Values of pattern and additional properties are walked for their own optional nulls; the keys themselves
		// are not optional properties, so they stay.
		if !patterns.is_empty() || additional.is_some() {
			for (key, x) in js_key_order(o) {
				let mut matched = properties.is_some_and(|p| p.contains_key(key));
				for (re, item) in &patterns {
					if !regex_test(re, key) {
						continue;
					}
					matched = true;
					let Some(current) = out.as_ref().map_or(Some(x), |out| out.get(key)) else {
						continue;
					};
					let current = current.clone();
					let next = self.drop_optional_nulls(&current, item, &b);
					set(&mut out, o, key, next);
				}
				if let Some(additional) = additional.filter(|_| !matched) {
					let next = self.drop_optional_nulls(x, additional, &b);
					set(&mut out, o, key, next);
				}
			}
		}
		out.map(JsonValue::Object)
	}

	// Pass 2: TypeBox's conversion rules.

	fn convert(&mut self, node: &'d JsonValue, base: &str, value: &JsonValue) -> Changed {
		let map = node.as_object()?;
		let b = self.c.r.base_for(map, base);
		if let Some(reference) = map.get("$ref").and_then(JsonValue::as_str) {
			let target = self.c.r.resolve(reference, &b).ok()?;
			return if target.node.is_object() {
				self.reenter(target.node, &target.base, value, true)
			} else {
				None
			};
		}
		self.convert_node(node, map, &b, value)
	}

	/// One schema node's conversion: its first keyword among `const`, `enum`, the unions, `allOf` and `type`.
	fn convert_node(
		&mut self,
		node: &'d JsonValue,
		map: &'d Map<String, JsonValue>,
		b: &str,
		value: &JsonValue,
	) -> Changed {
		if let Some(literal) = map.get("const") {
			return convert_literal(literal, value);
		}
		if let Some(values) = map.get("enum").and_then(JsonValue::as_array) {
			return self.convert_enum(node, values, b, value);
		}
		// A union or intersection node may also declare members (plain JSON Schema; TypeBox's never do): they convert
		// after the branches.
		for keyword in ["anyOf", "oneOf"] {
			if let Some(branches) = map.get(keyword).and_then(JsonValue::as_array) {
				return self.convert_union(node, map, branches, b, value);
			}
		}
		if let Some(members) = map.get("allOf").and_then(JsonValue::as_array) {
			let mut next: Changed = None;
			for member in members {
				let current = next.as_ref().unwrap_or(value);
				next = self.convert(member, b, current).or(next);
			}
			let current = next.as_ref().unwrap_or(value);
			return self.convert_members(map, b, current).or(next);
		}
		match map.get("type") {
			Some(JsonValue::String(ty)) => match ty.as_str() {
				"string" => changed(try_string(value), value),
				"number" => changed(try_number(value).map(js_number), value),
				"integer" => changed(try_number(value).map(|n| js_number(n.trunc())), value),
				"boolean" => changed(try_boolean(value).map(JsonValue::Bool), value),
				"null" => changed(try_null(value), value),
				"array" => self.convert_array(map, b, value),
				"object" => self.convert_object(map, b, value),
				_ => None,
			},
			None if map.contains_key("properties") || map.contains_key("patternProperties") => {
				self.convert_object(map, b, value)
			}
			None if map.contains_key("items") || map.contains_key("prefixItems") => self.convert_array(map, b, value),
			_ => None,
		}
	}

	/// An enum is a union of its values: a member stays, else the first value the input converts to.
	fn convert_enum(&mut self, node: &'d JsonValue, values: &[JsonValue], base: &str, value: &JsonValue) -> Changed {
		if values.iter().any(|member| same_value(member, value)) {
			return None;
		}
		for member in values {
			if let Some(candidate) = convert_literal(member, value)
				&& self.passes(node, base, &candidate) == Some(true)
			{
				return Some(candidate);
			}
		}
		None
	}

	/// A value one branch accepts stays; else the first branch's conversion that the whole union accepts. Branches are
	/// converted in order and only until one is accepted, so a later recursive branch is not entered needlessly. A node
	/// that also declares members converts them into each candidate before it is checked, so a branch is not
	/// discarded for a sibling property that is converted after it.
	fn convert_union(
		&mut self,
		node: &'d JsonValue,
		map: &'d Map<String, JsonValue>,
		branches: &'d [JsonValue],
		base: &str,
		value: &JsonValue,
	) -> Changed {
		for branch in branches {
			if self.passes(branch, base, value) == Some(true) {
				return self.convert_members(map, base, value);
			}
		}
		for branch in branches {
			let converted = self.convert(branch, base, value);
			let current = converted.as_ref().unwrap_or(value);
			let candidate = self.convert_members(map, base, current).or(converted);
			if self.passes(node, base, candidate.as_ref().unwrap_or(value)) == Some(true) {
				return candidate;
			}
		}
		self.convert_members(map, base, value)
	}

	/// The object members of a union or intersection node, when it declares any.
	fn convert_members(&mut self, map: &'d Map<String, JsonValue>, base: &str, value: &JsonValue) -> Changed {
		let declares = ["properties", "patternProperties", "additionalProperties"]
			.iter()
			.any(|k| map.contains_key(*k));
		if declares {
			self.convert_object(map, base, value)
		} else {
			None
		}
	}

	fn convert_array(&mut self, map: &'d Map<String, JsonValue>, base: &str, value: &JsonValue) -> Changed {
		let items_list = map.get("items").and_then(JsonValue::as_array);
		if let Some(prefix) = items_list.or_else(|| map.get("prefixItems").and_then(JsonValue::as_array)) {
			// A tuple converts its items and never wraps a single value.
			let items = value.as_array()?;
			let rest = if items_list.is_some() { None } else { map.get("items") };
			return map_items(items, |item, i| {
				prefix.get(i).or(rest).and_then(|s| self.convert(s, base, item))
			});
		}
		let item_schema = map.get("items");
		if let Some(items) = value.as_array() {
			let item_schema = item_schema?;
			return map_items(items, |item, _| self.convert(item_schema, base, item));
		}
		// The wrapped single value is converted by the item schema with the same value: a re-entry point.
		let item = match item_schema.filter(|s| s.is_object()) {
			Some(schema) => self
				.reenter(schema, base, value, false)
				.unwrap_or_else(|| value.clone()),
			None => value.clone(),
		};
		Some(JsonValue::Array(vec![item]))
	}

	fn convert_object(&mut self, map: &'d Map<String, JsonValue>, base: &str, value: &JsonValue) -> Changed {
		let o = value.as_object()?;
		let properties = map.get("properties").and_then(JsonValue::as_object);
		let patterns = self.patterns(map);
		let additional = map.get("additionalProperties").filter(|a| a.is_object());
		let mut out: Option<Map<String, JsonValue>> = None;
		for (key, x) in js_key_order(o) {
			let mut next: Changed = None;
			let property = properties.and_then(|p| p.get(key));
			let mut matched = property.is_some();
			if let Some(property) = property {
				next = self.convert(property, base, x);
			}
			for (re, schema) in &patterns {
				if !regex_test(re, key) {
					continue;
				}
				matched = true;
				let current = next.as_ref().unwrap_or(x);
				next = self.convert(schema, base, current).or(next);
			}
			if let Some(additional) = additional.filter(|_| !matched) {
				next = self.convert(additional, base, x);
			}
			set(&mut out, o, key, next);
		}
		out.map(JsonValue::Object)
	}

	// Pass 3: primitive coercion by type.

	fn coerce_types(&mut self, value: &JsonValue, schema: &'d JsonValue, base: &str) -> Changed {
		let map = schema.as_object()?;
		let b = self.c.r.base_for(map, base);
		let mut next: Changed = None;
		for member in map.get("allOf").and_then(JsonValue::as_array).into_iter().flatten() {
			let current = next.as_ref().unwrap_or(value);
			next = self.coerce_types(current, member, &b).or(next);
		}
		for keyword in ["anyOf", "oneOf"] {
			if let Some(branches) = map.get(keyword).and_then(JsonValue::as_array) {
				let current = next.as_ref().unwrap_or(value);
				next = self.coerce_union(current, branches, &b).or(next);
			}
		}
		let types: Vec<&str> = match map.get("type") {
			Some(JsonValue::String(ty)) => vec![ty.as_str()],
			Some(JsonValue::Array(list)) => list.iter().filter_map(JsonValue::as_str).collect(),
			_ => Vec::new(),
		};
		let current = next.as_ref().unwrap_or(value);
		let matches_union_member = types.len() > 1 && types.iter().any(|ty| matches_type(current, ty));
		if !matches_union_member && let Some(candidate) = types.iter().find_map(|ty| coerce_primitive(current, ty)) {
			next = Some(candidate);
		}
		let current = next.as_ref().unwrap_or(value);
		if types.contains(&"object") && current.is_object() {
			next = self.coerce_properties(current, map, &b).or(next);
		} else if types.contains(&"array") && current.is_array() {
			next = self.coerce_items(current, map, &b).or(next);
		}
		next
	}

	fn coerce_union(&mut self, value: &JsonValue, branches: &'d [JsonValue], base: &str) -> Changed {
		for branch in branches {
			if self.passes(branch, base, value) == Some(true) {
				return None;
			}
		}
		for branch in branches {
			let coerced = self.coerce_types(value, branch, base);
			if self.passes(branch, base, coerced.as_ref().unwrap_or(value)) == Some(true) {
				return coerced;
			}
		}
		None
	}

	fn coerce_properties(&mut self, value: &JsonValue, map: &'d Map<String, JsonValue>, base: &str) -> Changed {
		let o = value.as_object()?;
		let properties = map.get("properties").and_then(JsonValue::as_object);
		let patterns = self.patterns(map);
		let additional = map.get("additionalProperties").filter(|a| a.is_object());
		let mut out: Option<Map<String, JsonValue>> = None;
		for (key, property) in properties.into_iter().flat_map(js_key_order) {
			if let Some(x) = o.get(key) {
				let next = self.coerce_types(x, property, base);
				set(&mut out, o, key, next);
			}
		}
		for (key, x) in js_key_order(o) {
			let mut matched = properties.is_some_and(|p| p.contains_key(key));
			for (re, item) in &patterns {
				if !regex_test(re, key) {
					continue;
				}
				matched = true;
				let current = out.as_ref().and_then(|out| out.get(key)).unwrap_or(x).clone();
				let next = self.coerce_types(&current, item, base);
				set(&mut out, o, key, next);
			}
			if let Some(additional) = additional.filter(|_| !matched) {
				let next = self.coerce_types(x, additional, base);
				set(&mut out, o, key, next);
			}
		}
		out.map(JsonValue::Object)
	}

	fn coerce_items(&mut self, value: &JsonValue, map: &'d Map<String, JsonValue>, base: &str) -> Changed {
		let items = value.as_array()?;
		map_items(items, |item, i| {
			item_schema_at(map, i)
				.filter(|s| s.is_object())
				.and_then(|s| self.coerce_types(item, s, base))
		})
	}
}

/// Writes a changed member into the copy of `original` (made on the first change).
fn set(out: &mut Option<Map<String, JsonValue>>, original: &Map<String, JsonValue>, key: &str, next: Changed) {
	if let Some(next) = next {
		out.get_or_insert_with(|| original.clone()).insert(key.to_owned(), next);
	}
}

/// The schema of an array's item at `index`: a tuple's (`prefixItems`, or a draft-7 `items` array) for its prefix,
/// `items` after `prefixItems`, or `items` for every item. `None` when nothing applies.
fn item_schema_at(map: &Map<String, JsonValue>, index: usize) -> Option<&JsonValue> {
	if let Some(items) = map.get("items").and_then(JsonValue::as_array) {
		return items.get(index);
	}
	if let Some(prefix) = map.get("prefixItems").and_then(JsonValue::as_array)
		&& index < prefix.len()
	{
		return prefix.get(index);
	}
	map.get("items")
}

/// `items` mapped by `map`; `None` when every item maps to itself.
fn map_items(items: &[JsonValue], mut map: impl FnMut(&JsonValue, usize) -> Changed) -> Changed {
	let mut out: Option<Vec<JsonValue>> = None;
	for (i, item) in items.iter().enumerate() {
		if let Some(next) = map(item, i) {
			out.get_or_insert_with(|| items.to_vec())[i] = next;
		}
	}
	out.map(JsonValue::Array)
}

/// `result` unless it is the value itself (JS `===` on primitives).
fn changed(result: Option<JsonValue>, value: &JsonValue) -> Changed {
	result.filter(|r| !same_value(r, value))
}

/// JS `Object.is`/`===` on primitives: numbers as doubles; objects and arrays never (they are compared by identity).
fn same_value(a: &JsonValue, b: &JsonValue) -> bool {
	strict_equal(a, b)
}

/// A double as JSON, as JS prints it: integral values as integers.
pub(crate) fn js_number(x: f64) -> JsonValue {
	const LIMIT: f64 = 9_223_372_036_854_775_808.0;
	if x.fract() == 0.0 && x.abs() < LIMIT {
		// The cast is exact: integral and within i64.
		#[expect(clippy::cast_possible_truncation, reason = "integral and within i64, checked above")]
		return JsonValue::from(x as i64);
	}
	Number::from_f64(x).map_or(JsonValue::Null, JsonValue::Number)
}

// TypeBox's conversions of one value (value/convert/try in TypeBox 1.3.27): a result, or `None` when there is none.

/// A literal (`const`, an enum value) taken from a value of another type that converts to exactly it.
fn convert_literal(literal: &JsonValue, value: &JsonValue) -> Changed {
	if same_value(literal, value) {
		return None;
	}
	let converted = match literal {
		JsonValue::String(_) => try_string(value),
		JsonValue::Number(_) => try_number(value).map(js_number),
		JsonValue::Bool(_) => try_boolean(value).map(JsonValue::Bool),
		_ => None,
	};
	converted.filter(|c| same_value(c, literal)).map(|_| literal.clone())
}

fn try_string(value: &JsonValue) -> Option<JsonValue> {
	match value {
		JsonValue::Bool(b) => Some(JsonValue::String(b.to_string())),
		JsonValue::Number(n) => Some(JsonValue::String(pi_json::js_number_to_string(number_value(n)))),
		JsonValue::Null => Some(JsonValue::String("null".to_owned())),
		JsonValue::String(_) => Some(value.clone()),
		_ => None,
	}
}

const MAX_SAFE_INTEGER: i128 = 9_007_199_254_740_991;

fn try_number(value: &JsonValue) -> Option<f64> {
	match value {
		JsonValue::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
		JsonValue::Number(n) => Some(number_value(n)),
		JsonValue::Null => Some(0.0),
		JsonValue::String(s) => {
			let coerced = js_string_to_number(s);
			if coerced.is_finite() {
				return Some(coerced);
			}
			match s.to_lowercase().as_str() {
				"false" => Some(0.0),
				"true" => Some(1.0),
				_ => bigint_of_string(s)
					.filter(|big| big.abs() <= MAX_SAFE_INTEGER)
					.map(safe_integer),
			}
		}
		_ => None,
	}
}

/// A safe integer (`|n| <= 2^53 - 1`) as the double it is exactly.
#[expect(clippy::cast_precision_loss, reason = "a safe integer is exact as a double")]
fn safe_integer(n: i128) -> f64 {
	n as f64
}

/// TypeBox's `TryBigInt` of a string: `12n`, `12.5` (truncated) or `12`.
fn bigint_of_string(value: &str) -> Option<i128> {
	let integer = |s: &str| {
		let digits = s.strip_prefix('-').unwrap_or(s);
		let canonical = digits == "0" || (!digits.starts_with('0') && !digits.is_empty());
		(canonical && digits.bytes().all(|b| b.is_ascii_digit()))
			.then(|| s.parse::<i128>().ok())
			.flatten()
	};
	if let Some(body) = value.strip_suffix('n') {
		return integer(body);
	}
	if let Some((whole, fraction)) = value.split_once('.') {
		let fraction_ok = !fraction.is_empty() && fraction.bytes().all(|b| b.is_ascii_digit());
		return if fraction_ok { integer(whole) } else { None };
	}
	integer(value)
}

fn try_boolean(value: &JsonValue) -> Option<bool> {
	match value {
		JsonValue::Bool(b) => Some(*b),
		JsonValue::Number(n) => match number_value(n) {
			0.0 => Some(false),
			1.0 => Some(true),
			_ => None,
		},
		JsonValue::Null => Some(false),
		JsonValue::String(s) => match s.to_lowercase().as_str() {
			"false" | "0" => Some(false),
			"true" | "1" => Some(true),
			_ => None,
		},
		_ => None,
	}
}

fn try_null(value: &JsonValue) -> Option<JsonValue> {
	let null = match value {
		JsonValue::Bool(b) => !b,
		JsonValue::Number(n) => number_value(n) == 0.0,
		JsonValue::Null => true,
		JsonValue::String(s) => s.is_empty() || s == "0" || matches!(s.to_lowercase().as_str(), "undefined" | "null"),
		_ => false,
	};
	null.then_some(JsonValue::Null)
}

/// JS `Number(text)` (StringToNumber): whitespace-trimmed decimal, `Infinity`, or `0x`/`0o`/`0b` integers; an empty
/// string is 0, anything else NaN.
pub(crate) fn js_string_to_number(text: &str) -> f64 {
	let s = js_trim(text);
	if s.is_empty() {
		return 0.0;
	}
	for (prefix, radix) in [("0x", 16), ("0X", 16), ("0o", 8), ("0O", 8), ("0b", 2), ("0B", 2)] {
		if let Some(digits) = s.strip_prefix(prefix) {
			if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
				return f64::NAN;
			}
			return digits
				.chars()
				.filter_map(|c| c.to_digit(radix))
				.fold(0.0, |n, d| n * f64::from(radix) + f64::from(d));
		}
	}
	let unsigned = s.strip_prefix(['+', '-']).unwrap_or(s);
	if unsigned == "Infinity" {
		return if s.starts_with('-') {
			f64::NEG_INFINITY
		} else {
			f64::INFINITY
		};
	}
	if !is_decimal_literal(unsigned) {
		return f64::NAN;
	}
	s.parse().unwrap_or(f64::NAN)
}

/// `digits [. digits?] | . digits`, then an optional exponent: the unsigned decimal literal `StringToNumber` reads.
fn is_decimal_literal(s: &str) -> bool {
	let (mantissa, exponent) = match s.find(['e', 'E']) {
		Some(at) => (&s[..at], Some(&s[at + 1..])),
		None => (s, None),
	};
	let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
	let digits = |part: &str| part.bytes().all(|b| b.is_ascii_digit());
	let mantissa_ok = digits(whole) && digits(fraction) && !(whole.is_empty() && fraction.is_empty());
	let exponent_ok = exponent.is_none_or(|e| {
		let e = e.strip_prefix(['+', '-']).unwrap_or(e);
		!e.is_empty() && digits(e)
	});
	mantissa_ok && exponent_ok
}

/// `String.prototype.trim`: JS whitespace and line terminators at both ends.
fn js_trim(s: &str) -> &str {
	s.trim_matches(|c: char| {
		matches!(
			c,
			'\t' | '\n'
				| '\u{b}' | '\u{c}'
				| '\r' | ' ' | '\u{a0}'
				| '\u{1680}' | '\u{2028}'
				| '\u{2029}' | '\u{202f}'
				| '\u{205f}' | '\u{3000}'
				| '\u{feff}'
		) || ('\u{2000}'..='\u{200a}').contains(&c)
	})
}

// Primitive coercion by type (pass 3).

fn matches_type(value: &JsonValue, ty: &str) -> bool {
	match ty {
		"number" => value.is_number(),
		"integer" => value.as_number().is_some_and(is_integer),
		"boolean" => value.is_boolean(),
		"string" => value.is_string(),
		"null" => value.is_null(),
		"array" => value.is_array(),
		"object" => value.is_object(),
		_ => false,
	}
}

fn coerce_primitive(value: &JsonValue, ty: &str) -> Changed {
	let candidate = match (ty, value) {
		("number" | "integer", JsonValue::Null) => Some(js_number(0.0)),
		("number" | "integer", JsonValue::String(s)) if !js_trim(s).is_empty() => {
			let parsed = js_string_to_number(s);
			let ok = if ty == "number" {
				parsed.is_finite()
			} else {
				parsed.is_finite() && parsed.fract() == 0.0
			};
			ok.then(|| js_number(parsed))
		}
		("number" | "integer", JsonValue::Bool(b)) => Some(js_number(if *b { 1.0 } else { 0.0 })),
		("boolean", JsonValue::Null) => Some(JsonValue::Bool(false)),
		("boolean", JsonValue::String(s)) if s == "true" || s == "false" => Some(JsonValue::Bool(s == "true")),
		("boolean", JsonValue::Number(_)) => try_boolean(value).map(JsonValue::Bool),
		("string", JsonValue::Null) => Some(JsonValue::String(String::new())),
		("string", JsonValue::Number(_) | JsonValue::Bool(_)) => try_string(value),
		("null", JsonValue::String(s)) if s.is_empty() => Some(JsonValue::Null),
		("null", JsonValue::Number(n)) if number_value(n) == 0.0 => Some(JsonValue::Null),
		("null", JsonValue::Bool(false)) => Some(JsonValue::Null),
		_ => None,
	};
	changed(candidate, value)
}

#[cfg(test)]
mod typebox_tests;

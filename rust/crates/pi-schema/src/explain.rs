//! Errors of a failed value (TS `explain.ts`): a walk over the schema that reports each failing keyword at the value
//! it failed on. It runs only for values that failed the check, using the compiler's checks for subschemas; the
//! messages are the TS strings, in TS's order: object members are walked in JS key order (array-index keys first,
//! `pi_json::js_key_order`).

use crate::annotations::Annotations;
use crate::checks::{
	contains_ok, count, deep_equal, is_multiple, is_primitive, js_join, js_string, length_ok, number_value, regex_test,
	strict_equal, type_bit, type_bits, unique_ok,
};
use crate::compiler::{Compiler, SKIP_UNEVALUATED, Stop, View, item_schemas};
use crate::resolver::{DEFAULT_BASE, identity};
use crate::types::{Draft, ValidationError};
use pi_json::{JsonValue, js_key_order};
use serde_json::Map;
use std::collections::HashMap;

/// Explains failures of one compiled schema, within one `errors()` call.
pub(crate) struct Explainer<'c, 'd> {
	c: &'c mut Compiler<'d>,
	schema: &'d JsonValue,
	/// The errors of `$ref` targets for object and array values, by (target, value): a recursive union explains each
	/// branch to pick the one to blame, and every branch would explain the same children again, exponentially in the
	/// depth. Values are borrowed for the whole call, so their addresses identify them.
	memo: HashMap<(usize, usize), (String, Vec<ValidationError>)>,
}

type Walk = Result<(), Stop>;

impl<'c, 'd> Explainer<'c, 'd> {
	/// An explainer of `schema`, the root of `compiler`.
	pub(crate) fn new(compiler: &'c mut Compiler<'d>, schema: &'d JsonValue) -> Self {
		Self {
			c: compiler,
			schema,
			memo: HashMap::new(),
		}
	}

	/// The errors of `value`, at least one (`value` failed the check).
	pub(crate) fn errors(mut self, value: &JsonValue) -> Vec<ValidationError> {
		let mut out = Vec::new();
		if let Err(stop) = self.walk(self.schema, DEFAULT_BASE, value, "", &mut out) {
			// invariant: the schema compiled once already, with the same dynamic scope tracking, so its subschemas
			// compile again and never ask for a restart; should one fail all the same, the failure is the error.
			let message = match stop {
				Stop::Error(error) => error.to_string(),
				Stop::Restart => "invalid".to_owned(),
			};
			return vec![ValidationError::new("", "schema", message)];
		}
		if out.is_empty() {
			out.push(ValidationError::new("", "schema", "invalid"));
		}
		out
	}

	fn ok(&mut self, node: &'d JsonValue, base: &str, v: &JsonValue) -> Result<bool, Stop> {
		let check = self.c.check_finished(node, base)?;
		Ok(check.run(v, &mut self.c.ctx()))
	}

	// For $dynamicRef schemas this check runs outside the original dynamic scope, so their errors are approximate.
	fn walk(
		&mut self,
		node: &'d JsonValue,
		base: &str,
		v: &JsonValue,
		path: &str,
		out: &mut Vec<ValidationError>,
	) -> Walk {
		let map = match node {
			JsonValue::Bool(false) => {
				out.push(ValidationError::new(path, "false", "no value is allowed here"));
				return Ok(());
			}
			JsonValue::Object(map) => map,
			_ => return Ok(()),
		};
		let b = self.c.r.base_for(map, base);
		if self.ok(node, &b, v)? {
			return Ok(());
		}
		let start = out.len();
		if self.c.draft == Draft::V7
			&& let Some(reference) = map.get("$ref").and_then(JsonValue::as_str)
		{
			return self.walk_ref(reference, &b, v, path, out);
		}
		if let Some(ty) = map.get("type") {
			let types: &[JsonValue] = ty.as_array().map_or(std::slice::from_ref(ty), Vec::as_slice);
			let bits = type_bits(v);
			let matches = types
				.iter()
				.any(|t| t.as_str().and_then(type_bit).is_some_and(|bit| bit & bits != 0));
			if !matches {
				let message = format!("expected {}, got {}", js_join(types, " or "), type_name(v));
				out.push(ValidationError::new(path, "type", message));
				return Ok(());
			}
		}
		if let Some(values) = map.get("enum").and_then(JsonValue::as_array)
			&& !values.iter().any(|member| deep_equal(member, v))
		{
			out.push(ValidationError::new(
				path,
				"enum",
				format!("expected one of {}", list_values(values)),
			));
		}
		if let Some(value) = map.get("const")
			&& !deep_equal(value, v)
		{
			let message = format!("expected {}", pi_json::to_js_string(value));
			out.push(ValidationError::new(path, "const", message));
		}
		match v {
			JsonValue::String(s) if map.get("type").and_then(JsonValue::as_str) != Some("bytes") => {
				self.walk_string(map, s, path, out)?;
			}
			JsonValue::Number(n) => walk_number(map, number_value(n), path, out),
			JsonValue::Object(o) => self.walk_object(map, &b, o, v, path, out)?,
			JsonValue::Array(items) => self.walk_array(map, &b, items, v, path, out)?,
			_ => {}
		}
		self.walk_applicators(map, &b, v, path, out)?;
		let unevaluated = map.contains_key("unevaluatedProperties") || map.contains_key("unevaluatedItems");
		if out.len() == start && unevaluated {
			self.walk_unevaluated(map, &b, v, path, out)?;
		}
		if out.len() == start && map.get("$dynamicRef").is_some_and(JsonValue::is_string) {
			out.push(ValidationError::new(
				path,
				"$dynamicRef",
				"does not match the dynamically referenced schema",
			));
		}
		Ok(())
	}

	/// `allOf`, `anyOf`, `oneOf`, `not`, `if` and `$ref`.
	fn walk_applicators(
		&mut self,
		map: &'d Map<String, JsonValue>,
		b: &str,
		v: &JsonValue,
		path: &str,
		out: &mut Vec<ValidationError>,
	) -> Walk {
		for s in map.get("allOf").and_then(JsonValue::as_array).into_iter().flatten() {
			self.walk(s, b, v, path, out)?;
		}
		if let Some(branches) = map.get("anyOf").and_then(JsonValue::as_array) {
			self.walk_union(branches, b, v, path, out, "anyOf")?;
		}
		if let Some(branches) = map.get("oneOf").and_then(JsonValue::as_array) {
			self.walk_union(branches, b, v, path, out, "oneOf")?;
		}
		if let Some(not) = map.get("not")
			&& self.ok(not, b, v)?
		{
			out.push(ValidationError::new(path, "not", "must not match the schema in not"));
		}
		if let Some(cond) = map.get("if") {
			let branch = if self.ok(cond, b, v)? {
				map.get("then")
			} else {
				map.get("else")
			};
			if let Some(branch) = branch {
				self.walk(branch, b, v, path, out)?;
			}
		}
		if let Some(reference) = map.get("$ref").and_then(JsonValue::as_str) {
			self.walk_ref(reference, b, v, path, out)?;
		}
		Ok(())
	}

	fn walk_ref(
		&mut self,
		reference: &str,
		base: &str,
		v: &JsonValue,
		path: &str,
		out: &mut Vec<ValidationError>,
	) -> Walk {
		let target = self.c.r.resolve(reference, base)?;
		if *target.node == JsonValue::Bool(false) && self.c.r.is_unresolved(reference, base) {
			out.push(ValidationError::new(
				path,
				"$ref",
				format!("unresolved reference {reference}"),
			));
			return Ok(());
		}
		let Some(target_map) = target.node.as_object().filter(|_| !is_primitive(v)) else {
			return self.walk(target.node, &target.base, v, path, out);
		};
		let key = (identity(target_map), identity(v));
		if let Some((known_path, errors)) = self.memo.get(&key)
			&& known_path == path
		{
			out.extend(errors.iter().cloned());
			return Ok(());
		}
		let mut errors = Vec::new();
		self.walk(target.node, &target.base, v, path, &mut errors)?;
		out.extend(errors.iter().cloned());
		self.memo.insert(key, (path.to_owned(), errors));
		Ok(())
	}

	fn walk_string(
		&mut self,
		map: &Map<String, JsonValue>,
		v: &str,
		path: &str,
		out: &mut Vec<ValidationError>,
	) -> Walk {
		if let Some(min) = map.get("minLength").and_then(JsonValue::as_f64)
			&& !length_ok(v, min, f64::INFINITY)
		{
			out.push(ValidationError::new(
				path,
				"minLength",
				format!("expected at least {} characters", js(min)),
			));
		}
		if let Some(max) = map.get("maxLength").and_then(JsonValue::as_f64)
			&& !length_ok(v, 0.0, max)
		{
			out.push(ValidationError::new(
				path,
				"maxLength",
				format!("expected at most {} characters", js(max)),
			));
		}
		if let Some(pattern) = map.get("pattern").and_then(JsonValue::as_str)
			&& !regex_test(&*self.c.regex(pattern)?, v)
		{
			out.push(ValidationError::new(
				path,
				"pattern",
				format!("expected to match {pattern}"),
			));
		}
		if let Some(name) = map.get("format").and_then(JsonValue::as_str)
			&& let Some(format) = self.c.formats.get(name)
			&& !format(v)
		{
			out.push(ValidationError::new(path, "format", format!("expected format {name}")));
		}
		Ok(())
	}

	fn walk_object(
		&mut self,
		map: &'d Map<String, JsonValue>,
		b: &str,
		o: &Map<String, JsonValue>,
		v: &JsonValue,
		path: &str,
		out: &mut Vec<ValidationError>,
	) -> Walk {
		let properties = map.get("properties").and_then(JsonValue::as_object);
		for k in map.get("required").and_then(JsonValue::as_array).into_iter().flatten() {
			if let Some(k) = k.as_str()
				&& !o.contains_key(k)
			{
				out.push(ValidationError::new(pointer(path, k), "required", "missing"));
			}
		}
		for (k, s) in properties.into_iter().flat_map(js_key_order) {
			if let Some(x) = o.get(k) {
				self.walk(s, b, x, &pointer(path, k), out)?;
			}
		}
		let patterns = map.get("patternProperties").and_then(JsonValue::as_object);
		for (k, x) in js_key_order(o) {
			let mut matched = properties.is_some_and(|p| p.contains_key(k));
			for (p, s) in patterns.into_iter().flat_map(js_key_order) {
				if regex_test(&*self.c.regex(p)?, k) {
					matched = true;
					self.walk(s, b, x, &pointer(path, k), out)?;
				}
			}
			match map.get("additionalProperties") {
				Some(JsonValue::Bool(false)) if !matched => {
					out.push(ValidationError::new(
						pointer(path, k),
						"additionalProperties",
						"unknown field",
					));
				}
				Some(additional) if !matched => self.walk(additional, b, x, &pointer(path, k), out)?,
				_ => {}
			}
			if let Some(names) = map.get("propertyNames")
				&& !self.ok(names, b, &JsonValue::String(k.clone()))?
			{
				out.push(ValidationError::new(
					pointer(path, k),
					"propertyNames",
					"invalid property name",
				));
			}
		}
		let count = count(o.len());
		if let Some(min) = map.get("minProperties").and_then(JsonValue::as_f64)
			&& count < min
		{
			out.push(ValidationError::new(
				path,
				"minProperties",
				format!("expected at least {} properties", js(min)),
			));
		}
		if let Some(max) = map.get("maxProperties").and_then(JsonValue::as_f64)
			&& count > max
		{
			out.push(ValidationError::new(
				path,
				"maxProperties",
				format!("expected at most {} properties", js(max)),
			));
		}
		self.walk_dependencies(map, b, o, v, path, out)
	}

	/// `dependentRequired`, `dependencies` (spread over it, as TS merges them) and `dependentSchemas`.
	fn walk_dependencies(
		&mut self,
		map: &'d Map<String, JsonValue>,
		b: &str,
		o: &Map<String, JsonValue>,
		v: &JsonValue,
		path: &str,
		out: &mut Vec<ValidationError>,
	) -> Walk {
		let draft7 = self.c.draft == Draft::V7;
		let mut dependencies: Vec<(&'d str, &'d JsonValue)> = Vec::new();
		let required = map
			.get("dependentRequired")
			.and_then(JsonValue::as_object)
			.filter(|_| !draft7);
		let both = required
			.into_iter()
			.chain(map.get("dependencies").and_then(JsonValue::as_object));
		for (k, d) in both.flat_map(js_key_order) {
			match dependencies.iter_mut().find(|(known, _)| *known == k) {
				Some(entry) => entry.1 = d,
				None => dependencies.push((k, d)),
			}
		}
		for (k, d) in dependencies {
			if !o.contains_key(k) {
				continue;
			}
			let Some(list) = d.as_array() else {
				self.walk(d, b, v, path, out)?;
				continue;
			};
			for r in list {
				let r = js_string(r);
				if !o.contains_key(&r) {
					out.push(ValidationError::new(
						pointer(path, &r),
						"dependentRequired",
						format!("required when {k} is present"),
					));
				}
			}
		}
		if !draft7 {
			for (k, s) in map
				.get("dependentSchemas")
				.and_then(JsonValue::as_object)
				.into_iter()
				.flat_map(js_key_order)
			{
				if o.contains_key(k) {
					self.walk(s, b, v, path, out)?;
				}
			}
		}
		Ok(())
	}

	fn walk_array(
		&mut self,
		map: &'d Map<String, JsonValue>,
		b: &str,
		items: &[JsonValue],
		v: &JsonValue,
		path: &str,
		out: &mut Vec<ValidationError>,
	) -> Walk {
		let (prefix, rest) = item_schemas(View::whole(map), self.c.draft);
		for (i, item) in items.iter().enumerate() {
			if let Some(s) = prefix.get(i).or(rest) {
				self.walk(s, b, item, &format!("{path}/{i}"), out)?;
			}
		}
		let length = count(items.len());
		if let Some(min) = map.get("minItems").and_then(JsonValue::as_f64)
			&& length < min
		{
			out.push(ValidationError::new(
				path,
				"minItems",
				format!("expected at least {} items", js(min)),
			));
		}
		if let Some(max) = map.get("maxItems").and_then(JsonValue::as_f64)
			&& length > max
		{
			out.push(ValidationError::new(
				path,
				"maxItems",
				format!("expected at most {} items", js(max)),
			));
		}
		if map.get("uniqueItems") == Some(&JsonValue::Bool(true)) && !unique_ok(v) {
			out.push(ValidationError::new(path, "uniqueItems", "expected unique items"));
		}
		if let Some(contains) = map.get("contains") {
			let (min, max) = self.c.contains_bounds(View::whole(map));
			let check = self.c.check_finished(contains, b)?;
			if !contains_ok(&check, v, min, max, &mut self.c.ctx()) {
				out.push(ValidationError::new(path, "contains", contains_message(min, max)));
			}
		}
		Ok(())
	}

	fn walk_union(
		&mut self,
		branches: &'d [JsonValue],
		b: &str,
		v: &JsonValue,
		path: &str,
		out: &mut Vec<ValidationError>,
		keyword: &str,
	) -> Walk {
		let mut matches = 0;
		for s in branches {
			if self.ok(s, b, v)? {
				matches += 1;
			}
		}
		if matches == 1 || (keyword == "anyOf" && matches > 0) {
			return Ok(());
		}
		if matches > 1 {
			out.push(ValidationError::new(path, keyword, "matches more than one alternative"));
			return Ok(());
		}
		// A tagged union reports the branch its tag selects; otherwise the branch whose first error is deepest.
		if let Some((key, values)) = self.tag_of(branches, b)?
			&& let Some(o) = v.as_object()
		{
			let selected = o
				.get(key)
				.and_then(|tag| values.iter().position(|value| strict_equal(value, tag)));
			if let Some(i) = selected {
				self.walk(&branches[i], b, v, path, out)?;
			} else {
				let message = format!("expected one of {}", list_values(&values));
				out.push(ValidationError::new(pointer(path, key), keyword, message));
			}
			return Ok(());
		}
		// An object is blamed on the branch that declares most of its properties (the one it was most likely meant
		// for), then on the branch whose first error is deepest.
		let mut best: Option<(Vec<ValidationError>, usize)> = None;
		for s in branches {
			let mut errors = Vec::new();
			self.walk(s, b, v, path, &mut errors)?;
			let Some(first) = errors.first() else { continue };
			let declared = match v.as_object() {
				Some(o) => self.declared_keys(s, b, o)?,
				None => 0,
			};
			let better = best.as_ref().is_none_or(|(best_errors, best_declared)| {
				declared > *best_declared
					|| (declared == *best_declared && js_length(&first.path) > js_length(&best_errors[0].path))
			});
			if better {
				best = Some((errors, declared));
			}
		}
		match best {
			Some((errors, _)) => out.extend(errors),
			None => out.push(ValidationError::new(path, keyword, "matches no alternative")),
		}
		Ok(())
	}

	/// A union branch, through a `$ref` it is.
	fn resolved_branch(&mut self, s: &'d JsonValue, b: &str) -> Result<&'d JsonValue, Stop> {
		let Some((map, reference)) = s
			.as_object()
			.and_then(|m| m.get("$ref").and_then(JsonValue::as_str).map(|r| (m, r)))
		else {
			return Ok(s);
		};
		let base = self.c.r.base_for(map, b);
		Ok(self.c.r.resolve(reference, &base)?.node)
	}

	/// How many of the keys of `o` the branch `s` (through a `$ref`) declares in `properties`.
	fn declared_keys(&mut self, s: &'d JsonValue, b: &str, o: &Map<String, JsonValue>) -> Result<usize, Stop> {
		let node = self.resolved_branch(s, b)?;
		let properties = node.get("properties").and_then(JsonValue::as_object);
		Ok(o.keys()
			.filter(|k| properties.is_some_and(|p| p.contains_key(*k)))
			.count())
	}

	/// The key and tags of a union whose branches all have distinct primitive consts on one property.
	fn tag_of(&mut self, branches: &'d [JsonValue], b: &str) -> Result<Option<(&'d str, Vec<JsonValue>)>, Stop> {
		let mut resolved = Vec::with_capacity(branches.len());
		for s in branches {
			resolved.push(self.resolved_branch(s, b)?);
		}
		let Some(first) = resolved
			.first()
			.and_then(|s| s.get("properties"))
			.and_then(JsonValue::as_object)
		else {
			return Ok(None);
		};
		for (key, _) in js_key_order(first) {
			let values: Option<Vec<JsonValue>> = resolved
				.iter()
				.map(|s| {
					let tag = s
						.get("properties")
						.and_then(JsonValue::as_object)
						.and_then(|p| p.get(key));
					tag.and_then(JsonValue::as_object)
						.and_then(|t| t.get("const"))
						.filter(|c| is_primitive(c))
						.cloned()
				})
				.collect();
			let Some(values) = values else { continue };
			let distinct = values
				.iter()
				.enumerate()
				.all(|(i, value)| values[..i].iter().all(|other| !strict_equal(value, other)));
			if distinct {
				return Ok(Some((key.as_str(), values)));
			}
		}
		Ok(None)
	}

	fn walk_unevaluated(
		&mut self,
		map: &'d Map<String, JsonValue>,
		b: &str,
		v: &JsonValue,
		path: &str,
		out: &mut Vec<ValidationError>,
	) -> Walk {
		// `b` already applies the node's `$id`; the view without it must not apply it again.
		let check = self.c.annotating_finished(View::without(map, SKIP_UNEVALUATED), b)?;
		let mut a = Annotations::default();
		check.run(v, &mut a, &mut self.c.ctx());
		if let (Some(o), Some(unevaluated)) = (v.as_object(), map.get("unevaluatedProperties"))
			&& !a.all_props
		{
			for (k, x) in js_key_order(o) {
				if a.has_prop(k) {
					continue;
				}
				if *unevaluated == JsonValue::Bool(false) {
					out.push(ValidationError::new(
						pointer(path, k),
						"unevaluatedProperties",
						"unknown field",
					));
				} else {
					self.walk(unevaluated, b, x, &pointer(path, k), out)?;
				}
			}
		}
		if let (Some(items), Some(unevaluated)) = (v.as_array(), map.get("unevaluatedItems"))
			&& !a.all_items
		{
			for (i, item) in items.iter().enumerate().skip(a.items) {
				if a.has_index(i) {
					continue;
				}
				if *unevaluated == JsonValue::Bool(false) {
					out.push(ValidationError::new(
						format!("{path}/{i}"),
						"unevaluatedItems",
						"unexpected item",
					));
				} else {
					self.walk(unevaluated, b, item, &format!("{path}/{i}"), out)?;
				}
			}
		}
		Ok(())
	}
}

// Numbers alike: bounds compare as doubles.
fn walk_number(map: &Map<String, JsonValue>, v: f64, path: &str, out: &mut Vec<ValidationError>) {
	let bound = |k: &str| map.get(k).and_then(JsonValue::as_f64);
	if let Some(min) = bound("minimum")
		&& v < min
	{
		out.push(ValidationError::new(
			path,
			"minimum",
			format!("expected >= {}", js(min)),
		));
	}
	if let Some(max) = bound("maximum")
		&& v > max
	{
		out.push(ValidationError::new(
			path,
			"maximum",
			format!("expected <= {}", js(max)),
		));
	}
	if let Some(min) = bound("exclusiveMinimum")
		&& v <= min
	{
		out.push(ValidationError::new(
			path,
			"exclusiveMinimum",
			format!("expected > {}", js(min)),
		));
	}
	if let Some(max) = bound("exclusiveMaximum")
		&& v >= max
	{
		out.push(ValidationError::new(
			path,
			"exclusiveMaximum",
			format!("expected < {}", js(max)),
		));
	}
	if let Some(d) = bound("multipleOf")
		&& !is_multiple(v, d)
	{
		out.push(ValidationError::new(
			path,
			"multipleOf",
			format!("expected a multiple of {}", js(d)),
		));
	}
}

fn js(x: f64) -> String {
	pi_json::js_number_to_string(x)
}

/// A string's length as JS counts it (UTF-16 units), for comparing error paths as TS does.
fn js_length(s: &str) -> usize {
	s.encode_utf16().count()
}

// Enum errors name the allowed values; a long list (an enum of thousands) is cut, so the message stays readable.
const LISTED_VALUES: usize = 20;

/// `values` as JSON, at most `LISTED_VALUES` of them, then how many more there are.
fn list_values(values: &[JsonValue]) -> String {
	if values.len() <= LISTED_VALUES {
		return pi_json::to_js_string(&JsonValue::Array(values.to_vec()));
	}
	let listed = pi_json::to_js_string(&JsonValue::Array(values[..LISTED_VALUES].to_vec()));
	format!(
		"{}, ... and {} more]",
		&listed[..listed.len() - 1],
		values.len() - LISTED_VALUES
	)
}

#[expect(clippy::float_cmp, reason = "the bounds are compared as JS compares them, exactly")]
fn contains_message(min: f64, max: f64) -> String {
	let noun = |n: f64| if n == 1.0 { "item" } else { "items" };
	if max == f64::INFINITY {
		return format!("expected at least {} matching {}", js(min), noun(min));
	}
	if min == 0.0 {
		return format!("expected at most {} matching {}", js(max), noun(max));
	}
	format!("expected between {} and {} matching items", js(min), js(max))
}

fn pointer(path: &str, key: &str) -> String {
	format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"))
}

fn type_name(v: &JsonValue) -> &'static str {
	match v {
		JsonValue::Null => "null",
		JsonValue::Array(_) => "array",
		JsonValue::Number(n) if crate::checks::is_integer(n) => "integer",
		JsonValue::Number(_) => "number",
		JsonValue::String(_) => "string",
		JsonValue::Bool(_) => "boolean",
		JsonValue::Object(_) => "object",
	}
}

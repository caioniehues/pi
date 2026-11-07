//! The compiler (TS `compiler.ts`): a schema to one check, built once. Keywords group by the type they constrain, so
//! a value is classified once per schema, and the common shapes (closed objects, arrays of one item schema, tagged
//! unions, leaf schemas) get one specialized closure each.
//!
//! A Compiler borrows the schema documents. Checks are `'static`: recursive references go through slots of the
//! check context instead of pointing back at the compiler, so a compiled schema keeps no compile-time state alive.
//! Decided difference from TS: the targets of a bookended `$dynamicRef` are compiled when the schema compiles (TS
//! compiles them on first use), so checking never needs the compiler.

use crate::annotations::{
	AnnotatingCheck, ArrayParts, annotate_all, annotate_any, annotate_array, annotate_dependent, annotate_if,
	annotate_object, ignore_annotations, isolate_annotations, scoped_annotating, unevaluated_items,
	unevaluated_properties, with_fresh_annotations,
};
use crate::checks::{
	Bounds, Check, Ctx, Entry, Guard, NumberMode, OpenMembers, and, any_check, bare_type_check, const_check,
	contains_check, declared_object, dependent_required_check, dependent_schemas_check, enum_check, guard_array,
	guard_object, if_check, is_primitive, items_check, js_string, length_check, list_check, masked_any_check,
	not_check, number_check, one_check, open_object, property_count_check, property_names_check, scoped_check,
	strict_equal, string_check, tagged_check, type_check, unique_check,
};
use crate::resolver::{Resolved, Resolver, document_of, identity, resolve_uri};
use crate::types::{CompileError, CompileOptions, Draft, FormatSet};
use pi_json::{JsonValue, js_key_order};
use regress::Regex;
use serde_json::Map;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

/// Why a compile stopped: an error, or a bookended `$dynamicRef` met without dynamic scope tracking (compile again
/// with it, as TS's `RESTART`).
pub(crate) enum Stop {
	Restart,
	Error(CompileError),
}

impl From<CompileError> for Stop {
	fn from(error: CompileError) -> Self {
		Self::Error(error)
	}
}

/// The keywords of a schema object a compile reads: all of them, or the object without some (TS builds those as new
/// objects; a view keeps the subschemas' identities).
#[derive(Clone, Copy)]
pub(crate) struct View<'d> {
	pub(crate) map: &'d Map<String, JsonValue>,
	skip: u8,
}

/// A view without the keywords an annotating check handles itself.
const SKIP_ANNOTATING: u8 = 1;
/// A view without `unevaluatedProperties`, `unevaluatedItems` and `$id` (the explainer's copy of a node).
pub(crate) const SKIP_UNEVALUATED: u8 = 2;

impl<'d> View<'d> {
	/// All keywords of `map`.
	pub(crate) fn whole(map: &'d Map<String, JsonValue>) -> Self {
		Self { map, skip: 0 }
	}

	/// `map` without the keywords of `skip`.
	pub(crate) fn without(map: &'d Map<String, JsonValue>, skip: u8) -> Self {
		Self { map, skip }
	}

	fn skips(&self, keyword: &str) -> bool {
		(self.skip & SKIP_ANNOTATING != 0 && ANNOTATING.contains(&keyword))
			|| (self.skip & SKIP_UNEVALUATED != 0
				&& matches!(keyword, "unevaluatedProperties" | "unevaluatedItems" | "$id"))
	}

	/// The value of `keyword`, unless absent or skipped.
	pub(crate) fn get(&self, keyword: &str) -> Option<&'d JsonValue> {
		if self.skips(keyword) {
			None
		} else {
			self.map.get(keyword)
		}
	}

	/// Whether `keyword` is present and not skipped.
	pub(crate) fn has(&self, keyword: &str) -> bool {
		self.get(keyword).is_some()
	}

	fn keys(&self) -> impl Iterator<Item = &'d str> + '_ {
		self.map.keys().map(String::as_str).filter(|k| !self.skips(k))
	}

	fn str(&self, keyword: &str) -> Option<&'d str> {
		self.get(keyword).and_then(JsonValue::as_str)
	}

	fn array(&self, keyword: &str) -> Option<&'d Vec<JsonValue>> {
		self.get(keyword).and_then(JsonValue::as_array)
	}

	fn object(&self, keyword: &str) -> Option<&'d Map<String, JsonValue>> {
		self.get(keyword).and_then(JsonValue::as_object)
	}

	fn number(&self, keyword: &str) -> Option<f64> {
		self.get(keyword).and_then(JsonValue::as_f64)
	}
}

/// A `$dynamicRef` target table waiting to be filled once the anchors it may name are known.
enum Pending {
	Plain(String, Arc<OnceLock<HashMap<String, Check>>>),
	Annotating(String, Arc<OnceLock<HashMap<String, AnnotatingCheck>>>),
}

/// One compile of a schema. Its caches and indexes are garbage once the root check is built.
pub(crate) struct Compiler<'d> {
	pub(crate) r: Resolver<'d>,
	pub(crate) draft: Draft,
	pub(crate) formats: &'d FormatSet,
	/// Whether checks track the dynamic scope (the schema has a bookended `$dynamicRef`).
	pub(crate) dynamic: bool,
	root: &'d JsonValue,
	cache: HashMap<usize, Check>,
	/// Schema objects being built, with the slot a recursive reference to one took.
	building: HashMap<usize, Option<usize>>,
	slots: Vec<Check>,
	annotating_cache: HashMap<(usize, u8), AnnotatingCheck>,
	annotating_building: HashMap<(usize, u8), Option<usize>>,
	annotating_slots: Vec<AnnotatingCheck>,
	regexes: HashMap<String, Arc<Regex>>,
	pending: Vec<Pending>,
}

impl<'d> Compiler<'d> {
	/// A compiler of `root` with `options`, tracking the dynamic scope or not.
	pub(crate) fn new(root: &'d JsonValue, options: &'d CompileOptions, dynamic: bool) -> Self {
		Self {
			r: Resolver::new(root, options),
			draft: options.draft,
			formats: &options.formats,
			dynamic,
			root,
			cache: HashMap::new(),
			building: HashMap::new(),
			slots: Vec::new(),
			annotating_cache: HashMap::new(),
			annotating_building: HashMap::new(),
			annotating_slots: Vec::new(),
			regexes: HashMap::new(),
			pending: Vec::new(),
		}
	}

	/// The check of the root schema.
	pub(crate) fn root(&mut self) -> Result<Check, Stop> {
		let base = crate::resolver::DEFAULT_BASE;
		self.check_finished(self.root, base)
	}

	/// A check context over this compiler's slots.
	pub(crate) fn ctx(&self) -> Ctx<'_> {
		Ctx::new(&self.slots, &self.annotating_slots)
	}

	/// The slots of the checks built so far (they move into the validator).
	pub(crate) fn into_slots(self) -> (Vec<Check>, Vec<AnnotatingCheck>) {
		(self.slots, self.annotating_slots)
	}

	/// The check of a schema, ready to run: every `$dynamicRef` table it may use is filled.
	pub(crate) fn check_finished(&mut self, node: &'d JsonValue, base: &str) -> Result<Check, Stop> {
		let check = self.check(node, base)?;
		self.finish_dynamic()?;
		Ok(check)
	}

	/// The annotating check of a view, ready to run (the explainer's copy of a node).
	pub(crate) fn annotating_finished(&mut self, view: View<'d>, base: &str) -> Result<AnnotatingCheck, Stop> {
		let check = self.annotating_view(view, base)?;
		self.finish_dynamic()?;
		Ok(check)
	}

	/// The check of a schema (cached by schema object; a recursive reference goes through a slot).
	fn check(&mut self, node: &'d JsonValue, base: &str) -> Result<Check, Stop> {
		let map = match node {
			JsonValue::Bool(true) => return Ok(Check::Always),
			JsonValue::Bool(false) => return Ok(Check::Never),
			JsonValue::Object(map) => map,
			other => return Err(CompileError::Malformed(pi_json::to_js_string(other)).into()),
		};
		let key = identity(map);
		if let Some(cached) = self.cache.get(&key) {
			return Ok(cached.clone());
		}
		if let Some(slot) = self.building.get(&key) {
			let index = slot.unwrap_or(self.slots.len());
			if slot.is_none() {
				self.slots.push(Check::Never);
				self.building.insert(key, Some(index));
			}
			return Ok(Check::Slot(index));
		}
		if let Some(bare) = bare_type(map) {
			return Ok(bare);
		}
		self.building.insert(key, None);
		let base = self.r.base_for(map, base);
		let built = self.build(map, &base);
		let slot = self.building.remove(&key).flatten();
		let check = built?;
		if let Some(index) = slot {
			self.slots[index] = check.clone();
		}
		self.cache.insert(key, check.clone());
		Ok(check)
	}

	fn build(&mut self, map: &'d Map<String, JsonValue>, base: &str) -> Result<Check, Stop> {
		let view = View::whole(map);
		if self.draft == Draft::V7
			&& let Some(reference) = view.str("$ref")
		{
			return self.reference(reference, base);
		}
		if self.draft != Draft::V7 && (view.has("unevaluatedProperties") || view.has("unevaluatedItems")) {
			return Ok(with_fresh_annotations(self.annotating_view(view, base)?));
		}
		let check = self.plain(view, base)?;
		Ok(if self.dynamic && self.r.is_resource(map, base) {
			scoped_check(base, check)
		} else {
			check
		})
	}

	/// Every keyword of `view`, not tracking annotations.
	fn plain(&mut self, view: View<'d>, base: &str) -> Result<Check, Stop> {
		let types: Option<Vec<&JsonValue>> = view.get("type").map(|t| match t {
			JsonValue::Array(items) => items.iter().collect(),
			single => vec![single],
		});
		let single = match types.as_deref() {
			Some([single]) => single.as_str(),
			_ => None,
		};
		let mut parts = Vec::new();
		let type_in_group;
		if single == Some("bytes") {
			// The value model is JSON: no value is bytes.
			parts.push(Check::Never);
			type_in_group = true;
		} else {
			let string = self.string_group(view, single == Some("string"))?;
			let mode = match single {
				Some("number") => NumberMode::Number,
				Some("integer") => NumberMode::Integer,
				Some("bigint") => NumberMode::Bigint,
				_ => NumberMode::Any,
			};
			let number = number_group(view, mode);
			let object = self.object_group(view, base, single == Some("object"))?;
			let array = self.array_group(view, base, single == Some("array"))?;
			type_in_group = match single {
				Some("string") => string.is_some(),
				Some("number" | "integer" | "bigint") => number.is_some(),
				Some("object") => object.is_some(),
				Some("array") => array.is_some(),
				_ => false,
			};
			parts.extend([string, number, object, array].into_iter().flatten());
		}
		if let Some(types) = &types
			&& !type_in_group
		{
			parts.insert(0, type_check(types));
		}
		self.in_place(view, base, &mut parts)?;
		Ok(and(parts))
	}

	// The keywords that apply to the value itself, whatever its type.
	fn in_place(&mut self, view: View<'d>, base: &str, parts: &mut Vec<Check>) -> Result<(), Stop> {
		if let Some(values) = view.array("enum") {
			parts.push(enum_check(values));
		}
		if let Some(value) = view.get("const") {
			parts.push(const_check(value));
		}
		if let Some(all) = view.array("allOf") {
			let checks = all.iter().map(|s| self.check(s, base)).collect::<Result<Vec<_>, _>>()?;
			parts.push(and(checks));
		}
		if let Some(branches) = view.array("anyOf") {
			parts.push(self.union(branches, base, false)?);
		}
		if let Some(branches) = view.array("oneOf") {
			parts.push(self.union(branches, base, true)?);
		}
		if let Some(not) = view.get("not") {
			parts.push(not_check(self.check(not, base)?));
		}
		if let Some(cond) = view.get("if")
			&& (view.has("then") || view.has("else"))
		{
			let then = self.optional_check(view.get("then"), base)?;
			let otherwise = self.optional_check(view.get("else"), base)?;
			parts.push(if_check(self.check(cond, base)?, then, otherwise));
		}
		if let Some(reference) = view.str("$ref") {
			parts.push(self.reference(reference, base)?);
		}
		if self.draft != Draft::V7
			&& let Some(reference) = view.str("$dynamicRef")
		{
			parts.push(self.dynamic_reference(reference, base)?);
		}
		Ok(())
	}

	fn optional_check(&mut self, node: Option<&'d JsonValue>, base: &str) -> Result<Check, Stop> {
		node.map_or(Ok(Check::Always), |node| self.check(node, base))
	}

	fn union(&mut self, branches: &'d [JsonValue], base: &str, exactly_one: bool) -> Result<Check, Stop> {
		if let Some(check) = const_union(branches, exactly_one) {
			return Ok(check);
		}
		if let Some(check) = self.tagged(branches, base)? {
			return Ok(check);
		}
		let checks = branches
			.iter()
			.map(|s| self.check(s, base))
			.collect::<Result<Vec<_>, _>>()?;
		Ok(if exactly_one { one_check(checks) } else { any_of(checks) })
	}

	fn reference(&mut self, reference: &str, base: &str) -> Result<Check, Stop> {
		let target = self.r.resolve(reference, base)?;
		let check = self.check(target.node, &target.base)?;
		Ok(if self.dynamic && target.base != base {
			scoped_check(&target.base, check)
		} else {
			check
		})
	}

	fn dynamic_reference(&mut self, reference: &str, base: &str) -> Result<Check, Stop> {
		let Some((initial, name)) = self.bookended(reference, base)? else {
			return self.reference(reference, base);
		};
		let fallback = self.check(initial.node, &initial.base)?;
		let table = Arc::new(OnceLock::new());
		self.pending.push(Pending::Plain(name, table.clone()));
		Ok(Check::new(move |v, ctx| {
			let found = table
				.get()
				.and_then(|table: &HashMap<String, Check>| ctx.scope.iter().find_map(|uri| table.get(&**uri)));
			found.unwrap_or(&fallback).run(v, ctx)
		}))
	}

	/// The initial target and anchor name of a `$dynamicRef` that resolves dynamically, `None` for a plain ref.
	fn bookended(&mut self, reference: &str, base: &str) -> Result<Option<(Resolved<'d>, String)>, Stop> {
		let initial = self.r.resolve(reference, base)?;
		let uri = resolve_uri(reference, base);
		let name = uri.get(document_of(&uri).len() + 1..).unwrap_or("").to_owned();
		let anchored = initial
			.node
			.get("$dynamicAnchor")
			.and_then(JsonValue::as_str)
			.is_some_and(|anchor| anchor == name);
		if name.is_empty() || name.starts_with('/') || !anchored {
			return Ok(None);
		}
		if !self.dynamic {
			return Err(Stop::Restart);
		}
		Ok(Some((initial, name)))
	}

	/// Fills every pending `$dynamicRef` table with the checks of the `$dynamicAnchor`s of its name, by resource URI.
	/// Compiling a target can index more documents (more anchors) and add tables, so the targets are compiled until
	/// neither grows; then every table is filled from the cache.
	fn finish_dynamic(&mut self) -> Result<(), Stop> {
		if self.pending.is_empty() {
			return Ok(());
		}
		loop {
			let (anchors, tables) = (self.r.dynamic_anchors.len(), self.pending.len());
			for index in 0..tables {
				match &self.pending[index] {
					Pending::Plain(name, _) => {
						let name = name.clone();
						self.targets(&name, Self::check)?;
					}
					Pending::Annotating(name, _) => {
						let name = name.clone();
						self.targets(&name, Self::annotating)?;
					}
				}
			}
			if anchors == self.r.dynamic_anchors.len() && tables == self.pending.len() {
				break;
			}
		}
		// Each table is filled once: it leaves `pending` here, so `set` cannot find it full.
		for pending in std::mem::take(&mut self.pending) {
			match pending {
				Pending::Plain(name, table) => {
					let _ = table.set(self.targets(&name, Self::check)?);
				}
				Pending::Annotating(name, table) => {
					let _ = table.set(self.targets(&name, Self::annotating)?);
				}
			}
		}
		Ok(())
	}

	/// The checks of the `$dynamicAnchor`s named `name`, by the URI of their resource.
	fn targets<T>(
		&mut self,
		name: &str,
		compile: fn(&mut Self, &'d JsonValue, &str) -> Result<T, Stop>,
	) -> Result<HashMap<String, T>, Stop> {
		let mut found = HashMap::new();
		for (uri, target) in self.anchors_named(name) {
			let base = target
				.as_object()
				.and_then(|map| self.r.indexed_base(map))
				.unwrap_or(&uri)
				.to_owned();
			found.insert(uri, compile(self, target, &base)?);
		}
		Ok(found)
	}

	/// The resources (by URI) with a `$dynamicAnchor` named `name`, and its schema.
	fn anchors_named(&self, name: &str) -> Vec<(String, &'d JsonValue)> {
		let suffix = format!("#{name}");
		self.r
			.dynamic_anchors
			.iter()
			.filter_map(|(key, target)| key.strip_suffix(&suffix).map(|uri| (uri.to_owned(), *target)))
			.collect()
	}

	/// anyOf/oneOf whose branches are all objects with a distinct required const on one key: the tag picks the branch.
	fn tagged(&mut self, branches: &'d [JsonValue], base: &str) -> Result<Option<Check>, Stop> {
		if branches.len() < 2 {
			return Ok(None);
		}
		let mut resolved = Vec::with_capacity(branches.len());
		for branch in branches {
			let Some(map) = branch.as_object() else { return Ok(None) };
			let only_ref = map.len() == 1;
			let node = match map.get("$ref").and_then(JsonValue::as_str).filter(|_| only_ref) {
				Some(reference) => {
					let base = self.r.base_for(map, base);
					self.r.resolve(reference, &base)?.node
				}
				None => branch,
			};
			let Some(node) = node.as_object() else { return Ok(None) };
			resolved.push(node);
		}
		let Some(first) = resolved[0].get("properties").and_then(JsonValue::as_object) else {
			return Ok(None);
		};
		for (key, _) in js_key_order(first) {
			let tags: Option<Vec<JsonValue>> = resolved.iter().map(|s| tag_of(s, key).cloned()).collect();
			let Some(tags) = tags else { continue };
			let distinct = tags
				.iter()
				.enumerate()
				.all(|(i, tag)| tags[..i].iter().all(|other| !strict_equal(tag, other)));
			if !distinct {
				continue;
			}
			let checks = branches
				.iter()
				.map(|s| self.check(s, base))
				.collect::<Result<Vec<_>, _>>()?;
			return Ok(Some(tagged_check(key.clone(), tags, checks)));
		}
		Ok(None)
	}

	/// The regular expression of a `pattern` source, shared within one compile. Patterns are ECMAScript regular
	/// expressions with the `u` flag; a source that is invalid with it (some older schemas escape characters `u`
	/// rejects) compiles without it.
	pub(crate) fn regex(&mut self, source: &str) -> Result<Arc<Regex>, CompileError> {
		if let Some(re) = self.regexes.get(source) {
			return Ok(re.clone());
		}
		let re = Regex::with_flags(source, "u")
			.or_else(|_| Regex::with_flags(source, ""))
			.map_err(|error| CompileError::InvalidPattern {
				pattern: source.to_owned(),
				reason: error.to_string(),
			})?;
		let re = Arc::new(re);
		self.regexes.insert(source.to_owned(), re.clone());
		Ok(re)
	}

	fn string_group(&mut self, view: View<'d>, strict: bool) -> Result<Option<Check>, Stop> {
		let min = view.number("minLength").unwrap_or(0.0);
		let max = view.number("maxLength").unwrap_or(f64::INFINITY);
		let re = view.str("pattern").map(|p| self.regex(p)).transpose()?;
		let format = view.str("format").and_then(|name| self.formats.get(name)).cloned();
		if min == 0.0 && max == f64::INFINITY && re.is_none() && format.is_none() {
			return Ok(None);
		}
		Ok(Some(string_check(strict, min, max, re, format)))
	}

	fn object_group(&mut self, view: View<'d>, base: &str, strict: bool) -> Result<Option<Check>, Stop> {
		let only_core = OBJECT_EXTRAS.iter().all(|k| !view.has(k));
		// A schema with only the core object keywords gets one closure that also does the type guard.
		let guard = match (only_core, strict) {
			(true, true) => Guard::Strict,
			(true, false) => Guard::Loose,
			(false, _) => Guard::None,
		};
		let core = self.object_core(view, base, guard)?;
		if core.is_some() && only_core {
			return Ok(core);
		}
		let mut parts: Vec<Check> = core.into_iter().collect();
		if let Some(names) = view.get("propertyNames") {
			parts.push(property_names_check(self.check(names, base)?));
		}
		let min_properties = view.number("minProperties");
		let max_properties = view.number("maxProperties");
		if min_properties.is_some() || max_properties.is_some() {
			parts.push(property_count_check(
				min_properties.unwrap_or(0.0),
				max_properties.unwrap_or(f64::INFINITY),
			));
		}
		let mut dependent_required = Vec::new();
		let mut dependent_schemas = Vec::new();
		if self.draft != Draft::V7 {
			for (k, list) in view.object("dependentRequired").into_iter().flat_map(js_key_order) {
				if let Some(list) = list.as_array() {
					dependent_required.push((k.clone(), list.iter().map(js_string).collect()));
				}
			}
			for (k, s) in view.object("dependentSchemas").into_iter().flat_map(js_key_order) {
				dependent_schemas.push((k.clone(), self.check(s, base)?));
			}
		}
		for (k, dependency) in view.object("dependencies").into_iter().flat_map(js_key_order) {
			match dependency.as_array() {
				Some(list) => dependent_required.push((k.clone(), list.iter().map(js_string).collect())),
				None => dependent_schemas.push((k.clone(), self.check(dependency, base)?)),
			}
		}
		if !dependent_required.is_empty() {
			parts.push(dependent_required_check(dependent_required));
		}
		if !dependent_schemas.is_empty() {
			parts.push(dependent_schemas_check(dependent_schemas));
		}
		if parts.is_empty() {
			return Ok(strict.then_some(Check::Types(OBJECT_BIT)));
		}
		Ok(Some(guard_object(and(parts), strict)))
	}

	fn object_core(&mut self, view: View<'d>, base: &str, guard: Guard) -> Result<Option<Check>, Stop> {
		static EMPTY: OnceLock<Map<String, JsonValue>> = OnceLock::new();
		let empty = EMPTY.get_or_init(Map::new);
		let properties = view.object("properties").unwrap_or(empty);
		let required: Vec<&str> = view
			.array("required")
			.map(|list| list.iter().filter_map(JsonValue::as_str).collect())
			.unwrap_or_default();
		let patterns = view.object("patternProperties").unwrap_or(empty);
		let additional = view.get("additionalProperties");
		let open = additional.is_none_or(|a| *a == JsonValue::Bool(true));
		if properties.is_empty() && required.is_empty() && patterns.is_empty() && open {
			return Ok(None);
		}
		let mut entries = Vec::with_capacity(properties.len());
		for (k, s) in js_key_order(properties) {
			entries.push(Entry {
				key: k.clone(),
				check: self.check(s, base)?,
				required: required.contains(&k.as_str()),
			});
		}
		let extra_required: Vec<String> = required
			.iter()
			.filter(|k| !properties.contains_key(**k))
			.map(|k| (*k).to_owned())
			.collect();
		let mut pats = Vec::with_capacity(patterns.len());
		for (p, s) in js_key_order(patterns) {
			pats.push((self.regex(p)?, self.check(s, base)?));
		}
		let closed = additional == Some(&JsonValue::Bool(false));
		let extra = match additional {
			Some(a) if !open => Some(self.check(a, base)?),
			_ => None,
		};
		if pats.is_empty() && extra_required.is_empty() && (extra.is_none() || closed) {
			let required_count = entries.iter().filter(|e| e.required).count();
			return Ok(Some(declared_object(entries, required_count, closed, guard)));
		}
		let members = OpenMembers {
			extra_required,
			patterns: pats,
			additional: extra,
		};
		Ok(Some(open_object(entries, members, guard)))
	}

	fn array_group(&mut self, view: View<'d>, base: &str, strict: bool) -> Result<Option<Check>, Stop> {
		let (prefix, rest) = item_schemas(view, self.draft);
		let prefix_checks = prefix
			.iter()
			.map(|s| self.check(s, base))
			.collect::<Result<Vec<_>, _>>()?;
		let rest_check = match rest {
			Some(rest) if *rest != JsonValue::Bool(true) => Some(self.check(rest, base)?),
			_ => None,
		};
		let min_items = view.number("minItems");
		let max_items = view.number("maxItems");
		let unique = view.get("uniqueItems") == Some(&JsonValue::Bool(true));
		let has_items = !prefix_checks.is_empty() || rest_check.is_some();
		if !view.has("contains") && !unique && has_items {
			// The common array: one closure for the guard, the length bounds and the items.
			let guard = if strict { Guard::Strict } else { Guard::Loose };
			let (min, max) = (min_items.unwrap_or(0.0), max_items.unwrap_or(f64::INFINITY));
			return Ok(Some(list_check(guard, min, max, prefix_checks, rest_check)));
		}
		let mut parts = Vec::new();
		if has_items {
			parts.push(items_check(prefix_checks, rest_check));
		}
		if min_items.is_some() || max_items.is_some() {
			parts.push(length_check(
				min_items.unwrap_or(0.0),
				max_items.unwrap_or(f64::INFINITY),
			));
		}
		if let Some(contains) = view.get("contains") {
			let (min, max) = self.contains_bounds(view);
			parts.push(contains_check(self.check(contains, base)?, min, max));
		}
		if unique {
			parts.push(unique_check());
		}
		if parts.is_empty() {
			return Ok(strict.then_some(Check::Types(ARRAY_BIT)));
		}
		Ok(Some(guard_array(and(parts), strict)))
	}

	/// `minContains` and `maxContains`, which draft 7 does not have.
	pub(crate) fn contains_bounds(&self, view: View<'_>) -> (f64, f64) {
		if self.draft == Draft::V7 {
			return (1.0, f64::INFINITY);
		}
		(
			view.number("minContains").unwrap_or(1.0),
			view.number("maxContains").unwrap_or(f64::INFINITY),
		)
	}

	/// The annotating check of a schema evaluated in place (only below `unevaluatedProperties`/`unevaluatedItems`).
	fn annotating(&mut self, node: &'d JsonValue, base: &str) -> Result<AnnotatingCheck, Stop> {
		match node {
			JsonValue::Bool(true) => Ok(AnnotatingCheck::True),
			JsonValue::Bool(false) => Ok(AnnotatingCheck::False),
			JsonValue::Object(map) => self.annotating_view(View::whole(map), base),
			other => Err(CompileError::Malformed(pi_json::to_js_string(other)).into()),
		}
	}

	fn annotating_view(&mut self, view: View<'d>, base: &str) -> Result<AnnotatingCheck, Stop> {
		let key = (identity(view.map), view.skip);
		if let Some(cached) = self.annotating_cache.get(&key) {
			return Ok(cached.clone());
		}
		if let Some(slot) = self.annotating_building.get(&key) {
			let index = slot.unwrap_or(self.annotating_slots.len());
			if slot.is_none() {
				self.annotating_slots.push(AnnotatingCheck::False);
				self.annotating_building.insert(key, Some(index));
			}
			return Ok(AnnotatingCheck::Slot(index));
		}
		self.annotating_building.insert(key, None);
		// A view without `$id` is a copy of its node in TS: never indexed, so its base is the given one.
		let base = if view.skip == 0 {
			self.r.base_for(view.map, base)
		} else {
			base.to_owned()
		};
		let built = self.build_annotating(view, &base);
		let slot = self.annotating_building.remove(&key).flatten();
		let check = built?;
		if let Some(index) = slot {
			self.annotating_slots[index] = check.clone();
		}
		self.annotating_cache.insert(key, check.clone());
		Ok(check)
	}

	fn build_annotating(&mut self, view: View<'d>, base: &str) -> Result<AnnotatingCheck, Stop> {
		let annotates = view.keys().any(|k| ANNOTATING.contains(&k));
		let plain = self.plain(View::without(view.map, view.skip | SKIP_ANNOTATING), base)?;
		let mut parts = Vec::new();
		if annotates {
			self.annotating_parts(view, base, &mut parts)?;
		}
		let all = if parts.is_empty() {
			ignore_annotations(plain)
		} else {
			annotate_all(plain, parts)
		};
		let own = if view.has("unevaluatedProperties") || view.has("unevaluatedItems") {
			isolate_annotations(all)
		} else {
			all
		};
		let resource = view.skip == 0 && self.r.is_resource(view.map, base);
		Ok(if self.dynamic && resource {
			scoped_annotating(base, own)
		} else {
			own
		})
	}

	fn annotating_parts(&mut self, view: View<'d>, base: &str, parts: &mut Vec<AnnotatingCheck>) -> Result<(), Stop> {
		if view.has("properties") || view.has("patternProperties") || view.has("additionalProperties") {
			let mut props = Vec::new();
			for (k, s) in view.object("properties").into_iter().flat_map(js_key_order) {
				props.push((k.clone(), self.check(s, base)?));
			}
			let mut patterns = Vec::new();
			for (p, s) in view.object("patternProperties").into_iter().flat_map(js_key_order) {
				patterns.push((self.regex(p)?, self.check(s, base)?));
			}
			let extra = view
				.get("additionalProperties")
				.map(|s| self.check(s, base))
				.transpose()?;
			parts.push(annotate_object(props, patterns, extra));
		}
		if view.has("prefixItems") || view.has("items") || view.has("contains") {
			let prefix = view.array("prefixItems").or_else(|| view.array("items"));
			let rest = if view.array("items").is_some() {
				view.get("additionalItems")
			} else {
				view.get("items")
			};
			let prefix = prefix
				.into_iter()
				.flatten()
				.map(|s| self.check(s, base))
				.collect::<Result<Vec<_>, _>>()?;
			parts.push(annotate_array(ArrayParts {
				prefix,
				rest: rest.map(|s| self.check(s, base)).transpose()?,
				contains: view.get("contains").map(|s| self.check(s, base)).transpose()?,
				min_contains: view.number("minContains").unwrap_or(1.0),
				max_contains: view.number("maxContains").unwrap_or(f64::INFINITY),
			}));
		}
		for s in view.array("allOf").into_iter().flatten() {
			parts.push(self.annotating(s, base)?);
		}
		for (keyword, exactly_one) in [("anyOf", false), ("oneOf", true)] {
			if let Some(branches) = view.array(keyword) {
				let branches = branches
					.iter()
					.map(|s| self.annotating(s, base))
					.collect::<Result<Vec<_>, _>>()?;
				parts.push(annotate_any(branches, exactly_one));
			}
		}
		if let Some(cond) = view.get("if") {
			let then = view.get("then").map(|s| self.annotating(s, base)).transpose()?;
			let otherwise = view.get("else").map(|s| self.annotating(s, base)).transpose()?;
			parts.push(annotate_if(self.annotating(cond, base)?, then, otherwise));
		}
		if let Some(reference) = view.str("$ref") {
			parts.push(self.annotating_reference(reference, base)?);
		}
		if let Some(reference) = view.str("$dynamicRef") {
			parts.push(self.annotating_dynamic_reference(reference, base)?);
		}
		for (k, s) in view.object("dependentSchemas").into_iter().flat_map(js_key_order) {
			parts.push(annotate_dependent(k.clone(), self.annotating(s, base)?));
		}
		if let Some(s) = view.get("unevaluatedProperties") {
			parts.push(unevaluated_properties(self.check(s, base)?));
		}
		if let Some(s) = view.get("unevaluatedItems") {
			parts.push(unevaluated_items(self.check(s, base)?));
		}
		Ok(())
	}

	fn annotating_reference(&mut self, reference: &str, base: &str) -> Result<AnnotatingCheck, Stop> {
		let target = self.r.resolve(reference, base)?;
		let check = self.annotating(target.node, &target.base)?;
		Ok(if self.dynamic && target.base != base {
			scoped_annotating(&target.base, check)
		} else {
			check
		})
	}

	fn annotating_dynamic_reference(&mut self, reference: &str, base: &str) -> Result<AnnotatingCheck, Stop> {
		let Some((initial, name)) = self.bookended(reference, base)? else {
			return self.annotating_reference(reference, base);
		};
		let fallback = self.annotating(initial.node, &initial.base)?;
		let table = Arc::new(OnceLock::new());
		self.pending.push(Pending::Annotating(name, table.clone()));
		Ok(AnnotatingCheck::Fn(Arc::new(move |v, a, ctx| {
			let found = table.get().and_then(|table: &HashMap<String, AnnotatingCheck>| {
				ctx.scope.iter().find_map(|uri| table.get(&**uri))
			});
			found.unwrap_or(&fallback).run(v, a, ctx)
		})))
	}
}

const OBJECT_BIT: u16 = 64;
const ARRAY_BIT: u16 = 32;

/// The check of a schema that is only a `type` (with annotations), `Always` for one with no assertion at all; `None`
/// for every other schema. These are the most common subschemas and need no cache entry.
fn bare_type(map: &Map<String, JsonValue>) -> Option<Check> {
	let mut ty = None;
	for (k, v) in map {
		if k == "type" {
			ty = Some(v);
		} else if !NEUTRAL_KEYWORDS.contains(&k.as_str()) {
			return None;
		}
	}
	match ty {
		None => Some(Check::Always),
		Some(JsonValue::String(name)) => bare_type_check(name),
		Some(_) => None,
	}
}

/// Keywords that never affect a check.
pub(crate) const NEUTRAL_KEYWORDS: [&str; 16] = [
	"$schema",
	"$comment",
	"title",
	"description",
	"default",
	"examples",
	"deprecated",
	"readOnly",
	"writeOnly",
	"$defs",
	"definitions",
	"contentEncoding",
	"contentMediaType",
	"minContains",
	"maxContains",
	"$vocabulary",
];

// The object keywords besides the core (properties, required, patternProperties, additionalProperties).
const OBJECT_EXTRAS: [&str; 6] = [
	"propertyNames",
	"minProperties",
	"maxProperties",
	"dependentRequired",
	"dependentSchemas",
	"dependencies",
];

// The keywords an annotating check handles itself; every other keyword compiles into its plain part.
const ANNOTATING: [&str; 20] = [
	"properties",
	"patternProperties",
	"additionalProperties",
	"prefixItems",
	"items",
	"additionalItems",
	"contains",
	"minContains",
	"maxContains",
	"allOf",
	"anyOf",
	"oneOf",
	"if",
	"then",
	"else",
	"$ref",
	"$dynamicRef",
	"dependentSchemas",
	"unevaluatedProperties",
	"unevaluatedItems",
];

/// The schemas of an array's items: the prefix (a draft-7 `items` array, or `prefixItems` outside draft 7), and the
/// schema of the items after it (`additionalItems` after an `items` array, else `items`).
pub(crate) fn item_schemas(view: View<'_>, draft: Draft) -> (&[JsonValue], Option<&JsonValue>) {
	if let Some(items) = view.array("items") {
		return (items, view.get("additionalItems"));
	}
	let prefix = view.array("prefixItems").filter(|_| draft != Draft::V7);
	(prefix.map_or(&[], Vec::as_slice), view.get("items"))
}

/// The number keywords; `None` when there are none.
fn number_group(view: View<'_>, mode: NumberMode) -> Option<Check> {
	let min = view.number("minimum");
	let max = view.number("maximum");
	let xmin = view.number("exclusiveMinimum");
	let xmax = view.number("exclusiveMaximum");
	let multiple_of = view.number("multipleOf");
	if [min, max, xmin, xmax, multiple_of].iter().all(Option::is_none) {
		return None;
	}
	let bounds = Bounds {
		min: min.unwrap_or(f64::NEG_INFINITY),
		max: max.unwrap_or(f64::INFINITY),
		xmin: xmin.unwrap_or(f64::NEG_INFINITY),
		xmax: xmax.unwrap_or(f64::INFINITY),
	};
	Some(number_check(mode, bounds, multiple_of))
}

/// anyOf/oneOf whose branches are all `{ const: primitive }`: an enum (distinct values make oneOf the same).
fn const_union(branches: &[JsonValue], exactly_one: bool) -> Option<Check> {
	let mut values: Vec<JsonValue> = Vec::with_capacity(branches.len());
	for branch in branches {
		let map = branch.as_object()?;
		let value = map.get("const")?;
		if map
			.keys()
			.any(|k| k != "const" && !NEUTRAL_KEYWORDS.contains(&k.as_str()))
			|| !is_primitive(value)
		{
			return None;
		}
		if exactly_one && values.iter().any(|v| strict_equal(v, value)) {
			return None;
		}
		values.push(value.clone());
	}
	Some(enum_check(&values))
}

/// `anyOf`: its bare-type branches (`nullable` gives one) run as one type test.
fn any_of(checks: Vec<Check>) -> Check {
	let mut mask = 0;
	let mut rest = Vec::new();
	for check in checks {
		match check {
			Check::Types(bits) => mask |= bits,
			other => rest.push(other),
		}
	}
	if mask == 0 {
		any_check(rest)
	} else {
		masked_any_check(mask, rest)
	}
}

// The tag of a tagged-union branch: a primitive const on a required property of an object schema.
fn tag_of<'a>(branch: &'a Map<String, JsonValue>, key: &str) -> Option<&'a JsonValue> {
	if branch.get("type").and_then(JsonValue::as_str) != Some("object") {
		return None;
	}
	let required = branch.get("required").and_then(JsonValue::as_array)?;
	if !required.iter().any(|k| k.as_str() == Some(key)) {
		return None;
	}
	let tag = branch
		.get("properties")?
		.as_object()?
		.get(key)?
		.as_object()?
		.get("const")?;
	is_primitive(tag).then_some(tag)
}

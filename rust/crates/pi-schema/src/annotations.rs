//! Annotation tracking for `unevaluatedProperties` and `unevaluatedItems` (TS `annotations.ts`): annotating checks
//! record which properties and items a schema evaluated. Only schemas that use those keywords compile into these
//! checks, and only they allocate while checking (the sets of evaluated names and indexes).

use crate::checks::{Check, Ctx, regex_test};
use pi_json::JsonValue;
use regress::Regex;
use std::collections::HashSet;
use std::sync::Arc;

/// What a schema evaluated: property names (or all of them), the evaluated item prefix, items matched by `contains`.
#[derive(Default)]
pub(crate) struct Annotations<'v> {
	pub(crate) props: Option<HashSet<&'v str>>,
	pub(crate) all_props: bool,
	pub(crate) items: usize,
	pub(crate) all_items: bool,
	pub(crate) indexes: Option<HashSet<usize>>,
}

impl<'v> Annotations<'v> {
	/// Whether the property `key` was evaluated.
	pub(crate) fn has_prop(&self, key: &str) -> bool {
		self.props.as_ref().is_some_and(|props| props.contains(key))
	}

	/// Whether the item at `index` was matched by `contains`.
	pub(crate) fn has_index(&self, index: usize) -> bool {
		self.indexes.as_ref().is_some_and(|indexes| indexes.contains(&index))
	}

	fn merge(&mut self, other: Annotations<'v>) {
		for key in other.props.into_iter().flatten() {
			self.add_prop(key);
		}
		self.all_props |= other.all_props;
		self.items = self.items.max(other.items);
		self.all_items |= other.all_items;
		for index in other.indexes.into_iter().flatten() {
			self.add_index(index);
		}
	}

	fn add_prop(&mut self, key: &'v str) {
		self.props.get_or_insert_with(HashSet::new).insert(key);
	}

	fn add_index(&mut self, index: usize) {
		self.indexes.get_or_insert_with(HashSet::new).insert(index);
	}
}

/// The closure of an annotating check.
pub(crate) type AnnotatingFn = dyn for<'v> Fn(&'v JsonValue, &mut Annotations<'v>, &mut Ctx<'_>) -> bool + Send + Sync;

/// A check that records what it evaluated into its annotations when it passes.
#[derive(Clone)]
pub(crate) enum AnnotatingCheck {
	/// The schema `true`.
	True,
	/// The schema `false`.
	False,
	/// A recursive reference: the annotating check in that slot of the context.
	Slot(usize),
	/// Any other annotating check.
	Fn(Arc<AnnotatingFn>),
}

impl AnnotatingCheck {
	fn new(
		check: impl for<'v> Fn(&'v JsonValue, &mut Annotations<'v>, &mut Ctx<'_>) -> bool + Send + Sync + 'static,
	) -> Self {
		Self::Fn(Arc::new(check))
	}

	/// Whether `value` passes; when it does, what it evaluated is in `a`.
	pub(crate) fn run<'v>(&self, value: &'v JsonValue, a: &mut Annotations<'v>, ctx: &mut Ctx<'_>) -> bool {
		match self {
			Self::True => true,
			Self::False => false,
			Self::Slot(index) => {
				let slots = ctx.annotating;
				slots[*index].run(value, a, ctx)
			}
			Self::Fn(check) => check(value, a, ctx),
		}
	}
}

/// A plain check in annotating position: it evaluates nothing that unevaluated* sees.
pub(crate) fn ignore_annotations(check: Check) -> AnnotatingCheck {
	AnnotatingCheck::new(move |v, _, ctx| check.run(v, ctx))
}

/// An annotating check run as a plain check, with fresh annotations.
pub(crate) fn with_fresh_annotations(check: AnnotatingCheck) -> Check {
	Check::new(move |v, ctx| check.run(v, &mut Annotations::default(), ctx))
}

/// `check` with `uri` pushed onto the dynamic scope while it runs.
pub(crate) fn scoped_annotating(uri: &str, check: AnnotatingCheck) -> AnnotatingCheck {
	let uri: Arc<str> = Arc::from(uri);
	AnnotatingCheck::new(move |v, a, ctx| {
		ctx.scope.push(uri.clone());
		let ok = check.run(v, a, ctx);
		ctx.scope.pop();
		ok
	})
}

/// unevaluated* sees only the annotations of its own schema, never those of its siblings in the parent.
pub(crate) fn isolate_annotations(check: AnnotatingCheck) -> AnnotatingCheck {
	AnnotatingCheck::new(move |v, a, ctx| {
		let mut own = Annotations::default();
		if !check.run(v, &mut own, ctx) {
			return false;
		}
		a.merge(own);
		true
	})
}

/// The non-annotating keywords, then each annotating part in order.
pub(crate) fn annotate_all(plain: Check, parts: Vec<AnnotatingCheck>) -> AnnotatingCheck {
	AnnotatingCheck::new(move |v, a, ctx| plain.run(v, ctx) && parts.iter().all(|part| part.run(v, a, ctx)))
}

/// `properties`, `patternProperties` and `additionalProperties`.
pub(crate) fn annotate_object(
	props: Vec<(String, Check)>,
	patterns: Vec<(Arc<Regex>, Check)>,
	additional: Option<Check>,
) -> AnnotatingCheck {
	let declared: HashSet<String> = props.iter().map(|(k, _)| k.clone()).collect();
	AnnotatingCheck::new(move |v, a, ctx| {
		let Some(o) = v.as_object() else { return true };
		let mut seen: Vec<&str> = Vec::new();
		for (k, check) in &props {
			let Some((key, x)) = o.get_key_value(k) else { continue };
			if !check.run(x, ctx) {
				return false;
			}
			seen.push(key);
		}
		for (k, x) in o {
			let mut matched = declared.contains(k);
			for (re, check) in &patterns {
				if regex_test(re, k) {
					if !check.run(x, ctx) {
						return false;
					}
					matched = true;
					seen.push(k);
				}
			}
			if let Some(extra) = additional.as_ref().filter(|_| !matched) {
				if !extra.run(x, ctx) {
					return false;
				}
				seen.push(k);
			}
		}
		for key in seen {
			a.add_prop(key);
		}
		true
	})
}

/// The array keywords an annotating check evaluates: `prefixItems` (or a draft-7 `items` array), the items after
/// them, and `contains` with its bounds.
pub(crate) struct ArrayParts {
	pub(crate) prefix: Vec<Check>,
	pub(crate) rest: Option<Check>,
	pub(crate) contains: Option<Check>,
	pub(crate) min_contains: f64,
	pub(crate) max_contains: f64,
}

/// `prefixItems`/`items`/`additionalItems` and `contains` with its bounds.
pub(crate) fn annotate_array(parts: ArrayParts) -> AnnotatingCheck {
	AnnotatingCheck::new(move |v, a, ctx| {
		let Some(items) = v.as_array() else { return true };
		let m = items.len().min(parts.prefix.len());
		if !items
			.iter()
			.zip(&parts.prefix)
			.all(|(item, check)| check.run(item, ctx))
		{
			return false;
		}
		if let Some(rest) = &parts.rest {
			if !items.iter().skip(parts.prefix.len()).all(|item| rest.run(item, ctx)) {
				return false;
			}
			a.all_items = true;
		}
		a.items = a.items.max(m);
		if let Some(contains) = &parts.contains {
			let mut n = 0.0;
			for (index, item) in items.iter().enumerate() {
				if contains.run(item, ctx) {
					n += 1.0;
					a.add_index(index);
				}
			}
			if n < parts.min_contains || n > parts.max_contains {
				return false;
			}
		}
		true
	})
}

/// `anyOf` (or `oneOf` when `exactly_one`): every passing branch contributes its annotations.
pub(crate) fn annotate_any(branches: Vec<AnnotatingCheck>, exactly_one: bool) -> AnnotatingCheck {
	AnnotatingCheck::new(move |v, a, ctx| {
		let mut n = 0;
		for branch in &branches {
			let mut own = Annotations::default();
			if branch.run(v, &mut own, ctx) {
				n += 1;
				a.merge(own);
			}
		}
		if exactly_one { n == 1 } else { n > 0 }
	})
}

/// `if`, `then` and `else`: a passing `if` contributes its annotations even without `then`.
pub(crate) fn annotate_if(
	cond: AnnotatingCheck,
	then: Option<AnnotatingCheck>,
	otherwise: Option<AnnotatingCheck>,
) -> AnnotatingCheck {
	AnnotatingCheck::new(move |v, a, ctx| {
		let mut own = Annotations::default();
		if cond.run(v, &mut own, ctx) {
			a.merge(own);
			return then.as_ref().is_none_or(|then| then.run(v, a, ctx));
		}
		otherwise.as_ref().is_none_or(|otherwise| otherwise.run(v, a, ctx))
	})
}

/// One `dependentSchemas` entry.
pub(crate) fn annotate_dependent(key: String, check: AnnotatingCheck) -> AnnotatingCheck {
	AnnotatingCheck::new(move |v, a, ctx| v.as_object().is_none_or(|o| !o.contains_key(&key)) || check.run(v, a, ctx))
}

/// `unevaluatedProperties`: every property no other keyword evaluated passes `check`.
pub(crate) fn unevaluated_properties(check: Check) -> AnnotatingCheck {
	AnnotatingCheck::new(move |v, a, ctx| {
		let Some(o) = v.as_object() else { return true };
		if a.all_props {
			return true;
		}
		if !o.iter().all(|(k, x)| a.has_prop(k) || check.run(x, ctx)) {
			return false;
		}
		a.all_props = true;
		true
	})
}

/// `unevaluatedItems`: every item no other keyword evaluated passes `check`.
pub(crate) fn unevaluated_items(check: Check) -> AnnotatingCheck {
	AnnotatingCheck::new(move |v, a, ctx| {
		let Some(items) = v.as_array() else { return true };
		if a.all_items {
			return true;
		}
		let evaluated = a.items;
		if !items
			.iter()
			.enumerate()
			.skip(evaluated)
			.all(|(i, item)| a.has_index(i) || check.run(item, ctx))
		{
			return false;
		}
		a.all_items = true;
		true
	})
}

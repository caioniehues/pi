//! `compile()` (TS `validator.ts`): a schema to a Validator, whose `check` is the fast boolean path and whose
//! `errors` explains failures; with `coerce` the validator also coerces loosely typed input.
//!
//! Decided difference from TS: TS keeps the explainer and the coercer it builds on first use; here `errors()`,
//! `coerce()` and `parse()` compile the schema again for each call (a few microseconds for a tool schema), because
//! their compiler borrows the schema the validator owns. `check()` never compiles and never allocates for schemas
//! without the keywords `checks.rs` names.

use crate::annotations::AnnotatingCheck;
use crate::checks::{Check, Ctx};
use crate::coerce::Coercer;
use crate::compiler::{Compiler, Stop};
use crate::explain::Explainer;
use crate::types::{CompileError, CompileOptions, ParseResult, ValidationError};
use pi_json::JsonValue;
use std::ops::Deref;
use std::sync::Arc;

/// Compiles `schema` into a validator. Fails if the schema is malformed or a `$ref` names nothing (register other
/// documents with `options.schemas`). With `options.coerce` the validator also coerces and parses.
pub fn compile(schema: &JsonValue, options: CompileOptions) -> Result<Compiled, CompileError> {
	let coerce = options.coerce;
	let validator = Validator::new(schema, options)?;
	Ok(if coerce {
		Compiled::Coercing(CoercingValidator { validator })
	} else {
		Compiled::Plain(validator)
	})
}

/// A compiled schema: a plain validator, or one that also coerces (`CompileOptions::coerce`).
#[derive(Clone)]
pub enum Compiled {
	/// Compiled without `coerce`.
	Plain(Validator),
	/// Compiled with `coerce`.
	Coercing(CoercingValidator),
}

impl Compiled {
	/// The coercing validator, when compiled with `coerce`.
	pub fn coercing(&self) -> Option<&CoercingValidator> {
		match self {
			Self::Plain(_) => None,
			Self::Coercing(validator) => Some(validator),
		}
	}

	/// The coercing validator, when compiled with `coerce`.
	pub fn into_coercing(self) -> Option<CoercingValidator> {
		match self {
			Self::Plain(_) => None,
			Self::Coercing(validator) => Some(validator),
		}
	}
}

impl Deref for Compiled {
	type Target = Validator;

	fn deref(&self) -> &Validator {
		match self {
			Self::Plain(validator) => validator,
			Self::Coercing(validator) => &validator.validator,
		}
	}
}

/// The root check of `source` and the slots its checks use, tracking the dynamic scope or not.
fn compile_root(source: &Source, dynamic: bool) -> (Result<Check, Stop>, (Vec<Check>, Vec<AnnotatingCheck>)) {
	let mut compiler = Compiler::new(&source.schema, &source.options, dynamic);
	let root = compiler.root();
	(root, compiler.into_slots())
}

/// The schema and options a validator was compiled from; the errors and coercion compilers borrow them.
struct Source {
	schema: JsonValue,
	options: CompileOptions,
}

/// A compiled schema.
#[derive(Clone)]
pub struct Validator {
	root: Check,
	slots: Arc<[Check]>,
	annotating: Arc<[AnnotatingCheck]>,
	source: Arc<Source>,
	/// Whether the schema compiled with dynamic scope tracking (it has a bookended `$dynamicRef`).
	dynamic: bool,
}

impl Validator {
	/// Compiles `schema` (ignoring `options.coerce`).
	pub fn new(schema: &JsonValue, options: CompileOptions) -> Result<Self, CompileError> {
		let source = Arc::new(Source {
			schema: schema.clone(),
			options,
		});
		// Most schemas compile without tracking the dynamic scope; one with a bookended $dynamicRef compiles again.
		let (root, compiler, dynamic) = match compile_root(&source, false) {
			(Err(Stop::Restart), _) => {
				let (root, compiler) = compile_root(&source, true);
				(root, compiler, true)
			}
			(root, compiler) => (root, compiler, false),
		};
		let root = match root {
			Ok(root) => root,
			Err(Stop::Error(error)) => return Err(error),
			// invariant: only a compiler that does not track the dynamic scope asks for a restart (`bookended`).
			Err(Stop::Restart) => unreachable!("a compiler tracking the dynamic scope asked for a restart"),
		};
		let (slots, annotating) = compiler;
		Ok(Self {
			root,
			slots: slots.into(),
			annotating: annotating.into(),
			source,
			dynamic,
		})
	}

	/// Whether `value` is valid.
	pub fn check(&self, value: &JsonValue) -> bool {
		self.root.run(value, &mut Ctx::new(&self.slots, &self.annotating))
	}

	/// Why `value` is invalid: one error per failing keyword, empty when it is valid.
	pub fn errors(&self, value: &JsonValue) -> Vec<ValidationError> {
		if self.check(value) {
			return Vec::new();
		}
		let mut compiler = self.compiler();
		Explainer::new(&mut compiler, &self.source.schema).errors(value)
	}

	/// `Ok` when `value` is valid, else its errors: `check()` and `errors()` in one result, without coercion.
	pub fn validate(&self, value: &JsonValue) -> Result<(), Vec<ValidationError>> {
		if self.check(value) {
			Ok(())
		} else {
			Err(self.errors(value))
		}
	}

	fn compiler(&self) -> Compiler<'_> {
		Compiler::new(&self.source.schema, &self.source.options, self.dynamic)
	}
}

/// A validator compiled with `coerce`: it also coerces loosely typed input towards the schema.
#[derive(Clone)]
pub struct CoercingValidator {
	validator: Validator,
}

impl CoercingValidator {
	/// `value` coerced towards the schema: numeric strings to numbers, `"true"` to `true`, a single value to a
	/// one-item array, an optional `null` dropped, the first union branch the value converts to (the TS README lists
	/// every rule). Values that cannot be coerced stay as they are; `value` is never modified.
	pub fn coerce(&self, value: &JsonValue) -> JsonValue {
		let mut compiler = self.validator.compiler();
		Coercer::new(&mut compiler, &self.validator.source.schema).coerce(value)
	}

	/// Coerces `value`, then checks it: the coerced value when it is valid, else its errors.
	pub fn parse(&self, value: &JsonValue) -> ParseResult {
		let coerced = self.coerce(value);
		match self.validator.validate(&coerced) {
			Ok(()) => Ok(coerced),
			Err(errors) => Err(errors),
		}
	}

	/// Pass 2 of coercion alone, as TypeBox's `Value.Convert` (for the ported TypeBox tests).
	#[cfg(test)]
	pub(crate) fn convert_only(&self, value: &JsonValue) -> JsonValue {
		let mut compiler = self.validator.compiler();
		Coercer::new(&mut compiler, &self.validator.source.schema).convert_only(value)
	}
}

impl Deref for CoercingValidator {
	type Target = Validator;

	fn deref(&self) -> &Validator {
		&self.validator
	}
}

//! JSON Schema validation (drafts 2020-12 and 7) for tool parameters, ported from `packages/schema` (TS
//! `index.ts`): `compile()` builds a schema into closures once; the builders return plain JSON Schema values.

mod annotations;
pub mod builders;
mod checks;
mod coerce;
mod compiler;
mod explain;
pub mod formats;
mod resolver;
mod types;
mod validator;

pub use checks::deep_equal as equal;
pub use types::{
	CompileError, CompileOptions, Draft, FormatCheck, FormatSet, JsonSchema, ParseResult, UnresolvedRefs,
	ValidationError,
};
pub use validator::{CoercingValidator, Compiled, Validator, compile};

//! Chord: tracked JSON state, deltas and watches (ports `packages/chord`).

pub mod context;
pub mod delta;
pub mod json;
pub mod output;
pub mod sqlite;
pub mod watch;

#[cfg(test)]
mod test_executor;

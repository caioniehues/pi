//! Built-in providers. Wire providers come with their APIs (C3b and later); the faux provider is behind the `faux`
//! feature.

#[cfg(feature = "faux")]
pub mod faux;

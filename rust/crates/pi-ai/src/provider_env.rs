//! Provider-scoped environment values over the process environment (ports `packages/ai/src/utils/provider-env.ts`).
//! The Bun sandbox fallback of the TS file has no Rust counterpart.

use pi_types::Record;

/// Provider-scoped environment overrides; values win over the process environment (`ProviderEnv`).
pub type ProviderEnv = Record<String>;

/// A lookup of the process environment; injectable because tests cannot change the real one safely.
pub type ProcessEnv<'a> = &'a dyn Fn(&str) -> Option<String>;

/// The real process environment.
pub fn process_env(name: &str) -> Option<String> {
	std::env::var(name).ok()
}

/// A scoped value, else the process value; empty values count as absent (`getProviderEnvValue`).
pub fn get_provider_env_value(name: &str, env: Option<&ProviderEnv>) -> Option<String> {
	get_provider_env_value_in(name, env, &process_env)
}

/// [`get_provider_env_value`] over an injected process environment.
pub fn get_provider_env_value_in(name: &str, env: Option<&ProviderEnv>, process: ProcessEnv<'_>) -> Option<String> {
	env.and_then(|env| env.get(name))
		.filter(|value| !value.is_empty())
		.cloned()
		.or_else(|| process(name).filter(|value| !value.is_empty()))
}

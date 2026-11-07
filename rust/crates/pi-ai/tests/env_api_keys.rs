//! API keys from the environment (`env_api_keys.rs`): ports `env-api-keys.test.ts` over an injected process
//! environment (changing the real one is unsafe in Rust).

#![allow(
	clippy::unwrap_used,
	clippy::expect_used,
	clippy::unreadable_literal,
	clippy::needless_pass_by_value,
	clippy::unnecessary_literal_bound,
	clippy::assert_is_empty,
	clippy::cast_possible_truncation,
	clippy::cast_possible_wrap,
	clippy::cast_precision_loss,
	clippy::cast_sign_loss,
	clippy::format_collect,
	clippy::ignored_unit_patterns,
	clippy::items_after_statements,
	clippy::too_many_lines,
	clippy::type_complexity,
	reason = "test code: literals copied from the TS tests, helpers that fail the test on bad fixtures"
)]

use pi_ai::env_api_keys::{find_env_keys_in, get_env_api_key_in};
use std::collections::HashMap;

fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
	let map: HashMap<String, String> = pairs
		.iter()
		.map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
		.collect();
	move |name| map.get(name).cloned()
}

// TS: env-api-keys.test.ts
#[test]
fn does_not_treat_generic_github_tokens_as_github_copilot_credentials() {
	let process = env(&[("GH_TOKEN", "gh-token"), ("GITHUB_TOKEN", "github-token")]);
	assert_eq!(find_env_keys_in("github-copilot", None, &process), None);
	assert_eq!(get_env_api_key_in("github-copilot", None, &process), None);
}

// TS: env-api-keys.test.ts
#[test]
fn resolves_github_copilot_credentials_from_copilot_github_token() {
	let process = env(&[
		("COPILOT_GITHUB_TOKEN", "copilot-token"),
		("GH_TOKEN", "gh-token"),
		("GITHUB_TOKEN", "github-token"),
	]);
	assert_eq!(
		find_env_keys_in("github-copilot", None, &process),
		Some(vec!["COPILOT_GITHUB_TOKEN"])
	);
	assert_eq!(
		get_env_api_key_in("github-copilot", None, &process).as_deref(),
		Some("copilot-token")
	);
}

// TS: env-api-keys.test.ts
#[test]
fn resolves_zai_china_coding_plan_credentials_from_zai_coding_cn_api_key() {
	let process = env(&[("ZAI_CODING_CN_API_KEY", "zai-coding-cn-token")]);
	assert_eq!(
		find_env_keys_in("zai-coding-cn", None, &process),
		Some(vec!["ZAI_CODING_CN_API_KEY"])
	);
	assert_eq!(
		get_env_api_key_in("zai-coding-cn", None, &process).as_deref(),
		Some("zai-coding-cn-token")
	);
}

// TS: env-api-keys.test.ts
#[test]
fn reports_anthropic_auth_token_but_preserves_oauth_token_api_key_lookup() {
	let process = env(&[
		("ANTHROPIC_AUTH_TOKEN", "auth-token"),
		("ANTHROPIC_OAUTH_TOKEN", "oauth-token"),
		("ANTHROPIC_API_KEY", "api-key"),
	]);
	assert_eq!(
		find_env_keys_in("anthropic", None, &process),
		Some(vec![
			"ANTHROPIC_AUTH_TOKEN",
			"ANTHROPIC_OAUTH_TOKEN",
			"ANTHROPIC_API_KEY"
		])
	);
	assert_eq!(
		get_env_api_key_in("anthropic", None, &process).as_deref(),
		Some("oauth-token")
	);
}

// TS: env-api-keys.test.ts
#[test]
fn does_not_return_anthropic_auth_token_as_an_api_key() {
	let process = env(&[("ANTHROPIC_AUTH_TOKEN", "auth-token")]);
	assert_eq!(
		find_env_keys_in("anthropic", None, &process),
		Some(vec!["ANTHROPIC_AUTH_TOKEN"])
	);
	assert_eq!(get_env_api_key_in("anthropic", None, &process), None);
}

// TS: env-api-keys.test.ts
#[test]
fn preserves_anthropic_oauth_token_as_an_api_key() {
	let process = env(&[("ANTHROPIC_OAUTH_TOKEN", "oauth-token")]);
	assert_eq!(
		find_env_keys_in("anthropic", None, &process),
		Some(vec!["ANTHROPIC_OAUTH_TOKEN"])
	);
	assert_eq!(
		get_env_api_key_in("anthropic", None, &process).as_deref(),
		Some("oauth-token")
	);
}

// TS: env-api-keys.test.ts
#[test]
fn falls_back_to_anthropic_api_key_for_api_key_lookup() {
	let process = env(&[("ANTHROPIC_API_KEY", "api-key")]);
	assert_eq!(
		get_env_api_key_in("anthropic", None, &process).as_deref(),
		Some("api-key")
	);
}

// Rust-only.
#[test]
fn scoped_env_wins_over_the_process_and_ambient_bedrock_credentials_authenticate() {
	let process = env(&[("OPENAI_API_KEY", "process")]);
	let scoped: pi_ai::provider_env::ProviderEnv = [("OPENAI_API_KEY", "scoped".to_owned())].into_iter().collect();
	assert_eq!(
		get_env_api_key_in("openai", Some(&scoped), &process).as_deref(),
		Some("scoped")
	);
	assert_eq!(
		get_env_api_key_in("amazon-bedrock", None, &env(&[("AWS_PROFILE", "dev")])).as_deref(),
		Some("<authenticated>")
	);
}

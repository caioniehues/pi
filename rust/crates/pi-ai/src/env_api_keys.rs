//! API keys from environment variables (ports `packages/ai/src/env-api-keys.ts`): the variable of each provider, and
//! the ambient credentials of Vertex AI and Amazon Bedrock.

use crate::provider_env::{ProcessEnv, ProviderEnv, get_provider_env_value_in, process_env};
use std::path::PathBuf;

/// `ANTHROPIC_AUTH_TOKEN`: reported by discovery, never returned as an API key (it is sent as `Authorization`).
pub const ANTHROPIC_AUTH_TOKEN_ENV: &str = "ANTHROPIC_AUTH_TOKEN";
/// `ANTHROPIC_OAUTH_TOKEN`.
pub const ANTHROPIC_OAUTH_TOKEN_ENV: &str = "ANTHROPIC_OAUTH_TOKEN";
/// `ANTHROPIC_API_KEY`.
pub const ANTHROPIC_API_KEY_ENV: &str = "ANTHROPIC_API_KEY";
/// `ANTHROPIC_FEDERATION_RULE_ID`.
pub const ANTHROPIC_FEDERATION_RULE_ID_ENV: &str = "ANTHROPIC_FEDERATION_RULE_ID";
/// `ANTHROPIC_ORGANIZATION_ID`.
pub const ANTHROPIC_ORGANIZATION_ID_ENV: &str = "ANTHROPIC_ORGANIZATION_ID";
/// `ANTHROPIC_SERVICE_ACCOUNT_ID`.
pub const ANTHROPIC_SERVICE_ACCOUNT_ID_ENV: &str = "ANTHROPIC_SERVICE_ACCOUNT_ID";
/// `ANTHROPIC_IDENTITY_TOKEN_FILE`.
pub const ANTHROPIC_IDENTITY_TOKEN_FILE_ENV: &str = "ANTHROPIC_IDENTITY_TOKEN_FILE";
/// `ANTHROPIC_WORKSPACE_ID`.
pub const ANTHROPIC_WORKSPACE_ID_ENV: &str = "ANTHROPIC_WORKSPACE_ID";

/// The single API key variable of each provider, in the TS table's order.
const ENV_MAP: &[(&str, &str)] = &[
	("ant-ling", "ANT_LING_API_KEY"),
	("qwen-token-plan", "QWEN_TOKEN_PLAN_API_KEY"),
	("qwen-token-plan-cn", "QWEN_TOKEN_PLAN_CN_API_KEY"),
	("qwen-token-plan-individual", "QWEN_TOKEN_PLAN_API_KEY"),
	("qwen-token-plan-individual-cn", "QWEN_TOKEN_PLAN_CN_API_KEY"),
	("openai", "OPENAI_API_KEY"),
	("azure", "AZURE_OPENAI_API_KEY"),
	("nvidia", "NVIDIA_API_KEY"),
	("deepseek", "DEEPSEEK_API_KEY"),
	("google", "GEMINI_API_KEY"),
	("google-vertex", "GOOGLE_CLOUD_API_KEY"),
	("groq", "GROQ_API_KEY"),
	("cerebras", "CEREBRAS_API_KEY"),
	("xai", "XAI_API_KEY"),
	("typesafe", "TYPESAFE_API_KEY"),
	("radius", "RADIUS_API_KEY"),
	("openrouter", "OPENROUTER_API_KEY"),
	("vercel-ai-gateway", "AI_GATEWAY_API_KEY"),
	("zai", "ZAI_API_KEY"),
	("zai-coding-cn", "ZAI_CODING_CN_API_KEY"),
	("mistral", "MISTRAL_API_KEY"),
	("minimax", "MINIMAX_API_KEY"),
	("minimax-cn", "MINIMAX_CN_API_KEY"),
	("moonshotai", "MOONSHOT_API_KEY"),
	("moonshotai-cn", "MOONSHOT_API_KEY"),
	("huggingface", "HF_TOKEN"),
	("fireworks", "FIREWORKS_API_KEY"),
	("together", "TOGETHER_API_KEY"),
	("baseten", "BASETEN_API_KEY"),
	("opencode", "OPENCODE_API_KEY"),
	("opencode-go", "OPENCODE_API_KEY"),
	("kimi-coding", "KIMI_API_KEY"),
	("meta", "META_API_KEY"),
	("cloudflare-workers-ai", "CLOUDFLARE_API_KEY"),
	("cloudflare-ai-gateway", "CLOUDFLARE_API_KEY"),
	("xiaomi", "XIAOMI_API_KEY"),
	("xiaomi-token-plan-cn", "XIAOMI_TOKEN_PLAN_CN_API_KEY"),
	("xiaomi-token-plan-ams", "XIAOMI_TOKEN_PLAN_AMS_API_KEY"),
	("xiaomi-token-plan-sgp", "XIAOMI_TOKEN_PLAN_SGP_API_KEY"),
];

fn api_key_env_vars(provider: &str) -> Option<Vec<&'static str>> {
	match provider {
		"github-copilot" => Some(vec!["COPILOT_GITHUB_TOKEN"]),
		"anthropic" => Some(vec![
			ANTHROPIC_AUTH_TOKEN_ENV,
			ANTHROPIC_OAUTH_TOKEN_ENV,
			ANTHROPIC_API_KEY_ENV,
		]),
		_ => ENV_MAP
			.iter()
			.find(|(id, _)| *id == provider)
			.map(|(_, var)| vec![*var]),
	}
}

/// The set variables that can provide an API key for a provider (`findEnvKeys`); ambient sources are not reported.
pub fn find_env_keys(provider: &str, env: Option<&ProviderEnv>) -> Option<Vec<&'static str>> {
	find_env_keys_in(provider, env, &process_env)
}

/// [`find_env_keys`] over an injected process environment.
pub fn find_env_keys_in(
	provider: &str,
	env: Option<&ProviderEnv>,
	process: ProcessEnv<'_>,
) -> Option<Vec<&'static str>> {
	let found: Vec<&str> = api_key_env_vars(provider)?
		.into_iter()
		.filter(|var| get_provider_env_value_in(var, env, process).is_some())
		.collect();
	(!found.is_empty()).then_some(found)
}

/// The API key of a provider from its variable, or `<authenticated>` for ambient Vertex or Bedrock credentials
/// (`getEnvApiKey`).
pub fn get_env_api_key(provider: &str, env: Option<&ProviderEnv>) -> Option<String> {
	get_env_api_key_in(provider, env, &process_env)
}

/// The marker returned for ambient credentials.
const AUTHENTICATED: &str = "<authenticated>";

/// [`get_env_api_key`] over an injected process environment.
pub fn get_env_api_key_in(provider: &str, env: Option<&ProviderEnv>, process: ProcessEnv<'_>) -> Option<String> {
	let value = |name: &str| get_provider_env_value_in(name, env, process);
	if let Some(keys) = find_env_keys_in(provider, env, process) {
		let key = if provider == "anthropic" {
			keys.into_iter().find(|key| *key != ANTHROPIC_AUTH_TOKEN_ENV)
		} else {
			keys.first().copied()
		};
		if let Some(key) = key {
			return value(key);
		}
	}
	if provider == "google-vertex"
		&& has_vertex_adc_credentials(env, process)
		&& (value("GOOGLE_CLOUD_PROJECT").is_some() || value("GCLOUD_PROJECT").is_some())
		&& value("GOOGLE_CLOUD_LOCATION").is_some()
	{
		return Some(AUTHENTICATED.into());
	}
	let bedrock = provider == "amazon-bedrock"
		&& (value("AWS_PROFILE").is_some()
			|| (value("AWS_ACCESS_KEY_ID").is_some() && value("AWS_SECRET_ACCESS_KEY").is_some())
			|| value("AWS_BEARER_TOKEN_BEDROCK").is_some()
			|| value("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI").is_some()
			|| value("AWS_CONTAINER_CREDENTIALS_FULL_URI").is_some()
			|| value("AWS_WEB_IDENTITY_TOKEN_FILE").is_some());
	bedrock.then(|| AUTHENTICATED.into())
}

/// Whether Application Default Credentials exist: the explicit file, else `GOOGLE_APPLICATION_CREDENTIALS`, else
/// gcloud's default file. TS caches the answer per process; checking a path again is cheap enough here.
fn has_vertex_adc_credentials(env: Option<&ProviderEnv>, process: ProcessEnv<'_>) -> bool {
	if let Some(path) = env
		.and_then(|env| env.get("GOOGLE_APPLICATION_CREDENTIALS"))
		.filter(|path| !path.is_empty())
	{
		return PathBuf::from(path).exists();
	}
	if let Some(path) = get_provider_env_value_in("GOOGLE_APPLICATION_CREDENTIALS", env, process) {
		return PathBuf::from(path).exists();
	}
	let Some(home) = process("HOME").filter(|home| !home.is_empty()) else {
		return false;
	};
	PathBuf::from(home)
		.join(".config/gcloud/application_default_credentials.json")
		.exists()
}

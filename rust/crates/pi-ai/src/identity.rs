//! How requests identify the calling app: the `User-Agent` and provider attribution headers (ports
//! `packages/ai/src/utils/identity.ts`).
//!
//! The detailed `User-Agent` names the Rust runtime where TS names Node: `pi/<version> (<platform>; rust/<rustc
//! version>; <arch>)`, with Node's platform and architecture names.

use pi_json::JsonObject;
use pi_types::Record;
use serde::{Deserialize, Deserializer, Serialize};

/// The `@earendil-works/pi-ai` package version this crate ports; kept equal to `packages/ai/package.json`.
pub const PI_AI_VERSION: &str = "1.5.0";

/// The `User-Agent` mode (`UserAgentMode`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UserAgentMode {
	/// `"short"` or `"detailed"`.
	Named(UserAgentName),
	/// `{ custom }`: the given value.
	Custom {
		/// The value.
		custom: String,
	},
}

/// `"short" | "detailed"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UserAgentName {
	/// `pi`.
	Short,
	/// `pi/<version> (<platform>; <runtime>; <arch>)`.
	Detailed,
}

/// Who requests are attributed to (`ClientAttribution`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientAttribution {
	/// The app name.
	pub name: String,
	/// The app URL.
	pub url: String,
	/// The app categories, such as `cli-agent`.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub categories: Option<Vec<String>>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// pi's attribution, used by the agent, Durable and the coding agent (`PI_ATTRIBUTION`).
pub fn pi_attribution() -> ClientAttribution {
	ClientAttribution {
		name: "pi".into(),
		url: "https://pi.dev".into(),
		categories: Some(vec!["cli-agent".into()]),
		extra: JsonObject::new(),
	}
}

/// How pi-ai identifies the calling app; absent fields use the defaults (`ClientIdentity`). Plain JSON.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientIdentity {
	/// Default: detailed.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub user_agent: Option<UserAgentMode>,
	/// Absent: unset; `Some(None)`: `null`, no attribution, also over a merged identity's.
	#[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
	pub attribution: Option<Option<ClientAttribution>>,
	/// The version in the detailed `User-Agent`; default the pi-ai version.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub version: Option<String>,
}

fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(deserializer: D) -> Result<Option<T>, D::Error> {
	T::deserialize(deserializer).map(Some)
}

/// The provider and endpoint a request goes to (`ClientIdentityTarget`).
#[derive(Debug, Clone, Copy)]
pub struct ClientIdentityTarget<'a> {
	/// The provider id.
	pub provider: &'a str,
	/// The base URL.
	pub base_url: &'a str,
}

const NVIDIA_NIM_HOST: &str = "integrate.api.nvidia.com";
const OPENCODE_HOST: &str = "opencode.ai";

/// Merges identities field by field; later sources win, unset fields are skipped (`mergeClientIdentity`).
pub fn merge_client_identity(sources: &[Option<&ClientIdentity>]) -> Option<ClientIdentity> {
	let mut merged: Option<ClientIdentity> = None;
	for source in sources.iter().flatten() {
		let target = merged.get_or_insert_with(ClientIdentity::default);
		if source.user_agent.is_some() {
			target.user_agent.clone_from(&source.user_agent);
		}
		if source.attribution.is_some() {
			target.attribution.clone_from(&source.attribution);
		}
		if source.version.is_some() {
			target.version.clone_from(&source.version);
		}
	}
	merged
}

/// The version in the detailed `User-Agent` (`getClientVersion`).
pub fn get_client_version(identity: Option<&ClientIdentity>) -> &str {
	identity
		.and_then(|identity| identity.version.as_deref())
		.unwrap_or(PI_AI_VERSION)
}

/// The `User-Agent` of an identity (`formatUserAgent`).
pub fn format_user_agent(identity: Option<&ClientIdentity>) -> String {
	match identity.and_then(|identity| identity.user_agent.as_ref()) {
		Some(UserAgentMode::Custom { custom }) => custom.clone(),
		Some(UserAgentMode::Named(UserAgentName::Short)) => "pi".into(),
		Some(UserAgentMode::Named(UserAgentName::Detailed)) | None => format!(
			"pi/{} ({}; rust/{}; {})",
			get_client_version(identity),
			node_platform(),
			env!("PI_RUSTC_VERSION"),
			node_arch()
		),
	}
}

/// `process.platform` names.
fn node_platform() -> &'static str {
	match std::env::consts::OS {
		"macos" => "darwin",
		"windows" => "win32",
		other => other,
	}
}

/// `process.arch` names.
fn node_arch() -> &'static str {
	match std::env::consts::ARCH {
		"x86_64" => "x64",
		"aarch64" => "arm64",
		"x86" => "ia32",
		other => other,
	}
}

fn matches_host(base_url: &str, expected: &str) -> bool {
	url::Url::parse(base_url).is_ok_and(|url| url.host_str() == Some(expected))
}

fn is_openrouter(target: ClientIdentityTarget<'_>) -> bool {
	target.provider == "openrouter" || target.base_url.contains("openrouter.ai")
}

fn is_nvidia_nim(target: ClientIdentityTarget<'_>) -> bool {
	target.provider == "nvidia" || matches_host(target.base_url, NVIDIA_NIM_HOST)
}

fn is_opencode(target: ClientIdentityTarget<'_>) -> bool {
	matches!(target.provider, "opencode" | "opencode-go") || matches_host(target.base_url, OPENCODE_HOST)
}

/// The attribution headers of an identity for a target; empty when attribution is off (`getAttributionHeaders`).
pub fn get_attribution_headers(target: ClientIdentityTarget<'_>, identity: Option<&ClientIdentity>) -> Record<String> {
	let Some(Some(attribution)) = identity.and_then(|identity| identity.attribution.as_ref()) else {
		return Record::new();
	};
	let mut headers = Record::new();
	if is_openrouter(target) {
		headers.insert("HTTP-Referer", attribution.url.clone());
		headers.insert("X-OpenRouter-Title", attribution.name.clone());
		if let Some(categories) = attribution
			.categories
			.as_ref()
			.filter(|categories| !categories.is_empty())
		{
			headers.insert("X-OpenRouter-Categories", categories.join(","));
		}
	} else if is_nvidia_nim(target) {
		// NVIDIA expects a capitalized origin, such as `Pi`.
		let mut name = attribution.name.chars();
		let capitalized: String = name
			.next()
			.map(|first| first.to_uppercase().chain(name).collect())
			.unwrap_or_default();
		headers.insert("X-BILLING-INVOKE-ORIGIN", capitalized);
	} else if is_opencode(target) {
		headers.insert("x-opencode-client", attribution.name.clone());
	}
	headers
}

/// The per-conversation routing header of `opencode.ai`; `None` without a session or for other targets
/// (`getOpenCodeSessionHeaders`).
pub fn get_opencode_session_headers(
	target: ClientIdentityTarget<'_>,
	session_id: Option<&str>,
) -> Option<Record<String>> {
	let session = session_id.filter(|session| !session.is_empty())?;
	is_opencode(target).then(|| [("x-opencode-session", session.to_owned())].into_iter().collect())
}

/// The headers an identity adds below model, provider and request headers (`getIdentityHeaders`).
pub fn get_identity_headers(target: ClientIdentityTarget<'_>, identity: Option<&ClientIdentity>) -> Record<String> {
	let mut headers: Record<String> = [("User-Agent", format_user_agent(identity))].into_iter().collect();
	for (name, value) in get_attribution_headers(target, identity).iter() {
		headers.insert(name, value.clone());
	}
	headers
}

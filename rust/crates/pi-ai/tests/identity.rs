//! Client identity (`identity.rs`): ports `identity.test.ts`. The cases that capture wire headers of
//! `openai-completions` are C3b's; the identity merge through `Models` is checked on the options a provider gets.

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

mod common;

use async_trait::async_trait;
use pi_ai::auth::types::ProviderAuth;
use pi_ai::event_stream::AssistantMessageEventStream;
use pi_ai::identity::{
	ClientAttribution, ClientIdentity, ClientIdentityTarget, PI_AI_VERSION, UserAgentMode, UserAgentName,
	format_user_agent, get_attribution_headers, get_identity_headers, merge_client_identity, pi_attribution,
};
use pi_ai::model_operations::AnyModel;
use pi_ai::models::Models;
use pi_ai::options::{SimpleStreamOptions, StreamOptions};
use pi_ai::provider::{CreateProviderOptions, ProviderApi, ProviderStreams, create_provider};
use pi_ai::transcript::{Context, TranscriptContext};
use pi_types::{Model, Record};
use serde_json::json;
use std::sync::{Arc, Mutex};

fn target<'a>(provider: &'a str, base_url: &'a str) -> ClientIdentityTarget<'a> {
	ClientIdentityTarget { provider, base_url }
}

fn acme(categories: Option<Vec<&str>>) -> ClientAttribution {
	ClientAttribution {
		name: "acme".into(),
		url: "https://acme.example".into(),
		categories: categories.map(|list| list.into_iter().map(str::to_owned).collect()),
		extra: pi_json::JsonObject::new(),
	}
}

fn attributed(attribution: ClientAttribution) -> ClientIdentity {
	ClientIdentity {
		attribution: Some(Some(attribution)),
		..ClientIdentity::default()
	}
}

fn record(pairs: &[(&str, &str)]) -> Record<String> {
	pairs.iter().map(|(name, value)| (*name, (*value).to_owned())).collect()
}

fn runtime() -> String {
	let platform = match std::env::consts::OS {
		"macos" => "darwin",
		"windows" => "win32",
		other => other,
	};
	let arch = match std::env::consts::ARCH {
		"x86_64" => "x64",
		"aarch64" => "arm64",
		other => other,
	};
	format!("{platform}; rust/{}; {arch}", env!("PI_RUSTC_VERSION"))
}

// TS: identity.test.ts
#[test]
fn keeps_the_version_constant_in_sync_with_package_json() {
	let package = std::fs::read_to_string(concat!(
		env!("CARGO_MANIFEST_DIR"),
		"/../../../packages/ai/package.json"
	))
	.unwrap();
	assert_eq!(pi_json::parse(&package).unwrap()["version"], PI_AI_VERSION);
}

// TS: identity.test.ts
#[test]
fn formats_user_agents() {
	assert_eq!(format_user_agent(None), format!("pi/{PI_AI_VERSION} ({})", runtime()));
	let versioned = ClientIdentity {
		version: Some("9.9.9".into()),
		..ClientIdentity::default()
	};
	assert_eq!(format_user_agent(Some(&versioned)), format!("pi/9.9.9 ({})", runtime()));
	let short = ClientIdentity {
		user_agent: Some(UserAgentMode::Named(UserAgentName::Short)),
		version: Some("9.9.9".into()),
		..ClientIdentity::default()
	};
	assert_eq!(format_user_agent(Some(&short)), "pi");
	let custom = ClientIdentity {
		user_agent: Some(UserAgentMode::Custom {
			custom: "acme/1.0".into(),
		}),
		..ClientIdentity::default()
	};
	assert_eq!(format_user_agent(Some(&custom)), "acme/1.0");
}

// TS: identity.test.ts
#[test]
fn merges_identities_field_by_field() {
	assert_eq!(merge_client_identity(&[None, None]), None);
	let first = ClientIdentity {
		attribution: Some(Some(pi_attribution())),
		user_agent: Some(UserAgentMode::Named(UserAgentName::Short)),
		version: None,
	};
	let second = ClientIdentity {
		attribution: None,
		version: Some("1.0.0".into()),
		..ClientIdentity::default()
	};
	assert_eq!(
		merge_client_identity(&[Some(&first), Some(&second)]),
		Some(ClientIdentity {
			attribution: Some(Some(pi_attribution())),
			user_agent: Some(UserAgentMode::Named(UserAgentName::Short)),
			version: Some("1.0.0".into())
		})
	);
	let off = ClientIdentity {
		attribution: Some(None),
		..ClientIdentity::default()
	};
	assert_eq!(
		merge_client_identity(&[Some(&attributed(pi_attribution())), Some(&off)]),
		Some(off.clone())
	);
	assert_eq!(common::to_json(&off), json!({ "attribution": null }));
}

// TS: identity.test.ts
#[test]
fn sends_no_attribution_unless_enabled() {
	let open_router = target("openrouter", "https://openrouter.ai/api/v1");
	assert!(get_attribution_headers(open_router, None).is_empty());
	assert!(
		get_attribution_headers(
			open_router,
			Some(&ClientIdentity {
				attribution: Some(None),
				..ClientIdentity::default()
			})
		)
		.is_empty()
	);
	assert_eq!(
		get_identity_headers(open_router, None),
		record(&[("User-Agent", &format_user_agent(None))])
	);
}

// TS: identity.test.ts
#[test]
fn attributes_openrouter_requests() {
	let open_router = target("openrouter", "https://openrouter.ai/api/v1");
	assert_eq!(
		get_attribution_headers(open_router, Some(&attributed(pi_attribution()))),
		record(&[
			("HTTP-Referer", "https://pi.dev"),
			("X-OpenRouter-Title", "pi"),
			("X-OpenRouter-Categories", "cli-agent")
		])
	);
	assert_eq!(
		get_attribution_headers(
			open_router,
			Some(&attributed(acme(Some(vec!["cli-agent", "cloud-agent"]))))
		),
		record(&[
			("HTTP-Referer", "https://acme.example"),
			("X-OpenRouter-Title", "acme"),
			("X-OpenRouter-Categories", "cli-agent,cloud-agent")
		])
	);
	assert_eq!(
		get_attribution_headers(open_router, Some(&attributed(acme(Some(vec![]))))),
		record(&[("HTTP-Referer", "https://acme.example"), ("X-OpenRouter-Title", "acme")])
	);
	// Custom providers routed through OpenRouter, including legacy base URLs that are not valid URLs.
	let custom = get_attribution_headers(
		target("custom", "not-a-url-openrouter.ai"),
		Some(&attributed(pi_attribution())),
	);
	assert_eq!(custom.get("X-OpenRouter-Title").map(String::as_str), Some("pi"));
}

// TS: identity.test.ts
#[test]
fn attributes_nvidia_nim_requests_with_a_capitalized_name_and_opencode_requests_with_the_name() {
	assert_eq!(
		get_attribution_headers(
			target("nvidia", "https://example.test/v1"),
			Some(&attributed(pi_attribution()))
		),
		record(&[("X-BILLING-INVOKE-ORIGIN", "Pi")])
	);
	assert_eq!(
		get_attribution_headers(
			target("custom-nim", "https://integrate.api.nvidia.com/v1"),
			Some(&attributed(acme(None)))
		),
		record(&[("X-BILLING-INVOKE-ORIGIN", "Acme")])
	);
	assert_eq!(
		get_attribution_headers(
			target("opencode", "https://opencode.ai/zen/v1"),
			Some(&attributed(pi_attribution()))
		),
		record(&[("x-opencode-client", "pi")])
	);
	assert_eq!(
		get_attribution_headers(
			target("custom", "https://opencode.ai/zen/v1"),
			Some(&attributed(acme(None)))
		),
		record(&[("x-opencode-client", "acme")])
	);
}

// TS: identity.test.ts
#[test]
fn does_not_attribute_nvidia_models_routed_through_gateways_to_nvidia() {
	assert!(
		get_attribution_headers(
			target("openrouter", "https://openrouter.ai/api/v1"),
			Some(&attributed(pi_attribution()))
		)
		.get("X-BILLING-INVOKE-ORIGIN")
		.is_none()
	);
	assert!(
		get_attribution_headers(
			target("vercel-ai-gateway", "https://ai-gateway.vercel.sh/v1"),
			Some(&attributed(pi_attribution()))
		)
		.is_empty()
	);
}

/// Records the identity and headers each request carried.
#[derive(Default)]
struct Capture {
	seen: Mutex<Vec<StreamOptions>>,
}

#[async_trait]
impl ProviderStreams for Capture {
	fn stream(
		&self,
		_model: &Model,
		_context: TranscriptContext,
		options: StreamOptions,
	) -> AssistantMessageEventStream {
		self.seen.lock().unwrap().push(options);
		let stream = AssistantMessageEventStream::new();
		stream.end(Some(common::assistant(json!({}))));
		stream
	}
	fn stream_simple(
		&self,
		model: &Model,
		context: TranscriptContext,
		options: SimpleStreamOptions,
	) -> AssistantMessageEventStream {
		self.stream(model, context, options.stream)
	}
}

fn registry(provider: &str, base_url: &str, identity: Option<ClientIdentity>) -> (Models, Arc<Capture>, Model) {
	let capture = Arc::new(Capture::default());
	let model = common::model(
		"openai-completions",
		provider,
		"test-model",
		json!({ "baseUrl": base_url }),
	);
	let models = Models::new(pi_ai::models::CreateModelsOptions {
		identity,
		..Default::default()
	});
	models.set_provider(
		create_provider(CreateProviderOptions {
			id: provider.into(),
			auth: ProviderAuth {
				api_key: Some(pi_ai::auth::helpers::env_api_key_auth("Test", &[])),
				oauth: None,
			},
			models: vec![AnyModel::Chat(model.clone())],
			api: Some(ProviderApi::Single(Arc::clone(&capture) as Arc<dyn ProviderStreams>)),
			..CreateProviderOptions::default()
		})
		.unwrap(),
	);
	(models, capture, model)
}

fn hi() -> Context {
	Context {
		messages: vec![common::user("hello", 1)],
		..Context::default()
	}
}

fn with_key(identity: Option<ClientIdentity>) -> SimpleStreamOptions {
	let mut options = SimpleStreamOptions::default();
	options.stream.request.api_key = Some("key".into());
	options.stream.request.identity = identity;
	options
}

// TS: identity.test.ts
#[tokio::test]
async fn merges_request_identity_over_the_models_and_wrapper_identities() {
	let attribution = |name: &str| ClientAttribution {
		name: name.into(),
		url: "https://acme.example".into(),
		categories: None,
		extra: pi_json::JsonObject::new(),
	};
	let base = ClientIdentity {
		attribution: Some(Some(attribution("models"))),
		user_agent: Some(UserAgentMode::Named(UserAgentName::Short)),
		version: None,
	};
	let (models, capture, model) = registry("openrouter", "https://openrouter.ai/api/v1", Some(base));
	models.complete_simple(&model, hi(), with_key(None)).await;
	let identity = capture.seen.lock().unwrap()[0].request.identity.clone().unwrap();
	assert_eq!(identity.user_agent, Some(UserAgentMode::Named(UserAgentName::Short)));
	assert_eq!(identity.attribution, Some(Some(attribution("models"))));
	let wrapped = models.with_client_identity(&attributed(attribution("wrapper")));
	wrapped.complete_simple(&model, hi(), with_key(None)).await;
	let identity = capture.seen.lock().unwrap()[1].request.identity.clone().unwrap();
	assert_eq!(
		(identity.user_agent, identity.attribution),
		(
			Some(UserAgentMode::Named(UserAgentName::Short)),
			Some(Some(attribution("wrapper")))
		)
	);
	assert_eq!(wrapped.get_model("openrouter", "test-model"), Some(model.clone()));
	// The wrapper shares the providers.
	wrapped.delete_provider("openrouter");
	assert!(models.get_provider("openrouter").is_none());
}

// TS: identity.test.ts
#[tokio::test]
async fn sends_the_opencode_session_header_to_custom_providers_pointing_at_opencode() {
	let (models, capture, model) = registry("custom-opencode", "https://opencode.ai/zen/v1", None);
	let mut options = with_key(None);
	options.stream.session_id = Some("session-1".into());
	models.complete_simple(&model, hi(), options.clone()).await;
	options.stream.request.headers = Some(
		[("X-OpenCode-Session", Some("configured".to_owned()))]
			.into_iter()
			.collect(),
	);
	models.complete_simple(&model, hi(), options).await;
	let seen = capture.seen.lock().unwrap();
	assert_eq!(
		seen[0].request.headers,
		Some(
			[("x-opencode-session", Some("session-1".to_owned()))]
				.into_iter()
				.collect()
		)
	);
	assert_eq!(
		seen[1].request.headers,
		Some(
			[("X-OpenCode-Session", Some("configured".to_owned()))]
				.into_iter()
				.collect()
		)
	);
}

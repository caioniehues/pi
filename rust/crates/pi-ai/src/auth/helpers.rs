//! Standard api-key auth (ports `envApiKeyAuth` of `packages/ai/src/auth/helpers.ts`): a stored key wins, else the
//! first set variable. TS's `lazyOAuth` defers loading flow code for bundles; Rust links it, so it has no port.

use crate::auth::types::{
	ApiKeyAuth, ApiKeyCredential, ApiKeyInput, AuthInteraction, AuthPrompt, AuthResult, ModelAuth,
};
use crate::error::ModelsError;
use async_trait::async_trait;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Api-key auth from a stored key or the first set variable of `env_vars`, with a login that prompts for the key.
pub fn env_api_key_auth(name: impl Into<String>, env_vars: &[&str]) -> Arc<dyn ApiKeyAuth> {
	Arc::new(EnvApiKeyAuth {
		name: name.into(),
		env_vars: env_vars.iter().map(|var| (*var).to_owned()).collect(),
	})
}

struct EnvApiKeyAuth {
	name: String,
	env_vars: Vec<String>,
}

fn check(cancel: &CancellationToken) -> Result<(), ModelsError> {
	if cancel.is_cancelled() {
		Err(ModelsError::aborted())
	} else {
		Ok(())
	}
}

#[async_trait]
impl ApiKeyAuth for EnvApiKeyAuth {
	fn name(&self) -> &str {
		&self.name
	}

	fn supports_login(&self) -> bool {
		true
	}

	async fn login(
		&self,
		interaction: &dyn AuthInteraction,
		cancel: &CancellationToken,
	) -> Result<ApiKeyCredential, ModelsError> {
		check(cancel)?;
		let key = interaction
			.prompt(AuthPrompt::Secret {
				message: format!("Enter {}", self.name),
				placeholder: None,
				cancel: None,
			})
			.await?;
		check(cancel)?;
		Ok(ApiKeyCredential {
			key: Some(key),
			env: None,
		})
	}

	async fn resolve(&self, input: ApiKeyInput<'_>) -> Result<Option<AuthResult>, ModelsError> {
		check(input.cancel)?;
		if let Some(credential) = input.credential
			&& let Some(key) = credential.key.as_ref().filter(|key| !key.is_empty())
		{
			return Ok(Some(AuthResult {
				auth: ModelAuth {
					api_key: Some(key.clone()),
					..ModelAuth::default()
				},
				env: credential.env.clone(),
				source: Some("stored credential".into()),
			}));
		}
		for var in &self.env_vars {
			let value = input.ctx.env(var).await;
			check(input.cancel)?;
			if let Some(value) = value.filter(|value| !value.is_empty()) {
				return Ok(Some(AuthResult {
					auth: ModelAuth {
						api_key: Some(value),
						..ModelAuth::default()
					},
					env: None,
					source: Some(var.clone()),
				}));
			}
		}
		Ok(None)
	}
}

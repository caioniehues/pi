//! The default auth context (ports `packages/ai/src/auth/context.ts`): process environment and file existence.

use crate::auth::types::AuthContext;
use async_trait::async_trait;
use std::path::PathBuf;

/// Variables from the process environment (blank counts as unset), files from the file system.
pub struct DefaultAuthContext;

#[async_trait]
impl AuthContext for DefaultAuthContext {
	async fn env(&self, name: &str) -> Option<String> {
		std::env::var(name).ok().filter(|value| !value.trim().is_empty())
	}

	async fn file_exists(&self, path: &str) -> bool {
		let resolved = match path.strip_prefix('~') {
			Some(rest) => match std::env::var("HOME") {
				Ok(home) => PathBuf::from(format!("{home}{rest}")),
				Err(_) => return false,
			},
			None => PathBuf::from(path),
		};
		resolved.exists()
	}
}

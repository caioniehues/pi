//! The default in-memory credential store (ports `packages/ai/src/auth/credential-store.ts`). Writes are serialized
//! per provider id by a lock; the file-backed `auth.json` store is pi-core's (C7).

use crate::auth::types::{Credential, CredentialInfo, CredentialStore, CredentialStoreError, CredentialUpdate};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use tokio_util::sync::CancellationToken;

/// Credentials in memory, in insertion order for `list`.
#[derive(Default)]
pub struct InMemoryCredentialStore {
	credentials: Mutex<Vec<(String, Credential)>>,
	locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl InMemoryCredentialStore {
	/// An empty store.
	pub fn new() -> Self {
		Self::default()
	}

	fn get(&self, provider_id: &str) -> Option<Credential> {
		let credentials = self.credentials.lock().unwrap_or_else(PoisonError::into_inner);
		credentials
			.iter()
			.find(|(id, _)| id == provider_id)
			.map(|(_, credential)| credential.clone())
	}

	fn set(&self, provider_id: &str, credential: Option<Credential>) {
		let mut credentials = self.credentials.lock().unwrap_or_else(PoisonError::into_inner);
		match (credentials.iter_mut().find(|(id, _)| id == provider_id), credential) {
			(Some((_, slot)), Some(credential)) => *slot = credential,
			(None, Some(credential)) => credentials.push((provider_id.to_owned(), credential)),
			(_, None) => credentials.retain(|(id, _)| id != provider_id),
		}
	}

	fn lock_of(&self, provider_id: &str) -> Arc<tokio::sync::Mutex<()>> {
		let mut locks = self.locks.lock().unwrap_or_else(PoisonError::into_inner);
		Arc::clone(locks.entry(provider_id.to_owned()).or_default())
	}

	/// Waits for the provider's lock unless `cancel` fires first.
	async fn acquire(
		&self,
		provider_id: &str,
		cancel: &CancellationToken,
	) -> Result<tokio::sync::OwnedMutexGuard<()>, CredentialStoreError> {
		let lock = self.lock_of(provider_id);
		if cancel.is_cancelled() {
			return Err(CredentialStoreError::Aborted);
		}
		tokio::select! {
			biased;
			() = cancel.cancelled() => Err(CredentialStoreError::Aborted),
			guard = lock.lock_owned() => Ok(guard),
		}
	}
}

#[async_trait]
impl CredentialStore for InMemoryCredentialStore {
	async fn read(
		&self,
		provider_id: &str,
		cancel: &CancellationToken,
	) -> Result<Option<Credential>, CredentialStoreError> {
		if cancel.is_cancelled() {
			return Err(CredentialStoreError::Aborted);
		}
		Ok(self.get(provider_id))
	}

	async fn list(&self, cancel: &CancellationToken) -> Result<Vec<CredentialInfo>, CredentialStoreError> {
		if cancel.is_cancelled() {
			return Err(CredentialStoreError::Aborted);
		}
		let credentials = self.credentials.lock().unwrap_or_else(PoisonError::into_inner);
		Ok(credentials
			.iter()
			.map(|(provider_id, credential)| CredentialInfo {
				provider_id: provider_id.clone(),
				auth_type: credential.auth_type(),
			})
			.collect())
	}

	async fn modify(
		&self,
		provider_id: &str,
		update: CredentialUpdate,
		cancel: &CancellationToken,
	) -> Result<Option<Credential>, CredentialStoreError> {
		let _guard = self.acquire(provider_id, cancel).await?;
		let current = self.get(provider_id);
		let next = update(current.clone()).await.map_err(CredentialStoreError::Callback)?;
		if cancel.is_cancelled() {
			return Err(CredentialStoreError::Aborted);
		}
		match next {
			Some(next) => {
				self.set(provider_id, Some(next.clone()));
				Ok(Some(next))
			}
			None => Ok(current),
		}
	}

	async fn delete(&self, provider_id: &str, cancel: &CancellationToken) -> Result<(), CredentialStoreError> {
		let _guard = self.acquire(provider_id, cancel).await?;
		self.set(provider_id, None);
		Ok(())
	}
}

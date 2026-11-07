//! Persistent model catalogs keyed by provider id (ports `packages/ai/src/models-store.ts`), and the in-memory store.

use crate::error::ModelsError;
use crate::model_operations::AnyModel;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use tokio_util::sync::CancellationToken;

/// A provider's persisted catalog (`ModelsStoreEntry`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsStoreEntry {
	/// Models of every type.
	pub models: Vec<AnyModel>,
	/// Epoch milliseconds of the remote catalog's `Last-Modified`.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub last_modified: Option<u64>,
	/// Epoch milliseconds of the last completed remote check.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub checked_at: Option<u64>,
	/// The remote catalog's `ETag`, verbatim.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub etag: Option<String>,
}

/// Persistent model catalogs (`ModelsStore`).
#[async_trait]
pub trait ModelsStore: Send + Sync {
	/// The provider's entry, if stored.
	async fn read(
		&self,
		provider_id: &str,
		cancel: &CancellationToken,
	) -> Result<Option<ModelsStoreEntry>, ModelsError>;
	/// Stores the provider's entry.
	async fn write(
		&self,
		provider_id: &str,
		entry: ModelsStoreEntry,
		cancel: &CancellationToken,
	) -> Result<(), ModelsError>;
	/// Removes the provider's entry.
	async fn delete(&self, provider_id: &str, cancel: &CancellationToken) -> Result<(), ModelsError>;
}

/// Catalogs in memory; reads return copies, as `structuredClone` does in TS.
#[derive(Default)]
pub struct InMemoryModelsStore {
	entries: Mutex<HashMap<String, ModelsStoreEntry>>,
}

impl InMemoryModelsStore {
	/// An empty store.
	pub fn new() -> Self {
		Self::default()
	}
}

fn check(cancel: &CancellationToken) -> Result<(), ModelsError> {
	if cancel.is_cancelled() {
		Err(ModelsError::aborted())
	} else {
		Ok(())
	}
}

#[async_trait]
impl ModelsStore for InMemoryModelsStore {
	async fn read(
		&self,
		provider_id: &str,
		cancel: &CancellationToken,
	) -> Result<Option<ModelsStoreEntry>, ModelsError> {
		check(cancel)?;
		Ok(self
			.entries
			.lock()
			.unwrap_or_else(PoisonError::into_inner)
			.get(provider_id)
			.cloned())
	}

	async fn write(
		&self,
		provider_id: &str,
		entry: ModelsStoreEntry,
		cancel: &CancellationToken,
	) -> Result<(), ModelsError> {
		check(cancel)?;
		self.entries
			.lock()
			.unwrap_or_else(PoisonError::into_inner)
			.insert(provider_id.to_owned(), entry);
		Ok(())
	}

	async fn delete(&self, provider_id: &str, cancel: &CancellationToken) -> Result<(), ModelsError> {
		check(cancel)?;
		self.entries
			.lock()
			.unwrap_or_else(PoisonError::into_inner)
			.remove(provider_id);
		Ok(())
	}
}

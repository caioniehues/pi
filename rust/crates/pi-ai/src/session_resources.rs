//! Cleanup hooks for resources an API keeps per session, such as cached connections (ports
//! `packages/ai/src/session-resources.ts`). Process-wide, as the TS module set is.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

/// A cleanup: called with a session id, or `None` for every session.
pub type SessionResourceCleanup = Arc<dyn Fn(Option<&str>) -> Result<(), String> + Send + Sync>;

static CLEANUPS: Mutex<Vec<(u64, SessionResourceCleanup)>> = Mutex::new(Vec::new());
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// A registration; dropping it does not unregister, `unregister` does.
pub struct SessionResourceRegistration {
	id: u64,
}

impl SessionResourceRegistration {
	/// Removes the cleanup (the function `registerSessionResourceCleanup` returns).
	pub fn unregister(self) {
		CLEANUPS
			.lock()
			.unwrap_or_else(PoisonError::into_inner)
			.retain(|(id, _)| *id != self.id);
	}
}

/// Registers a cleanup (`registerSessionResourceCleanup`).
pub fn register_session_resource_cleanup(cleanup: SessionResourceCleanup) -> SessionResourceRegistration {
	let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
	CLEANUPS
		.lock()
		.unwrap_or_else(PoisonError::into_inner)
		.push((id, cleanup));
	SessionResourceRegistration { id }
}

/// Runs every cleanup; fails with every error after running all (`cleanupSessionResources`, an `AggregateError`).
pub fn cleanup_session_resources(session_id: Option<&str>) -> Result<(), Vec<String>> {
	let cleanups: Vec<SessionResourceCleanup> = CLEANUPS
		.lock()
		.unwrap_or_else(PoisonError::into_inner)
		.iter()
		.map(|(_, cleanup)| Arc::clone(cleanup))
		.collect();
	let errors: Vec<String> = cleanups
		.iter()
		.filter_map(|cleanup| cleanup(session_id).err())
		.collect();
	if errors.is_empty() { Ok(()) } else { Err(errors) }
}

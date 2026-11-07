//! Cancellation of public operations (ports `packages/ai/src/utils/abort.ts`, `abort-signals.ts` and `sleep.ts`).
//! TS passes an optional `AbortSignal`; Rust passes a `CancellationToken`, and a cancelled wait drops the future it
//! waited on. Where TS lets an abandoned operation finish in the background (an OAuth refresh that may already have
//! rotated the token), the caller spawns that operation instead of racing it. `AbortSignal.any` of a caller signal
//! and an operation's own controller is the caller's `child_token()`.

use crate::error::ModelsError;
use std::future::Future;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Runs `operation` until it settles or `cancel` fires (`raceWithAbortSignal`); a fired token wins over a result
/// that is ready at the same time.
pub async fn race_with_cancel<T>(
	operation: impl Future<Output = Result<T, ModelsError>>,
	cancel: &CancellationToken,
) -> Result<T, ModelsError> {
	if cancel.is_cancelled() {
		return Err(ModelsError::aborted());
	}
	tokio::select! {
		biased;
		() = cancel.cancelled() => Err(ModelsError::aborted()),
		result = operation => result,
	}
}

/// Waits `ms` milliseconds unless `cancel` fires first (`sleep`); `Err` when cancelled.
pub async fn sleep(ms: u64, cancel: &CancellationToken) -> Result<(), ModelsError> {
	race_with_cancel(
		async {
			tokio::time::sleep(Duration::from_millis(ms)).await;
			Ok(())
		},
		cancel,
	)
	.await
}

//! A test executor for the crate's async tests: spawned futures run when `run` is called, until none of
//! them can make progress, which stands in for TS tests' `await flush()`.

use std::future::Future;
use std::pin::{Pin, pin};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};

/// Spawned futures, polled by `run` on the calling thread; shareable, so a fake clock can spawn onto it.
#[derive(Default)]
pub(crate) struct Executor {
	tasks: Mutex<Vec<Pin<Box<dyn Future<Output = ()> + Send>>>>,
	woken: Arc<Flag>,
}

#[derive(Default)]
struct Flag(AtomicBool);

impl Wake for Flag {
	fn wake(self: Arc<Self>) {
		self.0.store(true, Ordering::SeqCst);
	}
}

impl Executor {
	pub(crate) fn spawn(&self, future: impl Future<Output = ()> + Send + 'static) {
		self.lock().push(Box::pin(future));
	}

	fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Pin<Box<dyn Future<Output = ()> + Send>>>> {
		self.tasks.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
	}

	/// Polls every task until a whole pass wakes none of them; returns how many are still pending.
	pub(crate) fn run(&self) -> usize {
		let waker = Waker::from(self.woken.clone());
		let mut cx = Context::from_waker(&waker);
		loop {
			self.woken.0.store(false, Ordering::SeqCst);
			// Taken out of the lock while polled, so a task may spawn another.
			let mut tasks = std::mem::take(&mut *self.lock());
			tasks.retain_mut(|task| task.as_mut().poll(&mut cx).is_pending());
			let mut current = self.lock();
			// Tasks spawned during the pass have not been polled yet: another pass polls them.
			let spawned = !current.is_empty();
			tasks.append(&mut current);
			*current = tasks;
			if !spawned && !self.woken.0.load(Ordering::SeqCst) {
				return current.len();
			}
		}
	}
}

/// Polls `future` once: its output if it is ready now.
pub(crate) fn now<F: Future>(future: F) -> Option<F::Output> {
	let waker = Waker::from(Arc::new(Flag::default()));
	let mut cx = Context::from_waker(&waker);
	match pin!(future).poll(&mut cx) {
		Poll::Ready(output) => Some(output),
		Poll::Pending => None,
	}
}

//! Shutdown accounting for detached compensation work.
//!
//! pmux deliberately runs several operations on tasks the requester does not
//! own, because a dropped public request must not be able to abandon a kill, a
//! terminal handoff, or a write that was already committed to the wire. That
//! choice is only safe if something else can still prove those tasks finished,
//! which is what this module is: a counter with an exact awaitable fence.
//!
//! It lives in its own module rather than inside `native` because it is shared
//! by two layers that must fence against the *same* set of tasks -- the service
//! (`NativeService::shutdown`) and the v1 session actors, whose detached
//! `close(Force)` is the longest-lived compensation task pmux spawns.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::watch;

/// Tracks detached cancellation-compensation work without making a caller own
/// its `JoinHandle`. A dropped public request can therefore leave a bounded
/// task running, while daemon shutdown still has an exact awaitable fence.
#[derive(Debug)]
pub struct TrackedTasks {
    active: AtomicUsize,
    changed: watch::Sender<u64>,
}

impl Default for TrackedTasks {
    fn default() -> Self {
        let (changed, _) = watch::channel(0);
        Self {
            active: AtomicUsize::new(0),
            changed,
        }
    }
}

impl TrackedTasks {
    /// Registers one task. The permit must be moved *into* the spawned task, so
    /// that the count falls only when the work is actually over.
    #[must_use]
    pub fn track(self: &Arc<Self>) -> TrackedTask {
        self.active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                active.checked_add(1)
            })
            .expect("tracked task count overflowed");
        TrackedTask {
            tasks: Arc::clone(self),
        }
    }

    /// Waits until no tracked task is running.
    ///
    /// This is a fence, not a latch: work registered after it returns is not
    /// covered, so callers must first stop whatever can still register more.
    pub async fn wait_idle(&self) {
        let mut changed = self.changed.subscribe();
        loop {
            if self.active.load(Ordering::Acquire) == 0 {
                return;
            }
            changed
                .changed()
                .await
                .expect("tracked task change sender must outlive its waiters");
        }
    }
}

pub struct TrackedTask {
    tasks: Arc<TrackedTasks>,
}

impl Drop for TrackedTask {
    fn drop(&mut self) {
        let previous = self.tasks.active.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "tracked task count underflowed");
        self.tasks
            .changed
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }
}

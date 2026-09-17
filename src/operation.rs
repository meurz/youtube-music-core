//! Cooperative operation control shared by HTTP, locks and the embedded JS engine.
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    cell::RefCell,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, MutexGuard,
    },
    time::{Duration, Instant},
};

#[derive(Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct OperationOptions {
    pub timeout_ms: u64,
}
impl Default for OperationOptions {
    fn default() -> Self {
        Self {
            timeout_ms: 120_000,
        }
    }
}

#[derive(Clone)]
pub struct OperationContext(Arc<Inner>);
struct Inner {
    cancelled: AtomicBool,
    start: Instant,
    deadline: Instant,
    phase: Mutex<String>,
}

#[derive(Serialize)]
pub struct Progress {
    pub phase: String,
    pub elapsed_ms: u64,
    pub cancelled: bool,
}

thread_local! { static CURRENT: RefCell<Option<OperationContext>> = const { RefCell::new(None) }; }

impl OperationContext {
    pub fn new(options: OperationOptions) -> Result<Self> {
        if !(1..=600_000).contains(&options.timeout_ms) {
            return Err(Error::InvalidInput(
                "timeout_ms must be between 1 and 600000".into(),
            ));
        }
        let start = Instant::now();
        Ok(Self(Arc::new(Inner {
            cancelled: AtomicBool::new(false),
            start,
            deadline: start + Duration::from_millis(options.timeout_ms),
            phase: Mutex::new("queued".into()),
        })))
    }
    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::Release);
    }
    pub fn check(&self) -> Result<()> {
        if self.0.cancelled.load(Ordering::Acquire) {
            return Err(Error::Cancelled);
        }
        if Instant::now() >= self.0.deadline {
            return Err(Error::Timeout);
        }
        Ok(())
    }
    pub fn remaining(&self) -> Duration {
        self.0.deadline.saturating_duration_since(Instant::now())
    }
    pub fn progress(&self) -> Progress {
        Progress {
            phase: self
                .0
                .phase
                .lock()
                .map(|p| p.clone())
                .unwrap_or_else(|_| "unknown".into()),
            elapsed_ms: self.0.start.elapsed().as_millis().min(u64::MAX as u128) as u64,
            cancelled: self.0.cancelled.load(Ordering::Acquire),
        }
    }
    pub fn set_phase(&self, phase: &str) {
        if let Ok(mut p) = self.0.phase.lock() {
            *p = phase.into();
        }
    }
    pub fn run<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        with_context(self, f)
    }
}
pub fn current() -> Option<OperationContext> {
    CURRENT.with(|c| c.borrow().clone())
}
pub fn check() -> Result<()> {
    current().map_or(Ok(()), |c| c.check())
}
pub fn phase(value: &str) {
    if let Some(c) = current() {
        c.set_phase(value);
    }
}

pub fn with_context<T>(context: &OperationContext, f: impl FnOnce() -> Result<T>) -> Result<T> {
    struct Restore(Option<OperationContext>);
    impl Drop for Restore {
        fn drop(&mut self) {
            CURRENT.with(|c| *c.borrow_mut() = self.0.take());
        }
    }
    let _restore = Restore(CURRENT.with(|c| c.replace(Some(context.clone()))));
    context.check()?;
    f()
}

pub(crate) fn ensure<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    if current().is_some() {
        check()?;
        f()
    } else {
        OperationContext::new(OperationOptions::default())?.run(f)
    }
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>> {
    loop {
        check()?;
        match mutex.try_lock() {
            Ok(value) => return Ok(value),
            Err(std::sync::TryLockError::Poisoned(_)) => {
                return Err(Error::Protocol("state lock unavailable".into()))
            }
            Err(std::sync::TryLockError::WouldBlock) => {
                std::thread::sleep(Duration::from_millis(10))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancellation_interrupts_lock_wait_and_context_is_restored() {
        let m = Arc::new(Mutex::new(()));
        let held = m.lock().unwrap();
        let c = OperationContext::new(OperationOptions::default()).unwrap();
        let worker_context = c.clone();
        let other = m.clone();
        let worker = std::thread::spawn(move || {
            worker_context.run(|| {
                let _guard = lock(&other)?;
                Ok(())
            })
        });
        c.cancel();
        assert!(matches!(worker.join().unwrap(), Err(Error::Cancelled)));
        drop(held);
        assert!(current().is_none());
    }
    #[test]
    fn total_deadline_includes_time_before_execution() {
        let c = OperationContext::new(OperationOptions { timeout_ms: 1 }).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        assert!(matches!(c.run(|| Ok(())), Err(Error::Timeout)));
        assert!(current().is_none());
    }
}

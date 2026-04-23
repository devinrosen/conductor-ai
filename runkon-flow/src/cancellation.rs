use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::cancellation_reason::CancellationReason;
use crate::engine_error::EngineError;

#[allow(dead_code)]
struct CancellationInner {
    cancelled: AtomicBool,
    reason: Mutex<Option<CancellationReason>>,
    parent: Option<Arc<CancellationInner>>,
}

impl CancellationInner {
    fn find_in_chain<T>(&self, f: impl Fn(&CancellationInner) -> Option<T>) -> Option<T> {
        let mut node = self;
        loop {
            if let Some(val) = f(node) {
                return Some(val);
            }
            match &node.parent {
                Some(p) => node = p,
                None => return None,
            }
        }
    }
}

#[allow(dead_code)]
#[derive(Clone)]
pub(crate) struct CancellationToken(Arc<CancellationInner>);

#[allow(dead_code)]
impl CancellationToken {
    pub(crate) fn new() -> Self {
        Self(Arc::new(CancellationInner {
            cancelled: AtomicBool::new(false),
            reason: Mutex::new(None),
            parent: None,
        }))
    }

    pub(crate) fn child(&self) -> Self {
        Self(Arc::new(CancellationInner {
            cancelled: AtomicBool::new(false),
            reason: Mutex::new(None),
            parent: Some(self.0.clone()),
        }))
    }

    pub(crate) fn cancel(&self, reason: CancellationReason) {
        if self
            .0
            .cancelled
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            *self.0.reason.lock().unwrap_or_else(|e| e.into_inner()) = Some(reason);
        }
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.0
            .find_in_chain(|n| n.cancelled.load(Ordering::SeqCst).then_some(()))
            .is_some()
    }

    pub(crate) fn reason(&self) -> Option<CancellationReason> {
        self.0
            .find_in_chain(|n| n.reason.lock().unwrap_or_else(|e| e.into_inner()).clone())
    }

    pub(crate) fn error_if_cancelled(&self) -> Result<(), EngineError> {
        if self.is_cancelled() {
            Err(EngineError::Cancelled(
                self.reason().unwrap_or(CancellationReason::ParentCancelled),
            ))
        } else {
            Ok(())
        }
    }
}

#[allow(dead_code)]
pub(crate) struct ExecutionContext<'a> {
    pub run: &'a dyn crate::traits::run_context::RunContext,
    pub cancellation: &'a CancellationToken,
}

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::engine_error::{CancellationReason, EngineError};

#[allow(dead_code)]
struct CancellationInner {
    cancelled: AtomicBool,
    // Only set on the false→true transition; `.lock().unwrap()` is safe because
    // we never panic while holding this lock.
    reason: Mutex<Option<CancellationReason>>,
    parent: Option<Arc<CancellationInner>>,
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

    /// Create a child token. Parent cancellation propagates to child;
    /// child cancellation does NOT propagate to parent.
    pub(crate) fn child(&self) -> Self {
        Self(Arc::new(CancellationInner {
            cancelled: AtomicBool::new(false),
            reason: Mutex::new(None),
            parent: Some(self.0.clone()),
        }))
    }

    /// Cancel this token with a reason. First call wins; subsequent calls are no-ops.
    pub(crate) fn cancel(&self, reason: CancellationReason) {
        if self
            .0
            .cancelled
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            *self.0.reason.lock().unwrap() = Some(reason);
        }
    }

    /// Returns true if this token or any ancestor is cancelled.
    pub(crate) fn is_cancelled(&self) -> bool {
        let mut node = &self.0;
        loop {
            if node.cancelled.load(Ordering::SeqCst) {
                return true;
            }
            match &node.parent {
                Some(p) => node = p,
                None => return false,
            }
        }
    }

    /// Returns the first cancellation reason found walking self → ancestors.
    pub(crate) fn reason(&self) -> Option<CancellationReason> {
        let mut node = &self.0;
        loop {
            let r = node.reason.lock().unwrap().clone();
            if r.is_some() {
                return r;
            }
            match &node.parent {
                Some(p) => node = p,
                None => return None,
            }
        }
    }

    /// Returns `Err(EngineError::Cancelled(...))` if this token or any ancestor is cancelled.
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
    pub run: &'a dyn crate::workflow::run_context::RunContext,
    pub cancellation: &'a CancellationToken,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_cancel_propagates_to_child() {
        let parent = CancellationToken::new();
        let child = parent.child();
        parent.cancel(CancellationReason::Timeout);
        assert!(child.is_cancelled());
    }

    #[test]
    fn child_cancel_does_not_propagate_to_parent() {
        let parent = CancellationToken::new();
        let child = parent.child();
        child.cancel(CancellationReason::FailFast);
        assert!(!parent.is_cancelled());
    }

    #[test]
    fn reason_returns_first_up_chain() {
        let parent = CancellationToken::new();
        let child = parent.child();
        // Only parent is cancelled — child walks up and finds parent's reason.
        parent.cancel(CancellationReason::Timeout);
        assert_eq!(child.reason(), Some(CancellationReason::Timeout));
    }

    #[test]
    fn reason_prefers_self_over_ancestor() {
        let parent = CancellationToken::new();
        let child = parent.child();
        // Child is cancelled first; parent is also cancelled later.
        child.cancel(CancellationReason::FailFast);
        parent.cancel(CancellationReason::Timeout);
        // reason() walks self first, so child's own reason wins.
        assert_eq!(child.reason(), Some(CancellationReason::FailFast));
    }

    #[test]
    fn multi_level_inheritance_propagates() {
        let grandparent = CancellationToken::new();
        let parent = grandparent.child();
        let child = parent.child();
        grandparent.cancel(CancellationReason::EngineShutdown);
        assert!(parent.is_cancelled());
        assert!(child.is_cancelled());
        assert_eq!(child.reason(), Some(CancellationReason::EngineShutdown));
    }

    #[test]
    fn error_if_cancelled_returns_cancelled_variant() {
        use super::super::engine_error::EngineError;
        let token = CancellationToken::new();
        token.cancel(CancellationReason::Timeout);
        let err = token.error_if_cancelled().unwrap_err();
        assert!(matches!(
            err,
            EngineError::Cancelled(CancellationReason::Timeout)
        ));
    }

    #[test]
    fn cancel_is_idempotent_first_reason_wins() {
        let token = CancellationToken::new();
        token.cancel(CancellationReason::UserRequested(Some("first".into())));
        token.cancel(CancellationReason::Timeout);
        assert_eq!(
            token.reason(),
            Some(CancellationReason::UserRequested(Some("first".into())))
        );
    }

    #[test]
    fn clone_shares_state() {
        let token = CancellationToken::new();
        let cloned = token.clone();
        token.cancel(CancellationReason::FailFast);
        assert!(cloned.is_cancelled());
        assert_eq!(cloned.reason(), Some(CancellationReason::FailFast));
    }

    #[test]
    fn error_if_cancelled_ok_when_not_cancelled() {
        let token = CancellationToken::new();
        assert!(token.error_if_cancelled().is_ok());
    }

    #[test]
    fn error_if_cancelled_returns_err_for_inherited_parent_cancellation() {
        use super::super::engine_error::EngineError;
        let parent = CancellationToken::new();
        let child = parent.child();
        parent.cancel(CancellationReason::UserRequested(Some("stop".into())));
        let err = child.error_if_cancelled().unwrap_err();
        assert!(matches!(
            err,
            EngineError::Cancelled(CancellationReason::UserRequested(_))
        ));
    }
}

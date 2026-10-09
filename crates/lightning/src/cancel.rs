//! Cooperative cancellation shared by the session, dispatch and whoever owns Ctrl+C
//! (the CLI) or the Stop button (the GUI).
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::Notify;

/// Clone freely; every clone observes the same cancellation.
#[derive(Clone, Default)]
pub struct CancelToken {
    inner: Arc<CancelInner>,
}

#[derive(Default)]
struct CancelInner {
    flag: AtomicBool,
    notify: Notify,
}

impl CancelToken {
    pub fn cancel(&self) {
        self.inner.flag.store(true, Ordering::SeqCst);
        self.inner.notify.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.flag.load(Ordering::SeqCst)
    }

    /// Resolves once cancelled.
    pub async fn cancelled(&self) {
        loop {
            let notified = self.inner.notify.notified();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn cancelled_resolves_after_cancel_and_immediately_if_already_cancelled() {
        let token = CancelToken::default();
        assert!(!token.is_cancelled());
        let waiter = token.clone();
        let handle = tokio::spawn(async move { waiter.cancelled().await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        token.cancel();
        tokio::time::timeout(Duration::from_secs(2), handle).await.unwrap().unwrap();
        tokio::time::timeout(Duration::from_millis(100), token.cancelled()).await.unwrap();
    }
}

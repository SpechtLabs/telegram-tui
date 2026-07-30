//! The `TdRuntime` trait: the sole seam between core and any TDLib
//! implementation (real or fake). See docs/architecture.md §4.7.

use crate::td::error::TdError;
use crate::td::request::{TdRequest, TdResponse};
use crate::td::update::TdUpdate;
use async_trait::async_trait;
use tokio::sync::mpsc;

#[async_trait]
pub trait TdRuntime: Send + Sync + 'static {
    async fn request(&self, req: TdRequest) -> Result<TdResponse, TdError>;
    /// Called exactly once by the runtime loop at boot; panics on second call.
    fn updates(&self) -> mpsc::Receiver<TdUpdate>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Minimal double proving the trait is object-safe and callable exactly
    /// as declared; real coverage of `TdRuntime` behavior lives in T09/T10.
    struct StubRuntime {
        receiver: Mutex<Option<mpsc::Receiver<TdUpdate>>>,
    }

    #[async_trait]
    impl TdRuntime for StubRuntime {
        async fn request(&self, _req: TdRequest) -> Result<TdResponse, TdError> {
            Ok(TdResponse::Ok)
        }

        fn updates(&self) -> mpsc::Receiver<TdUpdate> {
            self.receiver
                .lock()
                .unwrap()
                .take()
                .expect("updates() called twice")
        }
    }

    #[tokio::test]
    async fn trait_is_object_safe_and_dispatches() {
        let (_tx, rx) = mpsc::channel(1);
        let runtime: Box<dyn TdRuntime> = Box::new(StubRuntime {
            receiver: Mutex::new(Some(rx)),
        });
        let resp = runtime.request(TdRequest::LogOut).await.unwrap();
        assert_eq!(resp, TdResponse::Ok);
        let _ = runtime.updates();
    }

    #[test]
    #[should_panic(expected = "updates() called twice")]
    fn updates_panics_on_second_call() {
        let (_tx, rx) = mpsc::channel(1);
        let runtime = StubRuntime {
            receiver: Mutex::new(Some(rx)),
        };
        let _ = runtime.updates();
        let _ = runtime.updates();
    }
}

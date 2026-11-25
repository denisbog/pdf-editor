//! "Something happened on a worker thread" — the one thing the engine needs from a window.
//!
//! Under egui this is `egui::Context::request_repaint`; under iced it is an
//! `iced::task::Sender` (or a flag polled by a subscription). The engine only sees a closure,
//! so it never learns which toolkit is on the other side.

use std::sync::Arc;

/// A callback a background thread calls when it has something the UI should look at.
#[derive(Clone, Default)]
pub struct Waker(Option<Arc<dyn Fn() + Send + Sync>>);

impl Waker {
    /// A waker that calls `wake` after every finished compile or raster.
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self(Some(Arc::new(wake)))
    }

    /// A waker that does nothing (useful for the CLI, where there is no window).
    pub fn none() -> Self {
        Self(None)
    }

    pub fn wake(&self) {
        if let Some(wake) = &self.0 {
            wake();
        }
    }

    pub fn has_window(&self) -> bool {
        self.0.is_some()
    }
}

impl std::fmt::Debug for Waker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Waker").field(&self.0.is_some()).finish()
    }
}

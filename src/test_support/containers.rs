//! A fake podman that records each container it is asked to stop.

use std::{cell::RefCell, rc::Rc};

use futures_util::{FutureExt as _, future::LocalBoxFuture};

use super::{Call, FakeShepherd};
use crate::backend::Containers;

/// Records each stop, with how many sheep the shepherd had stopped by then, and stops nothing.
#[derive(Debug, Clone)]
pub(crate) struct FakeContainers {
    shepherd: FakeShepherd,
    stopped: Rc<RefCell<Vec<(String, usize)>>>,
    fails: bool,
}

impl FakeContainers {
    /// A podman whose stops succeed, counting `shepherd`'s stops as each comes.
    pub(crate) fn after(shepherd: &FakeShepherd) -> Self {
        Self {
            shepherd: shepherd.clone(),
            stopped: Rc::default(),
            fails: false,
        }
    }

    /// The same podman, whose every stop fails.
    pub(crate) fn failing(self) -> Self {
        Self {
            fails: true,
            ..self
        }
    }

    /// Each container asked to stop, with how many sheep stops the shepherd had seen before it.
    pub(crate) fn stopped(&self) -> Vec<(String, usize)> {
        self.stopped.borrow().clone()
    }
}

impl Containers for FakeContainers {
    fn stop(&self, name: &str) -> LocalBoxFuture<'_, Result<(), String>> {
        let sheep_stops = self
            .shepherd
            .calls()
            .iter()
            .filter(|call| matches!(call, Call::Stop(_)))
            .count();
        self.stopped
            .borrow_mut()
            .push((name.to_owned(), sheep_stops));
        let result = if self.fails {
            Err("podman could not be run".to_owned())
        } else {
            Ok(())
        };
        core::future::ready(result).boxed_local()
    }
}

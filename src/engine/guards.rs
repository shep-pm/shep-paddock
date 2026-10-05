//! Values that tell the engine something when they are dropped.

use tokio::sync::mpsc::UnboundedSender;

use super::Command;
use crate::{book::WaiterId, config::ModelName};

/// Ends one in-flight request when dropped, so a client that hangs up mid-stream still counts down
#[derive(Debug)]
pub(crate) struct InFlight {
    model: ModelName,
    tx: UnboundedSender<Command>,
}

impl InFlight {
    pub(super) fn new(model: ModelName, tx: UnboundedSender<Command>) -> InFlight {
        InFlight { model, tx }
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        // A stopped engine has nothing left to count down.
        let _ = self.tx.send(Command::Finished {
            model: self.model.clone(),
        });
    }
}

/// Tells the engine a waiting request's client left, unless the request was answered first
#[derive(Debug)]
pub(super) struct WaiterGuard {
    waiter: WaiterId,
    tx: UnboundedSender<Command>,
    answered: bool,
}

impl WaiterGuard {
    pub fn new(waiter: WaiterId, tx: UnboundedSender<Command>) -> WaiterGuard {
        WaiterGuard {
            waiter,
            tx,
            answered: false,
        }
    }

    /// Drops the guard without telling the engine anything
    pub fn answered(mut self) {
        self.answered = true;
    }
}

impl Drop for WaiterGuard {
    fn drop(&mut self) {
        if !self.answered {
            // A stopped engine has no waiter to forget.
            let _ = self.tx.send(Command::WaiterGone {
                waiter: self.waiter,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc::unbounded_channel;

    use super::*;

    #[test]
    fn dropping_in_flight_finishes_the_request() {
        let (tx, mut rx) = unbounded_channel();
        drop(InFlight::new(ModelName::from("laya"), tx));

        assert!(matches!(
            rx.try_recv(),
            Ok(Command::Finished { model }) if model == ModelName::from("laya")
        ));
    }

    #[test]
    fn a_dropped_waiter_guard_says_the_waiter_is_gone() {
        let (tx, mut rx) = unbounded_channel();
        drop(WaiterGuard::new(WaiterId(4), tx));

        assert!(matches!(
            rx.try_recv(),
            Ok(Command::WaiterGone {
                waiter: WaiterId(4)
            })
        ));
    }

    #[test]
    fn an_answered_waiter_guard_says_nothing() {
        let (tx, mut rx) = unbounded_channel();
        WaiterGuard::new(WaiterId(4), tx).answered();

        assert!(rx.try_recv().is_err());
    }
}

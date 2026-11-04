//! A meter that answers what the test sets, and the host beneath the parts meter.

use std::{cell::RefCell, collections::VecDeque};

use futures_util::{FutureExt as _, future::LocalBoxFuture};

use crate::meter::{Host, Meter, ReadError, Reading};

/// A meter that answers its readings in order, then reports the plug unreachable
#[derive(Debug, Default)]
pub(crate) struct FakeMeter {
    answers: RefCell<VecDeque<Result<Reading, ReadError>>>,
}

impl FakeMeter {
    /// A meter that answers `answers`, one per read.
    pub(crate) fn answering(answers: Vec<Result<Reading, ReadError>>) -> Self {
        Self {
            answers: RefCell::new(answers.into()),
        }
    }
}

impl Meter for FakeMeter {
    fn read(&self) -> LocalBoxFuture<'_, Result<Reading, ReadError>> {
        let answer = self
            .answers
            .borrow_mut()
            .pop_front()
            .unwrap_or(Err(ReadError::Unreachable));
        async move { answer }.boxed_local()
    }
}

/// A host whose `nvidia-smi` prints what the test says and whose counter reads what it is given
///
/// Each read of the counter takes the next of `counts`; one past the last finds the counter
/// unreadable.
#[derive(Debug, Default)]
pub(crate) struct FakeSensors {
    printed: Option<String>,
    counts: RefCell<VecDeque<u64>>,
    range: Option<u64>,
}

impl FakeSensors {
    /// A host whose `nvidia-smi` prints `printed`, whose counter reads `counts` in turn, and
    /// wraps at `range`.
    pub(crate) fn reading(printed: &str, counts: &[u64], range: u64) -> Self {
        Self {
            printed: Some(printed.to_owned()),
            counts: RefCell::new(counts.iter().copied().collect()),
            range: Some(range),
        }
    }

    /// The same host without `nvidia-smi`.
    pub(crate) fn without_gpu(mut self) -> Self {
        self.printed = None;
        self
    }
}

impl Host for FakeSensors {
    fn gpu_draw(&self) -> LocalBoxFuture<'_, Option<String>> {
        let printed = self.printed.clone();
        async move { printed }.boxed_local()
    }

    fn energy_uj(&self) -> LocalBoxFuture<'_, Option<u64>> {
        let count = self.counts.borrow_mut().pop_front();
        async move { count }.boxed_local()
    }

    fn energy_range_uj(&self) -> LocalBoxFuture<'_, Option<u64>> {
        let range = self.range;
        async move { range }.boxed_local()
    }
}

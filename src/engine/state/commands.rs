//! Answering what the engine's handle asks: admissions, leases and their changes, and reloads.

use std::{collections::VecDeque, sync::Arc};

use super::{Engine, Watched};
use crate::{
    book::{Event, LeaseAsk},
    engine::{Admission, Command},
};

impl Engine {
    pub fn command(&mut self, command: Command) {
        match command {
            Command::Admit {
                waiter,
                client,
                model,
                priority,
                max_wait,
                reply,
            } => {
                if reply.is_closed() {
                    return;
                }
                if !self.config.models.contains_key(&model) {
                    let _ = reply.send(Admission::Unknown);
                    return;
                }
                self.requests.insert(waiter, reply);
                self.mark_activity(&client, &model);
                self.feed(Event::RequestArrived {
                    waiter,
                    client,
                    model,
                    priority,
                    max_wait,
                });
            }
            Command::TakeLease {
                waiter,
                client,
                ask,
                events,
            } => {
                // Its watcher would only hear the hang-up after a grant in this same step.
                if events.is_closed() {
                    return;
                }
                let ask = LeaseAsk {
                    lease: self.next_lease(),
                    client,
                    model: ask.model,
                    priority: ask.priority,
                    expected: ask.expected,
                    max_wait: ask.max_wait,
                    hold: ask.hold,
                    note: ask.note,
                    reclaimable: ask.reclaimable,
                    release_if_idle: ask.release_if_idle,
                };
                self.watch(Watched::Waiter(waiter), events.clone());
                self.waiting_leases.insert(waiter, events);
                self.feed(Event::LeaseAsked { waiter, ask });
            }
            Command::Attach {
                client,
                lease,
                events,
                reply,
            } => {
                let attached = self.attach(&client, lease, events);
                let _ = reply.send(attached);
            }
            Command::Renew {
                client,
                lease,
                reply,
            } => {
                let renewed = self
                    .owned(&client, lease)
                    .map(|()| self.feed(Event::LeaseRenewed { lease }));
                let _ = reply.send(renewed);
            }
            Command::Note {
                client,
                lease,
                note,
                reply,
            } => {
                let noted = self
                    .owned(&client, lease)
                    .map(|()| self.feed(Event::LeaseNoted { lease, note }));
                let _ = reply.send(noted);
            }
            Command::Release {
                client,
                lease,
                reply,
            } => {
                let released = self
                    .owned(&client, lease)
                    .map(|()| self.feed(Event::LeaseReleased { lease }));
                let _ = reply.send(released);
            }
            Command::Snapshot { reply } => {
                let _ = reply.send(self.snapshot());
            }
            Command::Reconfigure { config, done } => {
                self.config = Arc::clone(&config);
                let actions = self.book.reconfigure(self.clock.moment(), config);
                let mut queue = VecDeque::new();
                self.apply(actions, &mut queue);
                while let Some(event) = queue.pop_front() {
                    self.feed(event);
                }
                self.forget_unnamed_sheep();
                // A reload can start a load, or fail a holder's queued request, without a feed.
                self.save_changes();
                let _ = done.send(());
            }
            Command::WaiterGone { waiter } => {
                self.requests.remove(&waiter);
                self.feed(Event::WaiterGone { waiter });
            }
            Command::Finished { model, client } => {
                self.feed(Event::RequestFinished { model, client });
            }
        }
    }
}

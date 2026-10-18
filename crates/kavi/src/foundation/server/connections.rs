use std::{
    future::{Future, poll_fn},
    num::NonZeroUsize,
    pin::Pin,
    task::{Context, Poll, Waker},
    time::Instant,
};

use karmaio::runtime::{JoinError, JoinHandle, spawn_local};

use super::{Connection, ConnectionControl};

/// Identity of one admitted connection task within its owning set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConnectionId(u64);

/// A completed connection, preserving its output and any task panic/cancellation.
#[must_use = "inspect connection errors and handle any upgraded transport"]
pub struct Completion<T> {
    /// The identity assigned when the connection was admitted.
    pub id: ConnectionId,
    /// The original driver result, or its task join failure.
    pub result: Result<T, JoinError>,
}

struct Entry<T> {
    id: ConnectionId,
    task: JoinHandle<T>,
    control: ConnectionControl,
}

/// A bounded local task set explicitly driven by the application's accept loop.
///
/// Completed tasks occupy a slot until reaped. No listener or process signals are
/// owned here. Shutdown signals connection controls and awaits their drivers;
/// task abort is never substituted for transport settlement. Dropping the set
/// requests abort and detaches handles, so it does not establish settlement.
/// Await shutdown before shutting down dependencies or dropping the runtime.
#[must_use = "drive and explicitly shut down the connection set"]
pub struct Connections<T> {
    entries: Vec<Entry<T>>,
    limit: NonZeroUsize,
    next: u64,
    stopping: bool,
}

impl<T: 'static> Connections<T> {
    /// Creates a local set with a nonzero connection admission limit.
    pub fn new(limit: NonZeroUsize) -> Self {
        Self {
            entries: Vec::new(),
            limit,
            next: 0,
            stopping: false,
        }
    }
    /// Returns tracked connections, including completed results not yet reaped.
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    /// Returns whether every admitted connection result has been reaped.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    /// Returns whether admission is stopped or the connection cap is occupied.
    pub fn is_full(&self) -> bool {
        self.stopping || self.entries.len() >= self.limit.get()
    }

    /// Spawns one local driver if admission is open and capacity is available.
    ///
    /// # Errors
    /// Returns the untouched connection on overload or after shutdown starts.
    pub fn try_spawn<F: Future<Output = T> + 'static>(
        &mut self,
        connection: Connection<F>,
    ) -> Result<ConnectionId, Connection<F>> {
        if self.is_full() {
            return Err(connection);
        }
        let id = ConnectionId(self.next);
        self.next = self.next.checked_add(1).expect("connection identity overflow");
        let control = connection.control();
        self.entries.push(Entry {
            id,
            control,
            task: spawn_local(connection.run()),
        });
        Ok(id)
    }

    /// Reaps one completed task without waiting or polling unrelated I/O.
    pub fn try_next(&mut self) -> Option<Completion<T>> {
        let index = self.entries.iter().position(|entry| entry.task.is_finished())?;
        let mut entry = self.entries.swap_remove(index);
        let Poll::Ready(result) = Pin::new(&mut entry.task).poll(&mut Context::from_waker(Waker::noop())) else {
            unreachable!("a finished Karmaio task has a ready join result");
        };
        Some(Completion { id: entry.id, result })
    }

    /// Awaits and reaps one result, or returns None for an empty set.
    ///
    /// Dropping this wait leaves all tasks and handles in the set.
    pub async fn next(&mut self) -> Option<Completion<T>> {
        let ready = poll_fn(|cx| {
            if self.entries.is_empty() {
                return Poll::Ready(None);
            }
            for (index, entry) in self.entries.iter_mut().enumerate() {
                if let Poll::Ready(result) = Pin::new(&mut entry.task).poll(cx) {
                    return Poll::Ready(Some((index, result)));
                }
            }
            Poll::Pending
        })
        .await;
        ready.map(|(index, result)| Completion {
            id: self.entries.swap_remove(index).id,
            result,
        })
    }

    /// Stops admission and requests graceful draining with an absolute deadline.
    ///
    /// Vakya escalates to cancellation at the deadline and settles retained I/O.
    /// Escalation drops application futures; it does not await their async cleanup.
    /// Repeated requests cannot extend an earlier deadline. Continue driving the set.
    pub fn begin_shutdown(&mut self, deadline: Instant) {
        self.stopping = true;
        for entry in &self.entries {
            entry.control.graceful_shutdown_with_deadline(deadline);
        }
    }

    /// Stops admission and requests immediate transport cancellation and settlement.
    /// Application futures are dropped without awaiting async cleanup.
    /// Continue driving the set to observe completion.
    pub fn abort(&mut self) {
        self.stopping = true;
        for entry in &self.entries {
            entry.control.abort();
        }
    }

    /// Drains and returns all remaining results after requesting graceful shutdown.
    ///
    /// Custom I/O must cooperate with cancellation; the deadline cannot make
    /// noncooperative transport futures complete. Upgraded transports in returned
    /// results require a separate caller-owned lifecycle.
    /// At the deadline, Vakya drops application futures and settles transport I/O.
    /// This does not guarantee application I/O settlement or async handler cleanup.
    pub async fn shutdown(&mut self, deadline: Instant) -> Vec<Completion<T>> {
        self.begin_shutdown(deadline);
        let mut completed = Vec::with_capacity(self.entries.len());
        while let Some(result) = self.next().await {
            completed.push(result);
        }
        completed
    }
}

impl<T> Drop for Connections<T> {
    fn drop(&mut self) {
        for entry in &self.entries {
            entry.control.abort();
        }
    }
}

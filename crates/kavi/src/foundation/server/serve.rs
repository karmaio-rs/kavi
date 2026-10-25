use std::{
    future::{Future, pending, poll_fn},
    io,
    net::SocketAddr,
    num::NonZeroUsize,
    pin::{Pin, pin},
    rc::Rc,
    task::Poll,
    time::{Duration, Instant},
};

use karmaio::{
    io::{Stream, StreamExt},
    net::tcp::{TcpListener, TcpStream},
    runtime::{CancellationSource, is_operation_canceled},
};

use super::{Completion, Connection, ConnectionError, ConnectionOutcome, Connections, Http1Server, http1::TcpOutcome};
use crate::{Application, http::ConnectionInfo};

/// The result of a supervised HTTP connection after its transport has settled.
///
/// Returned upgrades are closed and reported as unsupported I/O errors. The
/// listener runner does not supervise upgraded protocols or detached app tasks.
pub type ConnectionResult = Result<(), ConnectionError>;

/// Configures optional TCP acceptance and local HTTP/1 connection supervision.
///
/// Created by [`Http1Server::serve`]. Defaults to 1024 active connections and a
/// 30-second grace period. The application owns runtime creation, listener
/// binding, stop policy, and dependency cleanup. No `Send` or `Sync` is required.
///
/// Linux uses Karmaio's io_uring multishot accept stream (Linux 6.12+). Other
/// platforms retain a single submitted accept until completion. At capacity,
/// acceptance is canceled and settled, including closing queued/racing sockets,
/// before waiting for a free slot. Linux's transient accepted-socket queue is
/// separately bounded by `RuntimeBuilder::multishot_accept_capacity`.
///
/// Dropping the running future forfeits awaited shutdown. Deadline escalation
/// drops application futures and awaits HTTP transport settlement, not arbitrary
/// application I/O or asynchronous handler cleanup. Task panics are reported as
/// errors and cannot establish the driver's normal settlement guarantee.
#[must_use = "await run_until before shutting down application resources"]
pub struct Serve<F = fn(Completion<ConnectionResult>)> {
    server: Http1Server,
    listener: TcpListener,
    application: Rc<Application>,
    limit: NonZeroUsize,
    grace: Duration,
    on_connection: F,
}

impl Serve {
    pub(super) fn new(server: Http1Server, listener: TcpListener, application: Rc<Application>) -> Self {
        Self {
            server,
            listener,
            application,
            limit: NonZeroUsize::new(1024).expect("default connection limit is nonzero"),
            grace: Duration::from_secs(30),
            on_connection: drop,
        }
    }
}

impl<F> Serve<F> {
    /// Sets the maximum number of concurrently supervised HTTP connections.
    pub fn max_connections(mut self, limit: NonZeroUsize) -> Self {
        self.limit = limit;
        self
    }

    /// Sets the time allowed to finish accepted HTTP work after stopping begins.
    ///
    /// Zero requests immediate cancellation. At the deadline, retained transport
    /// I/O must still settle, so this is not a hard bound on shutdown duration.
    pub fn grace_period(mut self, grace: Duration) -> Self {
        self.grace = grace;
        self
    }

    /// Observes each reaped connection result, including task panic/cancellation.
    ///
    /// The callback runs synchronously on the serving worker during normal
    /// operation and shutdown. Integrate transport-error logging here. Without
    /// a callback, ordinary connection errors are discarded. A task failure also
    /// stops serving and is returned by [`Self::run_until`]. Callbacks must not
    /// panic or block; unwinding forfeits awaited shutdown.
    pub fn on_connection<G: FnMut(Completion<ConnectionResult>)>(self, callback: G) -> Serve<G> {
        Serve {
            server: self.server,
            listener: self.listener,
            application: self.application,
            limit: self.limit,
            grace: self.grace,
            on_connection: callback,
        }
    }
}

impl<F: FnMut(Completion<ConnectionResult>)> Serve<F> {
    /// Serves until the supplied stop future completes, then drains accepted work.
    ///
    /// Stop is checked before accepting another connection, including while at
    /// capacity or backing off after recoverable accept errors. Both successful
    /// and failed stop futures initiate draining. Acceptance is canceled and
    /// exhausted, the listener is closed, and all connection results are reaped
    /// before returning. The caller can then shut down application resources.
    ///
    /// # Errors
    /// Returns the first stop, fatal accept, socket metadata, close, or task-join
    /// error after attempting listener and connection cleanup. Failure to prepare
    /// acceptance stops serving. Accept completions with `InvalidInput`,
    /// `PermissionDenied`, `NotConnected`, or `NotFound` also stop serving; other
    /// accept errors, including unclassified runtime errors, retry after 100 ms.
    /// Classification uses portable [`io::ErrorKind`] values. Connection protocol
    /// errors, including grace-deadline timeouts, go to the completion callback instead. An
    /// unrepresentable grace deadline returns `InvalidInput` and requests abort.
    ///
    /// # Panics
    /// Panics outside a Karmaio runtime or if the completion callback panics.
    pub async fn run_until<S: Future<Output = io::Result<()>>>(self, stop: S) -> io::Result<()> {
        let Self {
            server,
            listener,
            application,
            limit,
            grace,
            mut on_connection,
        } = self;
        let mut connections = Connections::new(limit);
        let mut stop = pin!(stop);
        // Keep the listener future small even when native connection drivers
        // carry substantial completion state. This allocates once per runner.
        let mut result = Box::pin(accept_loop(
            &server,
            || incoming(&listener),
            application,
            &mut connections,
            stop.as_mut(),
            grace,
            &mut on_connection,
        ))
        .await;
        retain_error(&mut result, listener.close().await);
        while let Some(completion) = connections.next().await {
            retain_error(&mut result, observe(completion, &mut on_connection));
        }
        result
    }
}

type Accepted = io::Result<(TcpStream, SocketAddr)>;

// All races borrow the current accept/stream-next future. Reaping a connection
// never drops a submitted accept. An empty set is not a ready event.
enum Event<T> {
    Stop(io::Result<()>),
    Connection(Completion<ConnectionResult>),
    Operation(T),
}

async fn next_event<S: Future<Output = io::Result<()>>, O: Future>(
    mut stop: Pin<&mut S>,
    connections: &mut Connections<ConnectionResult>,
    mut operation: Pin<&mut O>,
) -> Event<O::Output> {
    let mut completion = pin!(async {
        if connections.is_empty() {
            pending().await
        } else {
            connections.next().await.expect("a nonempty set yields a completion")
        }
    });
    poll_fn(|cx| {
        if let Poll::Ready(result) = stop.as_mut().poll(cx) {
            return Poll::Ready(Event::Stop(result));
        }
        if let Poll::Ready(result) = completion.as_mut().poll(cx) {
            return Poll::Ready(Event::Connection(result));
        }
        operation.as_mut().poll(cx).map(Event::Operation)
    })
    .await
}

// Quiescing always retains and exhausts the current native accept stream.
// Its continuation is applied only after the terminal completion is observed.
enum Admission {
    Accepting,
    Quiescing(Resume),
    WaitingForCapacity,
    BackingOff,
    Stopping(io::Result<()>),
}

#[derive(Clone, Copy)]
enum Resume {
    Capacity,
    Backoff,
}

impl Admission {
    fn stop(&mut self, connections: &mut Connections<ConnectionResult>, grace: Duration, cause: io::Result<()>) {
        match self {
            Self::Stopping(result) => retain_error(result, cause),
            _ => *self = Self::Stopping(begin_shutdown(connections, grace, cause)),
        }
    }

    fn stream_ended(self) -> Self {
        match self {
            Self::Quiescing(Resume::Capacity) => Self::WaitingForCapacity,
            Self::Quiescing(Resume::Backoff) => Self::BackingOff,
            state => state,
        }
    }
}

enum Activity<T> {
    Stop(io::Result<()>),
    Operation(T),
}

// Consume successful connection completions here so every phase observes task
// failures and stop notifications with the same ordering. The operation itself
// remains pinned across reaping, including a submitted one-shot accept.
async fn next_activity<S: Future<Output = io::Result<()>>, O: Future>(
    mut stop: Pin<&mut S>,
    connections: &mut Connections<ConnectionResult>,
    mut operation: Pin<&mut O>,
    callback: &mut impl FnMut(Completion<ConnectionResult>),
) -> Activity<O::Output> {
    loop {
        match next_event(stop.as_mut(), connections, operation.as_mut()).await {
            Event::Stop(result) => return Activity::Stop(result),
            Event::Operation(result) => return Activity::Operation(result),
            Event::Connection(completion) => {
                if let Err(error) = observe(completion, callback) {
                    return Activity::Stop(Err(error));
                }
            }
        }
    }
}

async fn accept_loop<S, F, I>(
    server: &Http1Server,
    mut make_incoming: impl FnMut() -> io::Result<I>,
    application: Rc<Application>,
    connections: &mut Connections<ConnectionResult>,
    mut stop: Pin<&mut S>,
    grace: Duration,
    callback: &mut F,
) -> io::Result<()>
where
    S: Future<Output = io::Result<()>>,
    F: FnMut(Completion<ConnectionResult>),
    I: Stream<Item = Accepted>,
{
    let mut admission = Admission::Accepting;
    loop {
        admission = match admission {
            Admission::Accepting => match make_incoming() {
                Ok(stream) => {
                    accept_batch(
                        server,
                        stream,
                        &application,
                        connections,
                        stop.as_mut(),
                        grace,
                        callback,
                    )
                    .await
                }
                Err(error) => Admission::Stopping(begin_shutdown(connections, grace, Err(error))),
            },
            Admission::WaitingForCapacity => {
                if !connections.is_full() {
                    Admission::Accepting
                } else {
                    // One completion releases capacity. Other successful
                    // completions can remain tracked until the next batch.
                    let mut wait = pin!(pending::<()>());
                    match next_event(stop.as_mut(), connections, wait.as_mut()).await {
                        Event::Stop(result) => Admission::Stopping(begin_shutdown(connections, grace, result)),
                        Event::Connection(completion) => match observe(completion, callback) {
                            Ok(()) => Admission::Accepting,
                            Err(error) => Admission::Stopping(begin_shutdown(connections, grace, Err(error))),
                        },
                        Event::Operation(()) => unreachable!(),
                    }
                }
            }
            Admission::BackingOff => {
                let mut delay = pin!(karmaio::time::sleep(Duration::from_millis(100)));
                match next_activity(stop.as_mut(), connections, delay.as_mut(), callback).await {
                    Activity::Stop(result) => Admission::Stopping(begin_shutdown(connections, grace, result)),
                    Activity::Operation(()) => Admission::Accepting,
                }
            }
            Admission::Stopping(result) => return result,
            Admission::Quiescing(_) => unreachable!("a batch exhausts acceptance before returning"),
        };
    }
}

async fn accept_batch<S, F, I>(
    server: &Http1Server,
    incoming: I,
    application: &Rc<Application>,
    connections: &mut Connections<ConnectionResult>,
    mut stop: Pin<&mut S>,
    grace: Duration,
    callback: &mut F,
) -> Admission
where
    S: Future<Output = io::Result<()>>,
    F: FnMut(Completion<ConnectionResult>),
    I: Stream<Item = Accepted>,
{
    let cancellation = CancellationSource::new();
    let mut stream = incoming.with_cancellation(cancellation.token());
    let mut admission = Admission::Accepting;
    loop {
        if connections.is_full() && matches!(admission, Admission::Accepting) {
            admission = Admission::Quiescing(Resume::Capacity);
        }
        if !matches!(admission, Admission::Accepting) {
            cancellation.cancel();
        }
        let item = {
            let mut next = pin!(stream.next());
            if matches!(admission, Admission::Stopping(_)) {
                next.await
            } else {
                match next_activity(stop.as_mut(), connections, next.as_mut(), callback).await {
                    Activity::Operation(item) => item,
                    Activity::Stop(result) => {
                        admission.stop(connections, grace, result);
                        cancellation.cancel();
                        next.await
                    }
                }
            }
        };
        match item {
            // Successful disarming is rearmed immediately. Only actual
            // recoverable failures enter BackingOff after stream exhaustion.
            None => return admission.stream_ended(),
            Some(Err(error)) if cancellation.is_cancel_requested() && is_operation_canceled(&error) => {}
            Some(Err(error)) if recoverable(&error) => {
                if !matches!(admission, Admission::Stopping(_)) {
                    admission = Admission::Quiescing(Resume::Backoff);
                }
            }
            Some(Err(error)) => admission.stop(connections, grace, Err(error)),
            Some(Ok((io, _))) if !matches!(admission, Admission::Accepting) => {
                if let Err(error) = io.close().await {
                    admission.stop(connections, grace, Err(error));
                }
            }
            Some(Ok((io, _))) => {
                let info = match connection_info(&io) {
                    Ok(info) => info,
                    Err(error) => {
                        admission.stop(connections, grace, Err(error));
                        cancellation.cancel();
                        if let Err(error) = io.close().await {
                            admission.stop(connections, grace, Err(error));
                        }
                        continue;
                    }
                };
                let driver = server.tcp_with_info(io, info, application.clone());
                // Pin inline completion state before Karmaio task allocation
                // to keep unoptimized builds within the worker's stack budget.
                let driver = Connection {
                    future: Box::pin(driver.future),
                    control: driver.control,
                };
                let connection = Connection {
                    control: driver.control(),
                    future: drive_tcp(driver),
                };
                connections
                    .try_spawn(connection)
                    .unwrap_or_else(|_| unreachable!("admission is open and capacity was checked before accepting"));
            }
        }
    }
}

fn connection_info(io: &TcpStream) -> io::Result<ConnectionInfo> {
    Ok(ConnectionInfo {
        remote_addr: io.peer_addr()?,
        local_addr: io.local_addr()?,
        scheme: http::uri::Scheme::HTTP,
    })
}

async fn drive_tcp(driver: Connection<impl Future<Output = TcpOutcome>>) -> ConnectionResult {
    match driver.run().await? {
        ConnectionOutcome::Closed => Ok(()),
        ConnectionOutcome::Upgraded(io) => {
            let (reader, writer, _, _) = io.into_parts();
            let io = reader
                .reunite(writer)
                .map_err(|_| io::Error::other("settled TCP upgrade could not be reunited"))?;
            io.close().await?;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "the listener runner does not support HTTP upgrades",
            )
            .into())
        }
        ConnectionOutcome::UpgradeClaimed(_) => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "an upgrade escaped HTTP connection supervision",
        )
        .into()),
    }
}

fn observe(
    completion: Completion<ConnectionResult>,
    callback: &mut impl FnMut(Completion<ConnectionResult>),
) -> io::Result<()> {
    let failure = completion
        .result
        .as_ref()
        .err()
        .map(|error| io::Error::other(error.to_string()));
    callback(completion);
    failure.map_or(Ok(()), Err)
}

fn begin_shutdown(
    connections: &mut Connections<ConnectionResult>,
    grace: Duration,
    mut result: io::Result<()>,
) -> io::Result<()> {
    if let Some(deadline) = Instant::now().checked_add(grace) {
        connections.begin_shutdown(deadline);
    } else {
        connections.abort();
        retain_error(
            &mut result,
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "shutdown grace period exceeds Instant range",
            )),
        );
    }
    result
}

fn retain_error(result: &mut io::Result<()>, next: io::Result<()>) {
    if result.is_ok() {
        *result = next;
    }
}

fn recoverable(error: &io::Error) -> bool {
    // Kavi owns a typed, already-bound listener. Fail known configuration and
    // listener-state errors; retry other accept failures with backoff, including
    // runtime errors without a portable classification. OS-specific error
    // normalization belongs to Karmaio rather than the framework.
    !matches!(
        error.kind(),
        io::ErrorKind::InvalidInput
            | io::ErrorKind::PermissionDenied
            | io::ErrorKind::NotConnected
            | io::ErrorKind::NotFound
    )
}

#[cfg(target_os = "linux")]
fn incoming(listener: &TcpListener) -> io::Result<impl Stream<Item = Accepted> + use<>> {
    listener.incoming()
}

#[cfg(not(target_os = "linux"))]
fn incoming(listener: &TcpListener) -> io::Result<impl Stream<Item = Accepted> + '_> {
    Ok(SingleAccept { listener, ended: false })
}

#[cfg(not(target_os = "linux"))]
struct SingleAccept<'a> {
    listener: &'a TcpListener,
    ended: bool,
}

#[cfg(not(target_os = "linux"))]
impl Stream for SingleAccept<'_> {
    type Item = Accepted;

    #[inline]
    async fn next(&mut self) -> Option<Self::Item> {
        if self.ended {
            return None;
        }
        let result = self.listener.accept().await;
        self.ended = result.is_err();
        Some(result)
    }
}

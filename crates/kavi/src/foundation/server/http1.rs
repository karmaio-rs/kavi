use std::{convert::Infallible, future::Future, rc::Rc};

use karmaio::{io::IntoOwnedSplit, net::tcp::TcpStream};

use super::{ConnectionControl, ConnectionError, ConnectionOutcome};
use crate::foundation::application::HeadRepresentation;
use crate::{
    Application,
    http::{
        Body, ConnectionInfo,
        body::{BodyData, Frame, HttpBody, IncomingAbort, SizeHint, TrailerHint},
    },
};

/// Configures plain HTTP/1 connection drivers for an assembled application.
///
/// Deploy behind a reverse proxy that owns public connections, TLS termination,
/// and client-facing protocol negotiation.
///
/// The caller supplies accepted I/O and drives the returned connection. No socket
/// is bound and no task is spawned. Requests with unread bodies are abandoned
/// safely using Vakya's close policy; this server never performs an unbounded
/// drain to preserve keep-alive. Protocol errors and late body/write errors are
/// connection results. Application dispatch always returns a Response.
#[derive(Clone, Default)]
pub struct Http1Server {
    protocol: vakya::server::conn::http1::Builder,
}

/// One owned HTTP connection activity and its shutdown control.
///
/// Dropping the driver forfeits graceful completion. Keep driving run after
/// requesting shutdown so retained I/O can settle. Runtime-owned application
/// resources must outlive that settlement. Upgrade outcomes remain caller-owned.
/// Forced abort drops application futures and only awaits transport settlement;
/// it does not guarantee asynchronous handler cleanup or application I/O settlement.
#[must_use = "drive the connection or register it with Connections"]
pub struct Connection<F> {
    future: F,
    control: ConnectionControl,
}

impl<F: Future> Connection<F> {
    /// Returns a local handle for graceful shutdown, deadline escalation, or abort.
    pub fn control(&self) -> ConnectionControl {
        self.control.clone()
    }

    /// Drives the connection to closure, handoff, or a settled failure.
    pub async fn run(self) -> F::Output {
        self.future.await
    }
}

impl Http1Server {
    /// Creates an HTTP/1 server using Vakya's protocol defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Configures head/body framing limits, buffering, and progress deadlines.
    ///
    /// These transport controls are independent of middleware payload limits and
    /// dispatch deadlines. Settings are copied into each constructed connection.
    pub fn protocol(&mut self) -> &mut vakya::server::conn::http1::Builder {
        &mut self.protocol
    }

    /// Constructs a plain HTTP/1 driver for independently splittable I/O.
    ///
    /// The supplied metadata must describe the actual transport. Generic I/O uses
    /// Vakya's portable read strategy; use tcp for its native TCP receive strategy.
    /// Supply a plaintext transport with an HTTP connection scheme. Client-facing
    /// HTTPS origins are established separately by trusted proxy middleware.
    #[allow(clippy::type_complexity)]
    pub fn connection<I: IntoOwnedSplit>(
        &self,
        io: I,
        info: ConnectionInfo,
        application: Rc<Application>,
    ) -> Connection<impl Future<Output = Result<ConnectionOutcome<I::ReadHalf, I::WriteHalf>, ConnectionError>> + use<I>>
    {
        let connection = self.protocol.serve_connection(io, service(application, info));
        let control = connection.control();
        Connection {
            future: connection.run(),
            control,
        }
    }

    /// Constructs a native TCP driver and reads actual socket addresses.
    ///
    /// # Errors
    /// Returns socket metadata errors before constructing the connection.
    #[allow(clippy::type_complexity)]
    pub fn tcp(
        &self,
        io: TcpStream,
        application: Rc<Application>,
    ) -> std::io::Result<
        Connection<
            impl Future<
                Output = Result<
                    ConnectionOutcome<
                        <TcpStream as IntoOwnedSplit>::ReadHalf,
                        <TcpStream as IntoOwnedSplit>::WriteHalf,
                    >,
                    ConnectionError,
                >,
            > + use<>,
        >,
    > {
        let info = ConnectionInfo {
            remote_addr: io.peer_addr()?,
            local_addr: io.local_addr()?,
            scheme: crate::http::uri::Scheme::HTTP,
        };
        let connection = self.protocol.serve_tcp(io, service(application, info));
        let control = connection.control();
        Ok(Connection {
            future: connection.run(),
            control,
        })
    }
}

fn service(
    application: Rc<Application>,
    info: ConnectionInfo,
) -> impl vakya::service::Service<
    (http::Request<vakya::body::Incoming>, vakya::server::RequestContext),
    Response = http::Response<WireBody>,
    Error = Infallible,
> {
    vakya::service::service_fn(
        async move |(request, _context): (http::Request<vakya::body::Incoming>, vakya::server::RequestContext)| {
            let head = request.method() == http::Method::HEAD;
            let (parts, incoming) = request.into_parts();
            let control = IncomingAbort::default();
            let body = Body::from_incoming(incoming, control.clone());
            let mut request = http::Request::from_parts(parts, body);
            request.extensions_mut().insert(info.clone());
            request.extensions_mut().insert(control);
            let response = application.handle(request).await;
            let representation = if head {
                response.extensions().get::<HeadRepresentation>().copied()
            } else {
                None
            };
            Ok(response.map(|body| WireBody { body, representation }))
        },
    )
}

// Vakya needs representation hints after public HEAD payload suppression.
// Its metadata-only HEAD path never polls this producer.
struct WireBody {
    body: Body,
    representation: Option<HeadRepresentation>,
}
impl HttpBody for WireBody {
    type Data = BodyData;
    type Error = ConnectionError;
    async fn next_frame(&mut self) -> Result<Option<Frame<BodyData>>, Self::Error> {
        self.body.next_frame().await
    }
    fn size_hint(&self) -> SizeHint {
        self.representation
            .map_or_else(|| self.body.size_hint(), |value| value.size)
    }
    fn trailer_hint(&self) -> TrailerHint {
        self.representation
            .map_or_else(|| self.body.trailer_hint(), |value| value.trailers)
    }
    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }
    fn recycle(&mut self, data: BodyData) {
        self.body.recycle(data);
    }
}

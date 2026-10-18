use std::rc::Rc;

use crate::{
    Error,
    dispatch::{
        DispatchFuture, ErasedController, ErasedMiddleware, Middleware, MiddlewareFactory, Next, Terminal,
        defer_middleware,
    },
    http::{
        Body, HeaderValue, Method, Request, RequestMetadata, Response, RoutingInfo, StatusCode,
        body::{HttpBody, SizeHint, TrailerHint},
        header,
        response::empty,
    },
    routing::{BuildError, RecognitionError, Router, RoutingTable},
};

/// An assembled, worker-local application with no listener or runtime ownership.
///
/// The application owns each request and lends mutable references through the
/// middleware and controller chain. Every completed call returns an owned
/// response. Business errors are handled in handlers. Framework rejections are
/// converted to responses before returning through middleware. Original HEAD
/// finalization occurs after all middleware returns.
/// Calls can overlap on a worker. The application never spawns tasks.
pub struct Application {
    router: RoutingTable,
    middleware: Vec<Box<dyn ErasedMiddleware>>,
    chain: Vec<usize>,
}

/// Binds routes and application middleware to the same typed dependency owner.
///
/// Initialize dependencies on their Karmaio worker and retain an application-owned
/// Rc for explicit resource shutdown after serving work settles.
#[must_use]
pub struct ApplicationBuilder<S = ()> {
    router: Router<S>,
    middleware: Vec<MiddlewareFactory<S>>,
}

/// Representation hints retained for encoding a payload-free HEAD response.
#[derive(Clone, Copy)]
pub(super) struct HeadRepresentation {
    pub(super) size: SizeHint,
    pub(super) trailers: TrailerHint,
}

impl Application {
    /// Begins assembly around typed route declarations.
    pub fn builder<S: 'static>(router: Router<S>) -> ApplicationBuilder<S> {
        ApplicationBuilder {
            router,
            middleware: Vec::new(),
        }
    }

    /// Returns the compiled route catalog and URL generator.
    pub fn routes(&self) -> &RoutingTable {
        &self.router
    }

    /// Owns one request through dispatch and finalizes its returned response.
    ///
    /// Application middleware also surrounds metadata-validation failures. Such
    /// failures produce an early response instead of entering routing. Middleware
    /// may inspect and modify that response normally. Panics and dropped futures
    /// are not converted to responses. Late response-body failures belong to the
    /// serving connection.
    pub async fn handle(&self, mut request: Request) -> Response {
        request.extensions_mut().remove::<RoutingInfo>();
        let head = request.method() == Method::HEAD;
        let routing = RouteDispatch(self);
        let terminal = match capture_metadata(&mut request) {
            Ok(()) => Terminal::Controller(&routing),
            Err(error) => Terminal::Response(Box::new(Error::from(error).into_response())),
        };
        let mut response = Next::new(&self.middleware, &self.chain, terminal)
            .run(&mut request)
            .await;
        if head {
            let status = response.status();
            if !status.is_informational()
                && status != StatusCode::NO_CONTENT
                && status != StatusCode::NOT_MODIFIED
                && !response.headers().contains_key(header::CONTENT_LENGTH)
                && !response.headers().contains_key(header::TRANSFER_ENCODING)
                && let Some(length) = response.body().size_hint().exact()
            {
                response
                    .headers_mut()
                    .insert(header::CONTENT_LENGTH, HeaderValue::from(length));
            }
            if !status.is_informational() && status != StatusCode::NO_CONTENT {
                let size = response
                    .headers()
                    .get(header::CONTENT_LENGTH)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse::<u64>().ok())
                    .map_or_else(|| response.body().size_hint(), SizeHint::with_exact);
                let trailers = if response.headers().contains_key(header::CONTENT_LENGTH) {
                    TrailerHint::None
                } else {
                    response.body().trailer_hint()
                };
                response.extensions_mut().insert(HeadRepresentation { size, trailers });
            }
            *response.body_mut() = Body::empty();
        }
        response
    }

    async fn dispatch(&self, request: &mut Request) -> Response {
        let (chain, terminal) = match self.router.recognize(request.method(), request.uri()) {
            Ok(selected) => {
                if let Some(metadata) = selected.metadata {
                    request.extensions_mut().insert(metadata);
                }
                (selected.chain, Terminal::Controller(selected.controller))
            }
            Err(error) => (
                self.router.error_chain(),
                Terminal::Response(Box::new(routing_response(error))),
            ),
        };
        Next::new(self.router.middleware(), chain, terminal).run(request).await
    }
}

fn routing_response(error: RecognitionError) -> Response {
    match error {
        RecognitionError::InvalidCapture(error) => Error::with_status(StatusCode::BAD_REQUEST, error).into_response(),
        RecognitionError::NotFound => empty(StatusCode::NOT_FOUND),
        RecognitionError::MethodNotAllowed(allowed) => {
            let mut response = empty(StatusCode::METHOD_NOT_ALLOWED);
            let methods = allowed.iter().map(Method::as_str).collect::<Vec<_>>().join(", ");
            response.headers_mut().insert(
                header::ALLOW,
                HeaderValue::try_from(methods).expect("HTTP methods are valid header tokens"),
            );
            response
        }
    }
}

/// Enters application-owned routing through the existing terminal call contract.
struct RouteDispatch<'a>(&'a Application);

impl ErasedController for RouteDispatch<'_> {
    fn call<'a>(&'a self, request: &'a mut Request) -> DispatchFuture<'a> {
        Box::pin(self.0.dispatch(request))
    }
}

fn capture_metadata(request: &mut Request) -> Result<(), crate::http::MetadataError> {
    if request.extensions().get::<RequestMetadata>().is_none() {
        let metadata = RequestMetadata::capture(request)?;
        request.extensions_mut().insert(metadata);
    }
    Ok(())
}

impl ApplicationBuilder<()> {
    /// Builds an application without application dependencies.
    ///
    /// # Errors
    /// Returns invalid or conflicting route declarations before exposing the application.
    pub fn build(self) -> Result<Application, BuildError> {
        self.build_with_dependencies(Rc::new(()))
    }
}

impl<S: 'static> ApplicationBuilder<S> {
    /// Appends middleware in execution order, before route selection.
    pub fn middleware(mut self, middleware: impl Middleware<S>) -> Self {
        self.middleware.push(defer_middleware(middleware));
        self
    }

    /// Compiles routes and binds every handler to the same application dependency root.
    ///
    /// # Errors
    /// Returns invalid or conflicting route declarations. No services are initialized.
    pub fn build_with_dependencies(self, dependencies: Rc<S>) -> Result<Application, BuildError> {
        let router = self.router.build_with_dependencies(dependencies.clone())?;
        let middleware: Vec<_> = self
            .middleware
            .into_iter()
            .map(|factory| factory(dependencies.clone()))
            .collect();
        Ok(Application {
            chain: (0..middleware.len()).collect(),
            router,
            middleware,
        })
    }
}

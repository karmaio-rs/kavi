//! Streaming bodies and the types needed to produce and collect them.
//!
//! [`Body`] is Kavi's canonical body. Payload [`Bytes`] comes from the `bytes`
//! crate; [`HttpBody`], [`BodyExt`], frames, collection types, and hints are
//! re-exported from Vakya. [`Error`] is the Vakya body source error, distinct
//! from the framework's [`crate::Error`].

pub use bytes::Bytes;
pub use vakya::body::{Body as HttpBody, BodyExt, CollectError, Collected, Frame, Incoming, SizeHint, TrailerHint};
pub use vakya::error::Error;

use vakya::body::{BoxBody, Empty, Full};

/// Default maximum body size for extractors (2 MiB).
///
/// Built-in body extractors collect at most this many payload bytes.
/// Use [`Body::collect`] directly to choose a different limit.
pub const DEFAULT_BODY_LIMIT: usize = 2 * 1024 * 1024;

/// Kavi's canonical type-erased streaming body.
///
/// This is the single body type that flows through the entire framework.
/// It wraps any [`vakya::body::Body`] with `Data = Bytes` behind a type erasure boundary,
/// preserving streaming semantics throughout.
///
/// Construct bodies via:
/// - [`Body::empty`] for an empty body
/// - [`Body::full`] for a buffered bytes body
/// - [`Body::from_incoming`] to wrap a Vakya [`Incoming`] body
/// - [`Body::from_stream`] for custom streaming sources
/// - [`Body::new`] to erase any compatible Vakya body
pub struct Body {
    inner: BoxBody<'static, Bytes, Error>,
}

impl Body {
    /// Creates an empty body that immediately ends the stream.
    #[inline]
    pub fn empty() -> Self {
        Self {
            inner: BoxBody::new(Empty.map_err(|e| match e {})),
        }
    }

    /// Creates a body that yields `bytes` as its single data frame.
    #[inline]
    pub fn full(bytes: Bytes) -> Self {
        Self {
            inner: BoxBody::new(Full::new(bytes).map_err(|e| match e {})),
        }
    }

    /// Wraps a Vakya [`Incoming`] body, converting each frame's data into [`Bytes`] on the fly to preserve streaming.
    ///
    /// This is the primary conversion used at the Vakya → Kavi boundary.
    /// Data frames are copied into contiguous [`Bytes`] allocations as they arrive, so the body remains a true streaming body.
    #[inline]
    pub fn from_incoming(incoming: Incoming) -> Self {
        let mapped = incoming.map_frame(|frame| frame.map_data(|data| Bytes::copy_from_slice(data.as_ref())));
        Self {
            inner: BoxBody::new(mapped),
        }
    }

    /// Creates a body from a Karmaio stream of frames.
    ///
    /// The stream must produce data frames followed by at most one trailers frame.
    /// Errors and stream exhaustion terminate the body.
    #[inline]
    pub fn from_stream<S>(stream: S) -> Self
    where
        S: karmaio::io::Stream<Item = Result<Frame<Bytes>, Error>> + 'static,
    {
        Self {
            inner: BoxBody::new(vakya::body::StreamBody::new(stream)),
        }
    }

    /// Erases any Vakya body with `Data = Bytes` into Kavi's canonical body.
    ///
    /// The body type is type-erased; payload and error types are fixed to [`Bytes`]
    /// and [`vakya::error::Error`] respectively.
    #[inline]
    pub fn new<B>(body: B) -> Self
    where
        B: HttpBody<Data = Bytes, Error = Error> + 'static,
    {
        Self {
            inner: BoxBody::new(body),
        }
    }

    /// Collects the remaining body data up to `max_bytes`.
    ///
    /// This consumes the body. Trailers are preserved in the returned [`Collected`] value.
    /// Allocation stays within the payload limit.
    ///
    /// [`Collected`] contains the contiguous bytes and optional trailers.
    #[inline]
    pub async fn collect(self, max_bytes: usize) -> Result<Collected, CollectError<Error>> {
        self.inner.collect(max_bytes).await
    }

    /// Returns bounds on the number of payload bytes still to be produced.
    #[inline]
    pub fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }

    /// Returns `true` when no further frame can be produced.
    #[inline]
    pub fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }
}

impl HttpBody for Body {
    type Data = Bytes;
    type Error = Error;

    #[inline]
    async fn next_frame(&mut self) -> Result<Option<Frame<Bytes>>, Error> {
        self.inner.next_frame().await
    }

    #[inline]
    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }

    #[inline]
    fn trailer_hint(&self) -> TrailerHint {
        self.inner.trailer_hint()
    }

    #[inline]
    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    #[inline]
    fn recycle(&mut self, data: Bytes) {
        self.inner.recycle(data);
    }
}

impl std::fmt::Debug for Body {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Body")
            .field("size_hint", &self.size_hint())
            .field("is_end_stream", &self.is_end_stream())
            .finish_non_exhaustive()
    }
}

impl From<Bytes> for Body {
    #[inline]
    fn from(bytes: Bytes) -> Self {
        Self::full(bytes)
    }
}

impl From<String> for Body {
    #[inline]
    fn from(s: String) -> Self {
        Self::full(Bytes::from(s))
    }
}

impl From<&'static str> for Body {
    #[inline]
    fn from(s: &'static str) -> Self {
        Self::full(Bytes::from_static(s.as_bytes()))
    }
}

impl From<Vec<u8>> for Body {
    #[inline]
    fn from(v: Vec<u8>) -> Self {
        Self::full(Bytes::from(v))
    }
}

impl From<Incoming> for Body {
    #[inline]
    fn from(incoming: Incoming) -> Self {
        Self::from_incoming(incoming)
    }
}

impl Default for Body {
    #[inline]
    fn default() -> Self {
        Self::empty()
    }
}

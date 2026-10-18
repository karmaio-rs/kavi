//! Streaming bodies, owned payload segments, and bounded collection.
//!
//! [`Body`] preserves native incoming storage in [`BodyData`]. Empty, buffered,
//! and incoming producers use concrete variants; custom producers are erased.
//! [`Bytes`] remains the contiguous buffer type used by collected bodies.
//! [`HttpBody`], frames, collection types, and hints come from Vakya.
//! [`Error`] is the body-source error, distinct from [`crate::Error`].

pub use bytes::Bytes;
pub use vakya::body::{Body as HttpBody, BodyExt, CollectError, Collected, Frame, SizeHint, TrailerHint};
pub use vakya::error::Error;

use std::{
    future::{Future, poll_fn},
    ops::RangeBounds,
    pin::pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
};

use karmaio::{buf::IoBuf, runtime::CancellationSource};
use vakya::body::{BoxBody, Incoming, IncomingData};

/// Default payload ceiling and buffered extraction budget (2 MiB).
///
/// These are independent policies that happen to share a default value.
/// [`crate::http::middleware::BodyLimit`] enforces streaming payload bytes;
/// [`BufferedBodyLimit`] configures collection by built-in extractors.
pub const DEFAULT_BODY_LIMIT: usize = 2 * 1024 * 1024;

/// Maximum payload bytes collected by built-in JSON, form, bytes, and text extractors.
///
/// Keep this configuration in the application's dependency root and pass it to
/// [`super::RequestExt::extract_body_with`]. Parameterless extraction uses
/// [`Self::default`]. Neither extraction nor middleware reads it from request
/// extensions. A collection budget cannot relax an enforced stream ceiling.
#[derive(Clone, Copy, Debug)]
pub struct BufferedBodyLimit(pub usize);

impl Default for BufferedBodyLimit {
    #[inline]
    fn default() -> Self {
        Self(DEFAULT_BODY_LIMIT)
    }
}

/// A payload ceiling exceeded while configuring or consuming a body stream.
#[derive(Debug, thiserror::Error)]
#[error("request payload exceeds {limit} bytes")]
pub struct BodyLimitExceeded {
    /// The maximum allowed payload size.
    pub limit: usize,
}

/// An owned body segment backed by bytes or a native receive-buffer lease.
///
/// Moving, cloning, and slicing preserve the backing storage without copying
/// payload bytes. Native data remains local to its Karmaio execution context;
/// this type is neither `Send` nor `Sync`. Retained native segments can keep a
/// receive buffer leased, making subsequent receives use Vakya's portable path.
/// Use [`Self::into_bytes`] when a contiguous `Bytes` value is required.
#[derive(Clone, Debug)]
pub struct BodyData {
    inner: DataKind,
}

#[derive(Clone, Debug)]
enum DataKind {
    Bytes(Bytes),
    Incoming(IncomingData),
}

impl BodyData {
    /// Returns the number of bytes in this segment.
    #[inline]
    pub fn len(&self) -> usize {
        self.as_ref().len()
    }

    /// Returns whether this segment contains no bytes.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.as_ref().is_empty()
    }

    /// Returns an owned view of a subrange without copying payload bytes.
    ///
    /// # Panics
    /// Panics if the range is invalid or outside this segment.
    #[inline]
    pub fn slice(&self, range: impl RangeBounds<usize>) -> Self {
        match &self.inner {
            DataKind::Bytes(bytes) => bytes.slice(range).into(),
            DataKind::Incoming(data) => data.slice(range).into(),
        }
    }

    /// Returns contiguous bytes, copying native storage when necessary.
    ///
    /// Ordinary `Bytes` storage is moved directly. Native storage is copied to
    /// detach the returned value from its local receive-buffer lease.
    #[inline]
    pub fn into_bytes(self) -> Bytes {
        match self.inner {
            DataKind::Bytes(bytes) => bytes,
            DataKind::Incoming(data) => Bytes::copy_from_slice(data.as_ref()),
        }
    }
}

impl AsRef<[u8]> for BodyData {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        match &self.inner {
            DataKind::Bytes(bytes) => bytes,
            DataKind::Incoming(data) => data.as_ref(),
        }
    }
}

impl IoBuf for BodyData {
    #[inline]
    fn as_init(&self) -> &[u8] {
        self.as_ref()
    }
}

impl bytes::Buf for BodyData {
    #[inline]
    fn remaining(&self) -> usize {
        self.len()
    }

    #[inline]
    fn chunk(&self) -> &[u8] {
        self.as_ref()
    }

    #[inline]
    fn advance(&mut self, count: usize) {
        match &mut self.inner {
            DataKind::Bytes(bytes) => bytes::Buf::advance(bytes, count),
            DataKind::Incoming(data) => bytes::Buf::advance(data, count),
        }
    }
}

impl From<Bytes> for BodyData {
    #[inline]
    fn from(bytes: Bytes) -> Self {
        Self {
            inner: DataKind::Bytes(bytes),
        }
    }
}

impl From<IncomingData> for BodyData {
    #[inline]
    fn from(data: IncomingData) -> Self {
        Self {
            inner: DataKind::Incoming(data),
        }
    }
}

impl TryFrom<BodyData> for Bytes {
    type Error = BodyData;

    /// Recovers ordinary byte storage without copying, returning native data unchanged.
    #[inline]
    fn try_from(data: BodyData) -> Result<Self, Self::Error> {
        match data.inner {
            DataKind::Bytes(bytes) => Ok(bytes),
            _ => Err(data),
        }
    }
}

impl TryFrom<BodyData> for IncomingData {
    type Error = BodyData;

    /// Recovers native storage without copying, returning ordinary byte data unchanged.
    #[inline]
    fn try_from(data: BodyData) -> Result<Self, Self::Error> {
        match data.inner {
            DataKind::Incoming(incoming) => Ok(incoming),
            _ => Err(data),
        }
    }
}

/// Kavi's owned streaming body with one cumulative payload counter.
///
/// Empty, buffered, and native incoming bodies retain concrete producers without
/// per-frame future allocation. Arbitrary producers use Vakya's local erasure.
/// Limits are tightened in place, survive extraction and forwarding, and cover
/// all reads including [`Self::collect`]. Parser settings remain separate.
pub struct Body {
    inner: BodyKind,
    consumed: u64,
    limit: Option<usize>,
    done: bool,
    // Keep dispatch scopes alive for I/O already owned by the returned producer.
    // Drop the producer before these owners; new frame reads do not inherit them.
    dispatch_scopes: Vec<CancellationSource>,
}

enum BodyKind {
    Empty,
    Full(Option<BodyData>),
    Incoming(IncomingBody),
    Custom(BoxBody<'static, BodyData, Error>),
}

impl Body {
    fn from_kind(inner: BodyKind) -> Self {
        Self {
            inner,
            consumed: 0,
            limit: None,
            done: false,
            dispatch_scopes: Vec::new(),
        }
    }

    /// Creates an empty body without allocating a producer.
    #[inline]
    pub fn empty() -> Self {
        Self::from_kind(BodyKind::Empty)
    }

    /// Creates a body yielding one byte segment without allocating a producer.
    #[inline]
    pub fn full(bytes: Bytes) -> Self {
        Self::from(BodyData::from(bytes))
    }

    /// Creates a body from a Karmaio stream of frames.
    ///
    /// Segments may be `Bytes`, `BodyData`, or native `IncomingData`. Data frames
    /// precede at most one trailers frame. Exhaustion or errors terminate the body.
    #[inline]
    pub fn from_stream<S, D>(stream: S) -> Self
    where
        S: karmaio::io::Stream<Item = Result<Frame<D>, Error>> + 'static,
        D: IoBuf + Into<BodyData> + TryFrom<BodyData>,
    {
        Self::new(vakya::body::StreamBody::new(stream))
    }

    /// Erases a custom Vakya producer without copying its payload segments.
    ///
    /// `Bytes`, `BodyData`, and `IncomingData` support conversion and recovery.
    /// Recycled segments are converted back to the producer's original data type.
    /// Custom producers and their frame-read futures are boxed; neither needs
    /// `Send` or `Sync`.
    #[inline]
    pub fn new<B>(body: B) -> Self
    where
        B: HttpBody<Error = Error> + 'static,
        B::Data: Into<BodyData> + TryFrom<BodyData>,
    {
        Self::from_kind(BodyKind::Custom(BoxBody::new(CustomBody(body))))
    }

    pub(crate) fn from_incoming(body: Incoming, abort: IncomingAbort) -> Self {
        Self::from_kind(BodyKind::Incoming(IncomingBody {
            body: Some(body),
            abort,
        }))
    }

    pub(crate) fn retain_dispatch_scope(&mut self, source: CancellationSource) {
        self.dispatch_scopes.push(source);
    }

    /// Tightens the total payload ceiling without replacing the producer.
    ///
    /// The smallest installed ceiling remains effective. Bytes consumed before
    /// this call count toward it. Extraction moves this enforcement with the body;
    /// replacing the body with a new producer starts a separate stream.
    ///
    /// # Errors
    /// Returns [`BodyLimitExceeded`] if consumed bytes or the remaining lower size
    /// bound exceed the ceiling. Failure terminates and releases the producer.
    pub fn limit(&mut self, max_bytes: usize) -> Result<(), BodyLimitExceeded> {
        let limit = self.limit.map_or(max_bytes, |current| current.min(max_bytes));
        self.limit = Some(limit);
        if self.consumed > limit as u64 || self.size_hint().lower() > (limit as u64).saturating_sub(self.consumed) {
            self.terminate();
            return Err(BodyLimitExceeded { limit });
        }
        Ok(())
    }

    /// Collects remaining payload within both the installed and collection limits.
    ///
    /// This consumes the body. Contiguous output is allocated only by collection;
    /// trailers are retained in [`Collected`]. Installed payload ceilings count
    /// earlier reads too, while `max_bytes` covers only this collection.
    ///
    /// # Errors
    /// Returns a producer, payload-limit, collection-limit, framing, or allocation error.
    #[inline]
    pub async fn collect(mut self, max_bytes: usize) -> Result<Collected, CollectError<Error>> {
        vakya::body::collect(&mut self, max_bytes).await
    }

    /// Returns bounds on remaining payload bytes, excluding trailers.
    #[inline]
    pub fn size_hint(&self) -> SizeHint {
        if self.done {
            return SizeHint::with_exact(0);
        }
        match &self.inner {
            BodyKind::Empty | BodyKind::Full(None) => SizeHint::with_exact(0),
            BodyKind::Full(Some(bytes)) => SizeHint::with_exact(bytes.len() as u64),
            BodyKind::Incoming(body) => body.size_hint(),
            BodyKind::Custom(body) => body.size_hint(),
        }
    }

    /// Returns true only when no further data or trailers can be produced.
    #[inline]
    pub fn is_end_stream(&self) -> bool {
        self.done
            || match &self.inner {
                BodyKind::Empty | BodyKind::Full(None) => true,
                BodyKind::Full(Some(_)) => false,
                BodyKind::Incoming(body) => body.is_end_stream(),
                BodyKind::Custom(body) => body.is_end_stream(),
            }
    }

    fn terminate(&mut self) {
        self.done = true;
        self.inner = BodyKind::Empty;
    }
}

impl HttpBody for Body {
    type Data = BodyData;
    type Error = Error;

    async fn next_frame(&mut self) -> Result<Option<Frame<BodyData>>, Error> {
        if self.done {
            return Ok(None);
        }
        let result = match &mut self.inner {
            BodyKind::Empty => Ok(None),
            BodyKind::Full(data) => Ok(data.take().map(Frame::data)),
            BodyKind::Incoming(body) => body.next_frame().await,
            BodyKind::Custom(body) => body.next_frame().await,
        };
        let frame = match result {
            Ok(Some(frame)) => frame,
            Ok(None) => {
                self.done = true;
                return Ok(None);
            }
            Err(error) => {
                self.terminate();
                return Err(error);
            }
        };
        if let Some(data) = frame.data_ref() {
            match self
                .consumed
                .checked_add(data.len() as u64)
                .filter(|total| self.limit.is_none_or(|limit| *total <= limit as u64))
            {
                Some(total) => self.consumed = total,
                None => {
                    if let Ok(data) = frame.into_data() {
                        self.recycle(data);
                    }
                    let error = match self.limit {
                        Some(limit) => std::io::Error::other(BodyLimitExceeded { limit }).into(),
                        None => std::io::Error::other("body payload counter overflowed").into(),
                    };
                    self.terminate();
                    return Err(error);
                }
            }
        }
        Ok(Some(frame))
    }

    #[inline]
    fn size_hint(&self) -> SizeHint {
        self.size_hint()
    }

    #[inline]
    fn trailer_hint(&self) -> TrailerHint {
        if self.done {
            return TrailerHint::None;
        }
        match &self.inner {
            BodyKind::Empty | BodyKind::Full(_) => TrailerHint::None,
            BodyKind::Incoming(body) => body.trailer_hint(),
            BodyKind::Custom(body) => body.trailer_hint(),
        }
    }

    #[inline]
    fn is_end_stream(&self) -> bool {
        self.is_end_stream()
    }

    #[inline]
    fn recycle(&mut self, data: BodyData) {
        if let BodyKind::Custom(body) = &mut self.inner {
            body.recycle(data);
        }
    }
}

impl std::fmt::Debug for Body {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Body")
            .field("size_hint", &self.size_hint())
            .field("is_end_stream", &self.is_end_stream())
            .field("consumed", &self.consumed)
            .field("limit", &self.limit)
            .finish_non_exhaustive()
    }
}

impl From<BodyData> for Body {
    /// Creates a single-segment body without copying or erasing native storage.
    #[inline]
    fn from(data: BodyData) -> Self {
        Self::from_kind(BodyKind::Full(Some(data)))
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
    fn from(value: String) -> Self {
        Self::full(Bytes::from(value))
    }
}
impl From<&'static str> for Body {
    #[inline]
    fn from(value: &'static str) -> Self {
        Self::full(Bytes::from_static(value.as_bytes()))
    }
}
impl From<Vec<u8>> for Body {
    #[inline]
    fn from(value: Vec<u8>) -> Self {
        Self::full(Bytes::from(value))
    }
}
impl Default for Body {
    #[inline]
    fn default() -> Self {
        Self::empty()
    }
}

// Preserve the producer's recycling contract across the segment representation.
struct CustomBody<B>(B);
impl<B> HttpBody for CustomBody<B>
where
    B: HttpBody<Error = Error>,
    B::Data: Into<BodyData> + TryFrom<BodyData>,
{
    type Data = BodyData;
    type Error = Error;

    async fn next_frame(&mut self) -> Result<Option<Frame<BodyData>>, Error> {
        self.0
            .next_frame()
            .await
            .map(|frame| frame.map(|frame| frame.map_data(Into::into)))
    }
    #[inline]
    fn size_hint(&self) -> SizeHint {
        self.0.size_hint()
    }
    #[inline]
    fn trailer_hint(&self) -> TrailerHint {
        self.0.trailer_hint()
    }
    #[inline]
    fn is_end_stream(&self) -> bool {
        self.0.is_end_stream()
    }
    #[inline]
    fn recycle(&mut self, data: BodyData) {
        if let Ok(data) = B::Data::try_from(data) {
            self.0.recycle(data);
        }
    }
}

struct IncomingBody {
    body: Option<Incoming>,
    abort: IncomingAbort,
}

impl HttpBody for IncomingBody {
    type Data = BodyData;
    type Error = Error;

    async fn next_frame(&mut self) -> Result<Option<Frame<BodyData>>, Error> {
        let Some(body) = self.body.as_mut() else {
            return Ok(None);
        };
        let result = {
            let mut cancelled = pin!(self.abort.cancelled());
            // This is a passive channel receive. Vakya's connection retains all
            // submitted transport I/O and settles it when the body is abandoned.
            let mut read = pin!(body.next_frame());
            poll_fn(|cx| {
                if cancelled.as_mut().poll(cx).is_ready() {
                    return Poll::Ready(None);
                }
                read.as_mut().poll(cx).map(Some)
            })
            .await
        };
        match result {
            Some(Ok(Some(frame))) => Ok(Some(frame.map_data(BodyData::from))),
            Some(result) => {
                self.body = None;
                result.map(|_| None)
            }
            None => {
                self.body = None;
                Err(karmaio::runtime::operation_canceled().into())
            }
        }
    }
    fn size_hint(&self) -> SizeHint {
        self.body.as_ref().map_or(SizeHint::with_exact(0), HttpBody::size_hint)
    }
    fn trailer_hint(&self) -> TrailerHint {
        self.body.as_ref().map_or(TrailerHint::None, HttpBody::trailer_hint)
    }
    fn is_end_stream(&self) -> bool {
        self.body.as_ref().is_none_or(HttpBody::is_end_stream)
    }
}

/// Wakes and abandons a single native incoming stream without owning socket I/O.
///
/// The server installs this private capability in the request's extensions.
/// It contains only a cancellation flag and waiter, so it meets Extensions' thread
/// bounds without moving Karmaio-local tokens or I/O across threads. Dropping the
/// handle does not cancel a returned response stream. No application services or
/// request observations are stored here.
#[derive(Clone, Default)]
pub(crate) struct IncomingAbort {
    inner: Arc<Mutex<Signal>>,
}

#[derive(Default)]
struct Signal {
    cancelled: bool,
    waiter: Option<Waker>,
}

impl IncomingAbort {
    pub(crate) fn cancel(&self) {
        let waiter = {
            let mut signal = self.inner.lock().expect("incoming cancellation signal poisoned");
            signal.cancelled = true;
            signal.waiter.take()
        };
        if let Some(waiter) = waiter {
            waiter.wake();
        }
    }

    /// Waits for cancellation; the body adapter is the sole waiter.
    pub(crate) async fn cancelled(&self) {
        let _waiter = Waiter(self);
        poll_fn(|cx| self.poll_cancelled(cx)).await
    }

    fn poll_cancelled(&self, cx: &Context<'_>) -> Poll<()> {
        let waiter = cx.waker().clone();
        let previous = {
            let mut signal = self.inner.lock().expect("incoming cancellation signal poisoned");
            if signal.cancelled {
                return Poll::Ready(());
            }
            if signal.waiter.as_ref().is_some_and(|current| current.will_wake(&waiter)) {
                return Poll::Pending;
            }
            signal.waiter.replace(waiter)
        };
        drop(previous);
        Poll::Pending
    }
}

/// Each stream has one exclusive next_frame borrower and therefore one waiter.
struct Waiter<'a>(&'a IncomingAbort);
impl Drop for Waiter<'_> {
    fn drop(&mut self) {
        let waiter = self
            .0
            .inner
            .lock()
            .expect("incoming cancellation signal poisoned")
            .waiter
            .take();
        drop(waiter);
    }
}

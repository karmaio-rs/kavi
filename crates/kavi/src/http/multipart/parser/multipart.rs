use std::task::Poll;

use crate::support::future::WorkBudget;

use super::{
    Error, Result,
    buffer::{BoundaryMatch, StreamBuffer},
    constants,
    constraints::Constraints,
    content_disposition::ContentDisposition,
    field::Field,
    helpers,
};

/// Parser using an owned completion-native body and exclusive field borrows.
pub struct Multipart {
    state: MultipartState,
}

pub(super) struct MultipartState {
    pub(super) budget: WorkBudget,
    pub(super) buffer: StreamBuffer,
    pub(super) boundary_bytes: Vec<u8>,
    pub(super) field_boundary_bytes: Vec<u8>,
    pub(super) stage: StreamingStage,
    pub(super) next_field_idx: usize,
    pub(super) curr_field_name: Option<String>,
    pub(super) curr_field_size_limit: u64,
    pub(super) curr_field_size_counter: u64,
    pub(super) constraints: Constraints,
}

struct FieldHead {
    headers: http::HeaderMap,
    idx: usize,
    content_disposition: ContentDisposition,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum StreamingStage {
    FindingFirstBoundary,
    ReadingBoundary,
    DeterminingBoundaryType,
    ReadingTransportPadding,
    ReadingFieldHeaders,
    ReadingFieldData,
    Eof,
    Failed,
}

impl Multipart {
    /// Creates a parser with explicit limits for untrusted input.
    pub fn with_constraints(body: crate::http::Body, boundary: impl Into<String>, constraints: Constraints) -> Self {
        let boundary = boundary.into();
        let boundary_bytes = format!("{}{}", constants::BOUNDARY_EXT, boundary).into_bytes();
        let field_boundary_bytes = format!("{}{}{}", constants::CRLF, constants::BOUNDARY_EXT, boundary).into_bytes();
        Self {
            state: MultipartState {
                budget: WorkBudget::new(),
                buffer: StreamBuffer::new(body),
                boundary_bytes,
                field_boundary_bytes,
                stage: StreamingStage::FindingFirstBoundary,
                next_field_idx: 0,
                curr_field_name: None,
                curr_field_size_limit: constraints.size_limit.per_field,
                curr_field_size_counter: 0,
                constraints,
            },
        }
    }

    /// Returns the next field. A returned field borrows this parser exclusively.
    pub async fn next_field(&mut self) -> Result<Option<Field<'_>>> {
        loop {
            self.state.budget.step().await;
            let before = self.state.progress();
            match self.state.parse_next_field() {
                Poll::Ready(result) => {
                    if result.is_err() {
                        self.state.stage = StreamingStage::Failed;
                    }
                    return result.map(|field| {
                        field.map(|head| Field::new(&mut self.state, head.headers, head.idx, head.content_disposition))
                    });
                }
                Poll::Pending if self.state.progress() != before => continue,
                Poll::Pending => {
                    if let Err(error) = self.state.buffer.read_more().await {
                        self.state.stage = StreamingStage::Failed;
                        return Err(error);
                    }
                }
            }
        }
    }
}

impl MultipartState {
    fn progress(&self) -> (usize, StreamingStage, u64) {
        (self.buffer.buf.len(), self.stage, self.curr_field_size_counter)
    }

    // Multra's parsing transitions are retained. Poll here describes whether
    // the existing buffer has enough bytes; the source itself is awaited above.
    fn parse_next_field(&mut self) -> Poll<Result<Option<FieldHead>>> {
        let state = self;
        if state.stage == StreamingStage::Failed {
            return Poll::Ready(Err(Error::Terminated));
        }
        if state.stage == StreamingStage::Eof {
            return Poll::Ready(Ok(None));
        }

        if state.stage == StreamingStage::FindingFirstBoundary {
            let preamble_limit = state.constraints.size_limit.preamble;
            let boundary_match = state.buffer.find_boundary(&state.boundary_bytes)?;
            if let Some((preamble_len, BoundaryMatch::Valid)) = boundary_match {
                if preamble_len as u64 > preamble_limit {
                    return Poll::Ready(Err(Error::PreambleSizeExceeded { limit: preamble_limit }));
                }
                state.buffer.advance(preamble_len);
                state.stage = StreamingStage::ReadingBoundary;
            } else {
                // The independent preamble limit prevents unbounded buffer
                // growth when the whole-stream limit is left unbounded. A
                // trailing partial boundary is not preamble until more input
                // proves that it is not the opening delimiter.
                let preamble_len = match boundary_match {
                    Some((idx, BoundaryMatch::Partial)) => idx,
                    _ => {
                        let partial_boundary_len = state.buffer.partial_pattern_suffix_len(&state.boundary_bytes);
                        state.buffer.buf.len().saturating_sub(partial_boundary_len)
                    }
                };
                if preamble_len as u64 > preamble_limit {
                    return Poll::Ready(Err(Error::PreambleSizeExceeded { limit: preamble_limit }));
                }
                if state.buffer.eof {
                    return Poll::Ready(Err(Error::IncompleteStream));
                }
            }
        }

        // The previous field did not finish reading its data.
        if state.stage == StreamingStage::ReadingFieldData {
            match state
                .buffer
                .read_field_data(&state.field_boundary_bytes, state.curr_field_name.as_deref())?
            {
                Some((done, bytes)) => {
                    // Saturating add prevents an attacker from overflowing
                    // the counter back to zero to bypass the size check.
                    state.curr_field_size_counter = state.curr_field_size_counter.saturating_add(bytes.len() as u64);

                    if state.curr_field_size_counter > state.curr_field_size_limit {
                        return Poll::Ready(Err(Error::FieldSizeExceeded {
                            limit: state.curr_field_size_limit,
                            field_name: state.curr_field_name.clone(),
                        }));
                    }

                    if done {
                        state.stage = StreamingStage::ReadingBoundary;
                    } else {
                        return Poll::Pending;
                    }
                }
                None => {
                    return Poll::Pending;
                }
            }
        }

        if state.stage == StreamingStage::ReadingBoundary {
            let Some(boundary_bytes) = state.buffer.read_exact(state.boundary_bytes.len()) else {
                return if state.buffer.eof {
                    Poll::Ready(Err(Error::IncompleteStream))
                } else {
                    Poll::Pending
                };
            };

            if &boundary_bytes[..] == state.boundary_bytes.as_slice() {
                state.stage = StreamingStage::DeterminingBoundaryType;
            } else {
                return Poll::Ready(Err(Error::IncompleteStream));
            }
        }

        if state.stage == StreamingStage::DeterminingBoundaryType {
            let ext_len = constants::BOUNDARY_EXT.len();
            let Some(next_bytes) = state.buffer.peek_exact(ext_len) else {
                return if state.buffer.eof {
                    Poll::Ready(Err(Error::IncompleteStream))
                } else {
                    Poll::Pending
                };
            };

            if next_bytes == constants::BOUNDARY_EXT.as_bytes() {
                state.stage = StreamingStage::Eof;
                return Poll::Ready(Ok(None));
            }
            state.stage = StreamingStage::ReadingTransportPadding;
        }

        if state.stage == StreamingStage::ReadingTransportPadding {
            if !state.buffer.advance_past_transport_padding() {
                return if state.buffer.eof {
                    Poll::Ready(Err(Error::IncompleteStream))
                } else {
                    Poll::Pending
                };
            }

            let crlf_len = constants::CRLF.len();
            let Some(crlf_bytes) = state.buffer.read_exact(crlf_len) else {
                return if state.buffer.eof {
                    Poll::Ready(Err(Error::IncompleteStream))
                } else {
                    Poll::Pending
                };
            };

            if &crlf_bytes[..] == constants::CRLF.as_bytes() {
                state.stage = StreamingStage::ReadingFieldHeaders;
            } else {
                return Poll::Ready(Err(Error::IncompleteStream));
            }
        }

        if state.stage == StreamingStage::ReadingFieldHeaders {
            let headers_limit = state.constraints.size_limit.headers;
            let Some(header_bytes) = state.buffer.read_until(constants::CRLF_CRLF.as_bytes()) else {
                if state.buffer.buf.len() as u64 > headers_limit {
                    return Poll::Ready(Err(Error::HeadersSizeExceeded { limit: headers_limit }));
                }

                return if state.buffer.eof {
                    Poll::Ready(Err(Error::IncompleteStream))
                } else {
                    Poll::Pending
                };
            };

            if header_bytes.len() as u64 > headers_limit {
                return Poll::Ready(Err(Error::HeadersSizeExceeded { limit: headers_limit }));
            }

            let mut headers = [httparse::EMPTY_HEADER; constants::MAX_HEADERS];

            let headers = match httparse::parse_headers(&header_bytes, &mut headers).map_err(Error::ReadHeaderFailed)? {
                httparse::Status::Complete((_, raw_headers)) => {
                    match helpers::convert_raw_headers_to_header_map(raw_headers) {
                        Ok(headers) => headers,
                        Err(err) => {
                            return Poll::Ready(Err(err));
                        }
                    }
                }
                httparse::Status::Partial => {
                    return Poll::Ready(Err(Error::IncompleteHeaders));
                }
            };

            state.stage = StreamingStage::ReadingFieldData;

            let field_idx = state.next_field_idx;
            if field_idx >= state.constraints.field_count {
                return Poll::Ready(Err(Error::FieldCountExceeded {
                    limit: state.constraints.field_count,
                }));
            }
            state.next_field_idx += 1;

            let content_disposition = ContentDisposition::parse(&headers);
            let field_size_limit = state
                .constraints
                .size_limit
                .extract_size_limit_for(content_disposition.field_name.as_deref());

            state.curr_field_name.clone_from(&content_disposition.field_name);
            state.curr_field_size_limit = field_size_limit;
            state.curr_field_size_counter = 0;

            let field_name = content_disposition.field_name.as_deref();
            if !state.constraints.is_it_allowed(field_name) {
                return Poll::Ready(Err(Error::UnknownField {
                    field_name: field_name.map(str::to_owned),
                }));
            }

            return Poll::Ready(Ok(Some(FieldHead {
                headers,
                idx: field_idx,
                content_disposition,
            })));
        }

        Poll::Pending
    }
}

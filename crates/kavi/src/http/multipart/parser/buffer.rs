use crate::support::future::WorkBudget;
use bytes::{Buf, Bytes, BytesMut};
use vakya::body::Body as _;

use super::{Error, Result, constants};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BoundaryMatch {
    Valid,
    Partial,
    Invalid,
}

fn partial_boundary_suffix_len(buf: &[u8], boundary: &[u8]) -> usize {
    let max_len = buf.len().min(boundary.len().saturating_sub(1));

    (1..=max_len)
        .rev()
        .find(|len| buf[buf.len() - len..] == boundary[..*len])
        .unwrap_or(0)
}

fn match_padding_and_crlf(bytes: &[u8], eof: bool, allow_eof: bool, checked: &mut usize) -> Result<BoundaryMatch> {
    // The prefix already scanned stays in the buffer until this candidate is
    // resolved. Scan only newly arrived padding, with an independent size cap.
    while matches!(bytes.get(*checked), Some(b' ' | b'\t')) {
        *checked += 1;
        if *checked > constants::MAX_BOUNDARY_PADDING {
            return Err(Error::BoundaryPaddingSizeExceeded {
                limit: constants::MAX_BOUNDARY_PADDING,
            });
        }
    }

    let bytes = &bytes[*checked..];
    if bytes.is_empty() {
        return Ok(if eof && allow_eof {
            BoundaryMatch::Valid
        } else if eof {
            BoundaryMatch::Invalid
        } else {
            BoundaryMatch::Partial
        });
    }

    Ok(match bytes {
        [b'\r', b'\n', ..] => BoundaryMatch::Valid,
        [b'\r'] if !eof => BoundaryMatch::Partial,
        _ => BoundaryMatch::Invalid,
    })
}

fn match_boundary_suffix(buf: &[u8], suffix_start: usize, eof: bool, checked: &mut usize) -> Result<BoundaryMatch> {
    let suffix = &buf[suffix_start..];
    match suffix {
        [] | [b'-'] if !eof => Ok(BoundaryMatch::Partial),
        [b'-', b'-', rest @ ..] => match_padding_and_crlf(rest, eof, true, checked),
        [] | [b'-', ..] => Ok(BoundaryMatch::Invalid),
        _ => match_padding_and_crlf(suffix, eof, false, checked),
    }
}

pub struct StreamBuffer {
    pub(crate) eof: bool,
    pub(crate) buf: BytesMut,
    body: crate::http::Body,
    budget: WorkBudget,
    pending_boundary: Option<(usize, usize)>,
}

impl StreamBuffer {
    pub fn new(body: crate::http::Body) -> Self {
        StreamBuffer {
            eof: false,
            buf: BytesMut::new(),
            body,
            budget: WorkBudget::new(),
            pending_boundary: None,
        }
    }

    pub async fn read_more(&mut self) -> Result<()> {
        if self.eof {
            return Ok(());
        }

        loop {
            self.budget.step().await;
            match self.body.next_frame().await {
                Ok(Some(frame)) => {
                    let Ok(data) = frame.into_data() else {
                        continue;
                    };
                    let empty = data.is_empty();
                    self.buf.extend_from_slice(data.as_ref());
                    self.body.recycle(data);
                    if empty {
                        continue;
                    }
                    return Ok(());
                }
                Err(error) => return Err(Error::StreamReadFailed(Box::new(error))),
                Ok(None) => {
                    self.eof = true;
                    return Ok(());
                }
            }
        }
    }

    pub fn read_exact(&mut self, size: usize) -> Option<Bytes> {
        if size <= self.buf.len() {
            self.pending_boundary = None;
            Some(self.buf.split_to(size).freeze())
        } else {
            None
        }
    }

    pub fn peek_exact(&self, size: usize) -> Option<&[u8]> {
        self.buf.get(..size)
    }

    pub fn read_until(&mut self, pattern: &[u8]) -> Option<Bytes> {
        self.pending_boundary = None;
        memchr::memmem::find(&self.buf, pattern).map(|idx| self.buf.split_to(idx + pattern.len()).freeze())
    }

    pub(super) fn find_boundary(&mut self, boundary: &[u8]) -> Result<Option<(usize, BoundaryMatch)>> {
        let (mut search_from, mut checked) = self.pending_boundary.take().unwrap_or((0, 0));
        while let Some(relative_idx) = memchr::memmem::find(&self.buf[search_from..], boundary) {
            let idx = search_from + relative_idx;
            let boundary_match = match_boundary_suffix(&self.buf, idx + boundary.len(), self.eof, &mut checked)?;
            if boundary_match != BoundaryMatch::Invalid {
                if boundary_match == BoundaryMatch::Partial {
                    self.pending_boundary = Some((idx, checked));
                }
                return Ok(Some((idx, boundary_match)));
            }
            search_from = idx + 1;
            checked = 0;
        }
        Ok(None)
    }

    pub fn partial_pattern_suffix_len(&self, pattern: &[u8]) -> usize {
        partial_boundary_suffix_len(&self.buf, pattern)
    }

    pub fn advance(&mut self, size: usize) {
        self.pending_boundary = None;
        self.buf.advance(size);
    }

    pub fn advance_past_transport_padding(&mut self) -> bool {
        self.pending_boundary = None;
        if let Some(pos) = self.buf.iter().position(|b| *b != b' ' && *b != b'\t') {
            self.buf.advance(pos);
            true
        } else {
            self.buf.clear();
            false
        }
    }

    pub fn read_field_data(&mut self, boundary: &[u8], field_name: Option<&str>) -> Result<Option<(bool, Bytes)>> {
        if self.buf.is_empty() && self.eof {
            return Err(Error::IncompleteFieldData {
                field_name: field_name.map(ToOwned::to_owned),
            });
        } else if self.buf.is_empty() {
            return Ok(None);
        }

        // A boundary-shaped sequence with an invalid suffix is field data.
        // Keep searching so a later, valid delimiter in this buffer is found.
        if let Some((idx, boundary_match)) = self.find_boundary(boundary)? {
            match boundary_match {
                BoundaryMatch::Valid => {
                    let bytes = self.buf.split_to(idx).freeze();
                    self.pending_boundary = None;

                    // discard \r\n.
                    self.buf.advance(constants::CRLF.len());

                    return Ok(Some((true, bytes)));
                }
                BoundaryMatch::Partial => {
                    if idx == 0 {
                        return Ok(None);
                    }

                    self.pending_boundary = None;
                    return Ok(Some((false, self.buf.split_to(idx).freeze())));
                }
                BoundaryMatch::Invalid => unreachable!("invalid boundary candidates are skipped"),
            }
        }

        if self.eof {
            return Err(Error::IncompleteFieldData {
                field_name: field_name.map(ToOwned::to_owned),
            });
        }

        let preserve_len = partial_boundary_suffix_len(&self.buf, boundary);
        let readable_len = self.buf.len().saturating_sub(preserve_len);

        if readable_len == 0 {
            Ok(None)
        } else {
            Ok(Some((false, self.buf.split_to(readable_len).freeze())))
        }
    }
}

impl std::fmt::Debug for StreamBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamBuffer").finish()
    }
}

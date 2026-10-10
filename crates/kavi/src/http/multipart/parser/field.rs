use crate::support::future::WorkBudget;
use bytes::{Bytes, BytesMut};

use super::{
    Error, Result,
    content_disposition::ContentDisposition,
    helpers,
    multipart::{MultipartState, StreamingStage},
};

/// A single field that borrows its parser until it is dropped.
pub struct Field<'a> {
    state: &'a mut MultipartState,
    done: bool,
    headers: http::HeaderMap,
    content_disposition: ContentDisposition,
    content_type: Option<mime::Mime>,
    idx: usize,
}

impl<'a> Field<'a> {
    pub(super) fn new(
        state: &'a mut MultipartState,
        headers: http::HeaderMap,
        idx: usize,
        content_disposition: ContentDisposition,
    ) -> Self {
        let content_type = helpers::parse_content_type(&headers);
        Self {
            state,
            done: false,
            headers,
            content_disposition,
            content_type,
            idx,
        }
    }

    /// Returns the declared field name.
    pub fn name(&self) -> Option<&str> {
        self.content_disposition.field_name.as_deref()
    }

    /// Returns the untrusted original filename.
    pub fn file_name(&self) -> Option<&str> {
        self.content_disposition.file_name.as_deref()
    }

    /// Returns the declared field content type.
    pub fn content_type(&self) -> Option<&mime::Mime> {
        self.content_type.as_ref()
    }

    /// Returns field headers.
    pub fn headers(&self) -> &http::HeaderMap {
        &self.headers
    }

    /// Returns the field's index in the multipart stream.
    pub fn index(&self) -> usize {
        self.idx
    }

    /// Reads the next data chunk without collecting the entire field.
    pub async fn chunk(&mut self) -> Result<Option<Bytes>> {
        if self.state.stage == StreamingStage::Failed {
            return Err(Error::Terminated);
        }
        let result = self.read_chunk().await;
        if result.is_err() {
            self.state.stage = StreamingStage::Failed;
        }
        result
    }

    async fn read_chunk(&mut self) -> Result<Option<Bytes>> {
        if self.done {
            return Ok(None);
        }
        loop {
            self.state.budget.step().await;
            match self
                .state
                .buffer
                .read_field_data(&self.state.field_boundary_bytes, self.state.curr_field_name.as_deref())?
            {
                Some((done, bytes)) => {
                    self.state.curr_field_size_counter =
                        self.state.curr_field_size_counter.saturating_add(bytes.len() as u64);
                    if self.state.curr_field_size_counter > self.state.curr_field_size_limit {
                        return Err(Error::FieldSizeExceeded {
                            limit: self.state.curr_field_size_limit,
                            field_name: self.state.curr_field_name.clone(),
                        });
                    }
                    if done {
                        self.state.stage = StreamingStage::ReadingBoundary;
                        self.done = true;
                    }
                    return Ok(Some(bytes));
                }
                None => self.state.buffer.read_more().await?,
            }
        }
    }

    /// Collects the field's remaining bytes.
    pub async fn bytes(mut self) -> Result<Bytes> {
        let mut buf = BytesMut::new();
        let mut budget = WorkBudget::new();
        loop {
            budget.step().await;
            let Some(bytes) = self.chunk().await? else {
                break;
            };
            buf.extend_from_slice(&bytes);
        }
        Ok(buf.freeze())
    }
}

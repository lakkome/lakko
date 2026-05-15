// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Tiny shim for `tokio_util::io::ReaderStream` so we don't pull
//! the whole `tokio-util` crate. `tokio` does not export this on
//! its own, so we hand-roll a minimal
//! `Stream<Item = Result<Bytes, io::Error>>` over an `AsyncRead`.
//! 8 KB chunks balance syscall count and memory.
//!
//! Lifted out of `blob.rs` so the per-account backup endpoint
//! (Phase 2a1b — see [`crate::backup`]) can share the same
//! streaming primitive.

use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use futures::Stream;
use tokio::io::{AsyncRead, ReadBuf};

const CHUNK: usize = 8 * 1024;

pub(crate) struct ReaderStream<R> {
    reader: R,
    buf: Vec<u8>,
}

impl<R: AsyncRead + Unpin> ReaderStream<R> {
    pub(crate) fn new(reader: R) -> Self {
        Self {
            reader,
            buf: vec![0u8; CHUNK],
        }
    }
}

impl<R: AsyncRead + Unpin> Stream for ReaderStream<R> {
    type Item = std::io::Result<Bytes>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let mut read_buf = ReadBuf::new(&mut this.buf);
        match Pin::new(&mut this.reader).poll_read(cx, &mut read_buf) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(e)) => Poll::Ready(Some(Err(e))),
            Poll::Ready(Ok(())) => {
                let filled = read_buf.filled();
                if filled.is_empty() {
                    Poll::Ready(None)
                } else {
                    Poll::Ready(Some(Ok(Bytes::copy_from_slice(filled))))
                }
            },
        }
    }
}

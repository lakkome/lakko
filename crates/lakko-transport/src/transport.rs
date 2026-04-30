// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Core traits: [`Transport`], [`Listener`], [`Connection`].
//!
//! Lakko transports are message-oriented (length-delimited frames), not
//! byte-streams. A single [`Connection`] carries one logical channel
//! between two peers. Multiplexing into multiple channels is the next
//! layer's responsibility.

use async_trait::async_trait;
use bytes::Bytes;

use crate::endpoint::Endpoint;
use crate::error::Result;

/// Maximum frame size enforced by every transport (16 MiB).
///
/// Application protocols above this layer must chunk if they need more.
/// The limit guards against pathological allocations on the receive path.
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// A type that can open outbound connections to remote endpoints.
#[async_trait]
pub trait Transport: Send + Sync + 'static {
    /// Open a fresh connection to `endpoint`.
    async fn connect(&self, endpoint: &Endpoint) -> Result<Box<dyn Connection>>;
}

/// A type that accepts inbound connections.
#[async_trait]
pub trait Listener: Send + 'static {
    /// Wait for the next inbound connection.
    async fn accept(&mut self) -> Result<Box<dyn Connection>>;

    /// Local address the listener is bound to (for tests / observability).
    fn local_endpoint(&self) -> Endpoint;
}

/// A bidirectional message channel between two peers.
///
/// Implementations enforce [`MAX_FRAME_BYTES`] on both directions.
#[async_trait]
pub trait Connection: Send + 'static {
    /// Send a single frame to the peer.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::FrameTooLarge`] if `frame.len() > MAX_FRAME_BYTES`.
    /// Returns [`crate::Error::Io`] / [`crate::Error::PeerClosed`] / [`crate::Error::LocalClosed`]
    /// for transport-level failures.
    async fn send(&mut self, frame: Bytes) -> Result<()>;

    /// Receive the next frame from the peer.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::PeerClosed`] when the peer cleanly closed,
    /// [`crate::Error::Io`] for transport failures, or
    /// [`crate::Error::FrameTooLarge`] if the peer sent an oversize frame.
    async fn recv(&mut self) -> Result<Bytes>;

    /// Close the connection cleanly. Idempotent.
    async fn close(&mut self) -> Result<()>;
}

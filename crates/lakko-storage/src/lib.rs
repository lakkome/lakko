// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Local encrypted storage for Lakko clients.
//!
//! Provides:
//! - SQLCipher-backed relational store for messages, contacts, settings
//! - Content-addressable blob store for attachments and call recordings
//! - Hardware-backed root key management (Secure Enclave / StrongBox / TPM 2.0)
//!
//! All keys derive from a device root key that never leaves the platform
//! secure element.

#![forbid(unsafe_code)]

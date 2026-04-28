// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Encrypted push notifications for Lakko.
//!
//! Push payloads are reduced to `(group_pseudonym, envelope_pointer)` —
//! enough for the device to wake up and fetch the encrypted envelope from the
//! Delivery Service, decrypt it locally, and materialize the user-visible
//! notification. The push provider (APNs / FCM / UnifiedPush) never sees
//! plaintext.
//!
//! UnifiedPush is mandatory for regions where Google Play Services is blocked
//! (China, Russia post-2022).

#![forbid(unsafe_code)]

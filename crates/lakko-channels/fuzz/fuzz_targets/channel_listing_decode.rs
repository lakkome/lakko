// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `lakko_channels::decode_channel_listing` (ADR-0059 §3).
//!
//! A directory listing is uploaded by a channel owner and served to anyone
//! who fetches a bucket, a search result or a card: `services/channels`
//! decodes it on upload and every client decodes what the directory serves.
//! The owner (or a directory that lies) chooses every byte, so the version
//! gate, the four length prefixes, the UTF-8 checks and the handle, name,
//! description and language rules all run on hostile input.
//!
//! Properties asserted, not merely exercised:
//!
//! 1. The decoder never panics.
//! 2. A decoded listing lies within the documented length bounds and passes
//!    `validate`.
//! 3. It re-composes to the identical bytes — the encoding is canonical.
//! 4. A stamp is never newer than itself.

#![no_main]

use lakko_channels::{
    compose_channel_listing, decode_channel_listing, listing_is_newer, CHANNEL_LISTING_MAX_LEN,
    CHANNEL_LISTING_MIN_LEN,
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(listing) = decode_channel_listing(data) else {
        return;
    };
    assert!(
        data.len() >= CHANNEL_LISTING_MIN_LEN && data.len() <= CHANNEL_LISTING_MAX_LEN,
        "a decoded listing must lie within the documented length bounds"
    );
    assert!(
        listing.validate().is_ok(),
        "a decoded listing must pass validate"
    );
    let recomposed = compose_channel_listing(&listing).expect("a decoded listing must re-compose");
    assert_eq!(recomposed, data, "the encoding must be canonical");
    assert!(!listing_is_newer(
        listing.updated_at_ms,
        listing.updated_at_ms
    ));
});

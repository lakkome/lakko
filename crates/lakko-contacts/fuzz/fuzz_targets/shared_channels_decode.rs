// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `shared_channels::decode_shared_channels` (ADR-0059 §6): the list of
//! channels a member shares rides an MLS application message on the group
//! journal, so these bytes reach the parser from the zero-trust DS, crafted
//! by any member, before any state is consulted. The count and every length
//! prefix are attacker-chosen. Asserts no panic, that anything decoded lies
//! within the documented length bounds and the entry cap, round-trip
//! stability (the encoding is canonical), and that a stamp is never newer
//! than itself.

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_contacts::shared_channels::{
    compose_shared_channels, decode_shared_channels, shared_channels_is_newer,
    SHARED_CHANNELS_ENVELOPE_MAX_LEN, SHARED_CHANNELS_ENVELOPE_MIN_LEN, SHARED_CHANNELS_MAX,
};

fuzz_target!(|data: &[u8]| {
    let Ok(list) = decode_shared_channels(data) else {
        return;
    };
    assert!(
        data.len() >= SHARED_CHANNELS_ENVELOPE_MIN_LEN
            && data.len() <= SHARED_CHANNELS_ENVELOPE_MAX_LEN,
        "a decoded envelope must lie within the documented length bounds"
    );
    assert!(list.channels.len() <= SHARED_CHANNELS_MAX);
    assert!(list.validate().is_ok());
    let recomposed = compose_shared_channels(&list).expect("a decoded list must re-compose");
    assert_eq!(
        recomposed.as_slice(),
        data,
        "the encoding must be canonical"
    );
    assert_eq!(
        decode_shared_channels(&recomposed).expect("a composed list must decode"),
        list
    );
    assert!(!shared_channels_is_newer(
        list.updated_at_ms,
        list.updated_at_ms
    ));
});

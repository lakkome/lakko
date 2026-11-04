// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `BackupPayload::decode` on arbitrary plaintext: no panic, and whatever
//! decodes keeps the received-profile bounds and re-encodes to the identical
//! bytes (the encoding is canonical).

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_backup::{
    BackupPayload, ReceivedProfile, MAX_RECEIVED_PROFILES, MAX_RECEIVED_PROFILES_BYTES,
    MAX_RECEIVED_PROFILE_LEN,
};

fuzz_target!(|data: &[u8]| {
    let Ok(payload) = BackupPayload::decode(data) else {
        return;
    };
    let list = &payload.received_profiles;
    assert!(list.len() <= MAX_RECEIVED_PROFILES);
    assert!(
        list.iter().map(ReceivedProfile::wire_len).sum::<usize>() <= MAX_RECEIVED_PROFILES_BYTES
    );
    assert!(list.windows(2).all(|w| w[0].person_key < w[1].person_key));
    assert!(list
        .iter()
        .all(|p| !p.envelope.is_empty() && p.envelope.len() <= MAX_RECEIVED_PROFILE_LEN));
    let reencoded = payload.encode().expect("a decoded payload must re-encode");
    assert_eq!(reencoded, data, "the payload encoding must be canonical");
});

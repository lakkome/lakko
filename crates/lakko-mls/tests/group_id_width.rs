// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Group-id width invariants (invariant #3 — `lakko-mls` state-machine change).
//!
//! A conversation group's MLS `group_id` doubles as the Delivery Service
//! journal routing key: clients hex-encode it verbatim into
//! `POST /ds/group/{group_id_hex}`. The DS pins the accepted width in
//! `services/ds/src/journal.rs` and rejects anything else with a 400, and no
//! client applies padding, hashing, or truncation in between.
//!
//! That coupling was previously undefended. `Group::create` delegated to
//! OpenMLS's `MlsGroup::new`, which mints a **16-byte** `GroupId::random`
//! (openmls-0.8.1 `group/mod.rs:71-75`, reached via `builder.rs:84`
//! `.unwrap_or_else(|| GroupId::random(provider.rand()))`). Two doc comments
//! in the tree asserted it was 32. Nothing tested it, so every chat publish
//! sent a 32-hex-char path the DS refused.
//!
//! These tests pin the width for every constructor, so a regression on either
//! side of the boundary fails here instead of silently 400-ing at runtime.
//!
//! `PublisherGroup` had the *same* class of break — a 41-byte id, also
//! DS-routed, via the ADR-0028 OQ3 co-publisher flow — and is fixed too, by
//! narrowing its domain prefix from 25 bytes to 16 so `prefix || channel_id`
//! lands on 32. That kept the derivation deterministic AND invertible; see
//! [`publisher_group_id_matches_the_ds_width`] and
//! [`publisher_group_id_round_trips_through_channel_id`].

use std::fmt::Write as _;

use lakko_mls::{
    channel_id_from_group_id, AccountId, ChannelId, DeviceGroup, Group, Identity, PublisherGroup,
    GROUP_ID_LEN,
};

/// Local restatement of `services/ds/src/journal.rs`:
///
/// ```text
/// const PSEUDONYM_LEN: usize = 32;
/// const HEX_LEN: usize = PSEUDONYM_LEN * 2;
/// ```
///
/// `services/ds` deliberately depends on no `lakko-*` crate — it is the
/// opaque zero-trust relay and never parses MLS (invariant #2) — so the two
/// constants cannot be tied by a compile-time assertion without pulling the
/// whole OpenMLS stack into the server. Restating it here and asserting
/// equality is the achievable guard: if either side moves, this fails.
const DS_PSEUDONYM_LEN: usize = 32;

/// The width `Group::create` mints must be exactly what the DS accepts.
///
/// This is the assertion whose absence let the 16-byte id ship.
#[test]
fn group_id_length_is_stable_across_constructors() {
    assert_eq!(
        GROUP_ID_LEN, DS_PSEUDONYM_LEN,
        "lakko-mls GROUP_ID_LEN drifted from the DS PSEUDONYM_LEN in \
         services/ds/src/journal.rs — every group publish would 400"
    );

    let identity = Identity::generate("alice").expect("identity");
    let group = Group::create(&identity).expect("create");

    assert_eq!(
        group.group_id().len(),
        DS_PSEUDONYM_LEN,
        "Group::create must mint a {DS_PSEUDONYM_LEN}-byte group id — the DS \
         hex-decodes this verbatim as the journal routing key and rejects any \
         other width with a 400. OpenMLS's own GroupId::random is 16 bytes, \
         so create() must not delegate the choice to MlsGroup::new."
    );
}

/// The hex form the client actually puts on the wire must match the DS's
/// `HEX_LEN` check. This is one step closer to the failure than a byte-length
/// assertion: the clients' `pseudonymHex` is a plain byte-to-hex encoder with
/// no padding, so `hex.len() == 2 * bytes.len()`.
#[test]
fn created_group_id_hex_matches_the_ds_path_length() {
    let identity = Identity::generate("alice").expect("identity");
    let group = Group::create(&identity).expect("create");

    // Mirrors the clients' `pseudonymHex`: a plain byte-to-hex loop with no
    // padding, so the path length is exactly twice the id length.
    let id = group.group_id();
    let hex = id
        .iter()
        .fold(String::with_capacity(id.len() * 2), |mut acc, b| {
            write!(acc, "{b:02x}").expect("writing to a String cannot fail");
            acc
        });

    assert_eq!(
        hex.len(),
        DS_PSEUDONYM_LEN * 2,
        "the DS path segment must be exactly {} lowercase hex chars",
        DS_PSEUDONYM_LEN * 2
    );
    assert!(
        hex.chars()
            .all(|c| c.is_ascii_digit() || c.is_ascii_lowercase()),
        "DS decode_pseudonym refuses uppercase hex"
    );
}

/// `create_with_id` uses the caller's bytes verbatim — it must not pad,
/// truncate, or re-derive. `PublisherGroup` depends on this to round-trip a
/// deterministic id across restarts.
#[test]
fn create_with_id_preserves_the_supplied_bytes_exactly() {
    for len in [16usize, 32, 41] {
        let identity = Identity::generate("alice").expect("identity");
        let supplied: Vec<u8> = (0..len)
            .map(|i| u8::try_from(i % 256).unwrap_or(0))
            .collect();
        let group = Group::create_with_id(&identity, &supplied).expect("create_with_id");

        assert_eq!(
            group.group_id(),
            supplied,
            "create_with_id must round-trip the caller's id verbatim at len {len}"
        );
    }
}

/// Two groups created back-to-back must not collide — the id is now minted by
/// Lakko rather than OpenMLS, so the randomness is our responsibility.
#[test]
fn created_group_ids_are_unique_and_not_all_zero() {
    let mut seen: Vec<Vec<u8>> = Vec::new();

    for _ in 0..16 {
        let identity = Identity::generate("alice").expect("identity");
        let id = Group::create(&identity).expect("create").group_id();

        assert_eq!(id.len(), GROUP_ID_LEN);
        assert_ne!(
            id,
            vec![0u8; GROUP_ID_LEN],
            "group id was all zeros — RNG not wired"
        );
        assert!(!seen.contains(&id), "two freshly created groups collided");

        seen.push(id);
    }
}

/// `DeviceGroup` derives its id as `BLAKE3("lakko-device-group/v1" ||
/// account_id)`, which is 32 bytes by construction. It is DS-routed (the
/// device journal), so it is subject to the same width requirement.
#[test]
fn device_group_id_matches_the_ds_width() {
    let account: AccountId = [3u8; 16];

    assert_eq!(
        DeviceGroup::group_id_for(&account).len(),
        DS_PSEUDONYM_LEN,
        "DeviceGroup ids are DS-routed and must match the DS width"
    );

    let identity = Identity::generate("alice").expect("identity");
    let dg = DeviceGroup::create_founder(&identity, &account).expect("device group");
    assert_eq!(dg.group_id().len(), DS_PSEUDONYM_LEN);
}

/// `PublisherGroup` ids are DS-routed and must match the DS width.
///
/// This was the *second* break of the same coupling, and it is the one that
/// actually reached a live route. The ADR-0028 OQ3 co-publisher flow passes
/// this id straight to the DS journal on all three clients — verified at
/// `apps/web/src/channels/copublisher.ts` (`ds.fetchGroup(groupId, …)` /
/// `ds.publishGroup(groupId, …)`), `apps/ios/Lakko/Channels/CoPublisher.swift`,
/// and `apps/android/.../channels/CoPublisher.kt`. Under the retired v1
/// derivation the id was 41 bytes (a 25-byte prefix plus the 16-byte channel
/// id), so it became 82 hex chars and the DS answered 400 on both the poll and
/// the publish.
///
/// The id is *deterministic* from the channel id so `PublisherGroup::load` can
/// find the persisted group across restarts, and it must stay invertible so an
/// ADR-0028 OQ3 co-publisher can recover the channel id from a `/g/`
/// capability that conveys only the `group_id`. Narrowing the domain prefix to
/// 16 bytes reaches the DS width while keeping both — see
/// `publisher_group_id_round_trips_through_channel_id`.
#[test]
fn publisher_group_id_matches_the_ds_width() {
    let identity = Identity::generate("alice").expect("identity");
    let channel = ChannelId::try_from_slice(&[9u8; 16]).expect("channel id");
    let pg = PublisherGroup::create(&identity, &channel).expect("publisher group");

    assert_eq!(
        pg.group_id().len(),
        DS_PSEUDONYM_LEN,
        "PublisherGroup ids are DS-routed by the ADR-0028 OQ3 co-publisher \
         flow on all three clients, so they must be exactly \
         {DS_PSEUDONYM_LEN} bytes. The retired v1 derivation produced 41, \
         which decode_pseudonym answers 400 on."
    );
    assert_eq!(
        pg.group_id().len(),
        GROUP_ID_LEN,
        "a PublisherGroup id must be the same width as every other Lakko \
         group id — it travels the same DS route"
    );
}

/// The width fix must not cost the inverse. A co-publisher joining from a
/// `/g/` ticket has only the `group_id`; `channel_id_from_group_id` is how it
/// recovers the channel id, so the trailing bytes must remain the channel id
/// verbatim. Deriving the id by hashing the channel id would also have hit 32
/// bytes and would have broken exactly this.
#[test]
fn publisher_group_id_round_trips_through_channel_id() {
    let identity = Identity::generate("alice").expect("identity");

    for fill in [0x00u8, 0x01, 0x5C, 0x9A, 0xFF] {
        let channel = ChannelId::try_from_slice(&[fill; 16]).expect("channel id");
        let pg = PublisherGroup::create(&identity, &channel).expect("publisher group");
        let gid = pg.group_id();

        assert_eq!(gid.len(), DS_PSEUDONYM_LEN, "width at fill {fill:#04x}");
        assert_eq!(
            channel_id_from_group_id(&gid),
            Some(channel),
            "a co-publisher must be able to recover the channel id from the \
             group id alone (fill {fill:#04x})"
        );
    }

    // A chat group id is 32 bytes too, so the length check alone no longer
    // discriminates — the prefix must. A random id decoding as a channel would
    // let a `/g/` chat ticket be mistaken for a co-publisher ticket.
    let chat = Group::create(&identity).expect("chat group");
    assert_eq!(chat.group_id().len(), DS_PSEUDONYM_LEN);
    assert_eq!(
        channel_id_from_group_id(&chat.group_id()),
        None,
        "a chat group id must not decode as a PublisherGroup id"
    );

    // Same for a DeviceGroup id, which is also 32 bytes.
    let account: AccountId = [7u8; 16];
    assert_eq!(
        channel_id_from_group_id(&DeviceGroup::group_id_for(&account)),
        None,
        "a DeviceGroup id must not decode as a PublisherGroup id"
    );
}

/// The hex form of a PublisherGroup id — what actually goes on the wire at
/// `POST /ds/group/{hex}` on the co-publisher path — must match the DS's
/// `HEX_LEN`. This is the assertion closest to the observed 400.
#[test]
fn publisher_group_id_hex_matches_the_ds_path_length() {
    let identity = Identity::generate("alice").expect("identity");
    let channel = ChannelId::try_from_slice(&[9u8; 16]).expect("channel id");
    let pg = PublisherGroup::create(&identity, &channel).expect("publisher group");

    let id = pg.group_id();
    let hex = id
        .iter()
        .fold(String::with_capacity(id.len() * 2), |mut acc, b| {
            write!(acc, "{b:02x}").expect("writing to a String cannot fail");
            acc
        });

    assert_eq!(
        hex.len(),
        DS_PSEUDONYM_LEN * 2,
        "the co-publisher DS path segment must be exactly {} lowercase hex \
         chars; 82 is what the v1 derivation produced and what the DS refused",
        DS_PSEUDONYM_LEN * 2
    );
}

// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The messages a device missed while it was out of a conversation, sealed to
//! that device alone ([ADR-0091] section 7, security review 2026-11-21).
//!
//! ## Why
//!
//! The first build sealed the gap's history bundle under
//! `Group.exportSecret("lakko/conversation-reentry/history/v1", leaf key)` at
//! the epoch the replacement created. Every member after that commit holds
//! that exporter, so every member could open the bundle: messages the
//! re-admitter held, possibly from before another member joined, reached the
//! whole group's current roster. And the start of the gap came unchecked
//! from the request, so a device could ask for history from before it was
//! ever in the conversation.
//!
//! Now the returning device mints a fresh X25519 **gap key** for each
//! request and puts its public half in the request envelope
//! ([`crate::conversation_reentry`], version 2). The re-admitter seals the
//! bundle to that key with the [`crate::envelope`] sealed box under this
//! module's own labels; only the device holding the gap secret opens it. The
//! rendezvous the bundle is posted to is derived from the gap public key,
//! not from any group secret. The re-admitter clamps the start of the gap
//! with [`gap_history_start`] before choosing the rows.
//!
//! ## Wire format (version 2)
//!
//! ```text
//! sealed     = version(1)=0x02 ‖ eph_x25519_pk(32) ‖ AEAD(plaintext) (+16 tag)
//!   plaintext = signer_pub(32) ‖ sig(64) ‖ bundle
//!   AAD      = GAP_HISTORY_AAD_DOMAIN ‖ version ‖ gap_pub(32) ‖ group_id(32)
//!   sig      = Ed25519_signer(GAP_HISTORY_SIG_DOMAIN ‖ version ‖ group_id(32)
//!                             ‖ gap_pub(32) ‖ signer_pub(32) ‖ bundle)
//! rendezvous = BLAKE3-derive-key(GAP_HISTORY_RENDEZVOUS_DOMAIN, gap_pub)
//! ```
//!
//! **The re-admitter signs, inside the encryption** (security review
//! 2026-11-21, second round): the sealed box alone is anonymous, so anyone
//! who saw the request (a sibling device, a holder of the knock seed, the
//! other person of a one-to-one) could post a bundle of invented messages
//! attributed to others. The re-admitter signs the bundle with its
//! conversation leaf signing key and seals the signer key, the signature and
//! the bundle together, so a reader of the rendezvous learns nothing about
//! who sent it. [`open_gap_history`] opens the box, verifies the signature
//! and returns the signer's leaf key with the bundle; the returning device
//! keeps the bundle only when `lakko_mls::Group::gap_history_signer_accepted`
//! says that leaf may bring it back (its own account, the owner or an admin,
//! or in a one-to-one the other person), whichever device committed the
//! replacement. Version 1 (unsigned) is refused.
//!
//! The signature covers the group, the gap key, the signer's own key and the
//! bundle, so it cannot be moved to another conversation, recipient or signer
//! key; the box's AEAD binds the same group and gap key.
//!
//! The bundle is opaque here (the clients put the prost-encoded
//! `HistoryBundle` in it). The group id in the AAD binds a bundle to the
//! conversation it was sent for: a bundle posted for one conversation never
//! opens as another's.
//!
//! [ADR-0091]: ../../../docs/adr/0091-conversation-rejoin.md

use core::fmt;
use core::result::Result as CoreResult;

use lakko_crypto::agreement::{PublicKey, SecretKey};
use lakko_crypto::hash::blake3_derive_key_from_material;
use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey, SIGNATURE_LEN};
use lakko_crypto::SecretBytes;
use thiserror::Error;

use crate::envelope::{self, SealLabels, EPH_PUB_LEN, TAG_LEN};

/// Length of a gap key (secret or public) and of a group id.
pub const GAP_KEY_LEN: usize = 32;

/// Schema version of a sealed gap history. Version 1 (unsigned) is refused.
pub const GAP_HISTORY_VERSION_V2: u8 = 0x02;

/// Domain separator the re-admitter signs a gap history under. Frozen.
pub const GAP_HISTORY_SIG_DOMAIN: &[u8] = b"lakko/conversation-reentry/gap-history-sig/v1\x00";

/// AAD domain of a sealed gap history. Frozen.
pub const GAP_HISTORY_AAD_DOMAIN: &[u8] = b"lakko/conversation-reentry/gap-history/v1\x00";

/// BLAKE3 derive-key context of the rendezvous id. Frozen.
pub const GAP_HISTORY_RENDEZVOUS_DOMAIN: &str =
    "lakko/conversation-reentry/gap-history-rendezvous/v1";

/// What a sealed gap history adds around the bundle: version, ephemeral key,
/// and inside the box the signer key and signature, and the tag.
pub const GAP_HISTORY_OVERHEAD: usize = 1 + GAP_KEY_LEN + SIGNATURE_LEN + EPH_PUB_LEN + TAG_LEN;

/// The longest sealed gap history: the Delivery Service's 1 MiB message cap.
pub const GAP_HISTORY_MAX_SEALED_LEN: usize = 1024 * 1024;

/// The longest bundle a sealed gap history carries.
pub const GAP_HISTORY_MAX_BUNDLE_LEN: usize = GAP_HISTORY_MAX_SEALED_LEN - GAP_HISTORY_OVERHEAD;

/// The sealed-box labels of a gap history: its own, so a bundle opens as no
/// other sealed envelope.
pub(crate) const SEAL_LABELS: SealLabels = SealLabels {
    key: b"lakko/sealed/gap-history/v1/key",
    nonce: b"lakko/sealed/gap-history/v1/nonce",
};

/// Why a gap history was refused. A reason code; nothing from the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum GapHistoryError {
    /// The gap public key is a low-order point (or the sealed box's
    /// ephemeral key is).
    #[error("gap key is not usable")]
    BadKey,
    /// The bundle is empty or longer than [`GAP_HISTORY_MAX_BUNDLE_LEN`].
    #[error("gap history bundle has a bad length")]
    BadBundle,
    /// The sealed bytes are shorter than the overhead or longer than the cap.
    #[error("sealed gap history has the wrong length")]
    BadLength,
    /// The version byte is not [`GAP_HISTORY_VERSION_V1`].
    #[error("sealed gap history carries unknown version {0:#04x}")]
    UnknownVersion(u8),
    /// The box did not open: another gap key, another conversation, or a
    /// tampered box.
    #[error("sealed gap history did not open")]
    NotOpened,
    /// The signature does not verify, or the signer key is not usable.
    #[error("sealed gap history is not signed by the key it names")]
    BadSignature,
}

impl GapHistoryError {
    /// Stable, language-neutral reason code. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::BadKey => "badKey",
            Self::BadBundle => "badBundle",
            Self::BadLength => "badLength",
            Self::UnknownVersion(_) => "unknownVersion",
            Self::NotOpened => "notOpened",
            Self::BadSignature => "badSignature",
        }
    }
}

/// A gap key pair. `Debug` never prints the secret.
#[derive(Clone)]
pub struct GapKey {
    secret: SecretKey,
}

impl fmt::Debug for GapKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GapKey")
            .field("public", &self.public())
            .finish_non_exhaustive()
    }
}

impl GapKey {
    /// A fresh gap key from OS randomness. One per request; the device keeps
    /// the secret with its durable request record until the bundle arrives
    /// or the record is dropped.
    #[must_use]
    pub fn generate() -> Self {
        Self {
            secret: SecretKey::generate(),
        }
    }

    /// Rebuild from a stored secret.
    #[must_use]
    pub fn from_secret(secret: &[u8; GAP_KEY_LEN]) -> Self {
        Self {
            secret: SecretKey::from_bytes(*secret),
        }
    }

    /// The 32-byte secret. Secret material.
    #[must_use]
    pub fn secret(&self) -> SecretBytes<GAP_KEY_LEN> {
        SecretBytes::new(self.secret.to_bytes())
    }

    /// The public key the request carries.
    #[must_use]
    pub fn public(&self) -> [u8; GAP_KEY_LEN] {
        self.secret.public_key().to_bytes()
    }
}

/// The rendezvous id a gap history is posted to:
/// `BLAKE3-derive-key(GAP_HISTORY_RENDEZVOUS_DOMAIN, gap_pub)`.
#[must_use]
pub fn gap_history_rendezvous(gap_pub: &[u8; GAP_KEY_LEN]) -> [u8; GAP_KEY_LEN] {
    blake3_derive_key_from_material(GAP_HISTORY_RENDEZVOUS_DOMAIN, gap_pub)
}

fn aad(gap_pub: &[u8; GAP_KEY_LEN], group_id: &[u8; GAP_KEY_LEN]) -> Vec<u8> {
    let mut out = Vec::with_capacity(GAP_HISTORY_AAD_DOMAIN.len() + 1 + 2 * GAP_KEY_LEN);
    out.extend_from_slice(GAP_HISTORY_AAD_DOMAIN);
    out.push(GAP_HISTORY_VERSION_V2);
    out.extend_from_slice(gap_pub);
    out.extend_from_slice(group_id);
    out
}

fn signed_bytes(
    group_id: &[u8; GAP_KEY_LEN],
    gap_pub: &[u8; GAP_KEY_LEN],
    signer_pub: &[u8; GAP_KEY_LEN],
    bundle: &[u8],
) -> Vec<u8> {
    let mut out =
        Vec::with_capacity(GAP_HISTORY_SIG_DOMAIN.len() + 1 + 3 * GAP_KEY_LEN + bundle.len());
    out.extend_from_slice(GAP_HISTORY_SIG_DOMAIN);
    out.push(GAP_HISTORY_VERSION_V2);
    out.extend_from_slice(group_id);
    out.extend_from_slice(gap_pub);
    out.extend_from_slice(signer_pub);
    out.extend_from_slice(bundle);
    out
}

/// An opened gap history: the bundle and the 32-byte leaf key that signed
/// it. Keep the bundle only when `lakko_mls::Group::gap_history_signer_accepted`
/// says that leaf may.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenedGapHistory {
    /// The Ed25519 leaf signature key the signature verified under.
    pub signer_leaf_key: [u8; GAP_KEY_LEN],
    /// The bundle bytes.
    pub bundle: Vec<u8>,
}

/// Seal a gap history `bundle` of the conversation `group_id` to the gap
/// public key a request carried, signed inside the box by `signer`: the
/// re-admitter's conversation leaf signing key.
///
/// # Errors
///
/// [`GapHistoryError::BadBundle`] for an empty bundle or one above
/// [`GAP_HISTORY_MAX_BUNDLE_LEN`]; [`GapHistoryError::BadKey`] for a
/// low-order gap key.
pub fn seal_gap_history(
    signer: &SigningKey,
    gap_pub: &[u8; GAP_KEY_LEN],
    group_id: &[u8; GAP_KEY_LEN],
    bundle: &[u8],
) -> CoreResult<Vec<u8>, GapHistoryError> {
    if bundle.is_empty() || bundle.len() > GAP_HISTORY_MAX_BUNDLE_LEN {
        return Err(GapHistoryError::BadBundle);
    }
    let signer_pub = signer.verifying_key().to_bytes();
    let sig = signer
        .sign(&signed_bytes(group_id, gap_pub, &signer_pub, bundle))
        .to_bytes();
    let mut plaintext = Vec::with_capacity(GAP_KEY_LEN + SIGNATURE_LEN + bundle.len());
    plaintext.extend_from_slice(&signer_pub);
    plaintext.extend_from_slice(&sig);
    plaintext.extend_from_slice(bundle);
    let recipient = PublicKey::from_array(*gap_pub);
    let sealed_box = envelope::seal_labeled(
        &SEAL_LABELS,
        &recipient,
        &aad(gap_pub, group_id),
        &plaintext,
    )
    .map_err(|_| GapHistoryError::BadKey);
    plaintext.fill(0);
    let sealed_box = sealed_box?;
    let mut out = Vec::with_capacity(1 + sealed_box.len());
    out.push(GAP_HISTORY_VERSION_V2);
    out.extend_from_slice(&sealed_box);
    Ok(out)
}

/// Open a sealed gap history with the gap secret, for the conversation
/// `group_id`: the box is opened, then the signature inside it is verified
/// (under the signer key it names, over the bundle, the group, this gap key
/// and that signer key). Returns the bundle and the signer's leaf key; keep
/// the bundle only when `lakko_mls::Group::gap_history_signer_accepted`
/// accepts that key.
///
/// # Errors
///
/// [`GapHistoryError::UnknownVersion`] (checked first),
/// [`GapHistoryError::BadLength`], [`GapHistoryError::NotOpened`] and
/// [`GapHistoryError::BadSignature`].
pub fn open_gap_history(
    gap_secret: &[u8; GAP_KEY_LEN],
    group_id: &[u8; GAP_KEY_LEN],
    sealed: &[u8],
) -> CoreResult<OpenedGapHistory, GapHistoryError> {
    let (&version, rest) = sealed.split_first().ok_or(GapHistoryError::BadLength)?;
    if version != GAP_HISTORY_VERSION_V2 {
        return Err(GapHistoryError::UnknownVersion(version));
    }
    if sealed.len() <= GAP_HISTORY_OVERHEAD || sealed.len() > GAP_HISTORY_MAX_SEALED_LEN {
        return Err(GapHistoryError::BadLength);
    }
    let key = GapKey::from_secret(gap_secret);
    let gap_pub = key.public();
    let plaintext =
        envelope::open_labeled(&SEAL_LABELS, &key.secret, &aad(&gap_pub, group_id), rest)
            .map_err(|_| GapHistoryError::NotOpened)?;
    parse_signed(group_id, &gap_pub, plaintext)
}

/// Check the signed plaintext `signer_pub(32) ‖ sig(64) ‖ bundle` of an
/// opened box.
fn parse_signed(
    group_id: &[u8; GAP_KEY_LEN],
    gap_pub: &[u8; GAP_KEY_LEN],
    mut plaintext: Vec<u8>,
) -> CoreResult<OpenedGapHistory, GapHistoryError> {
    if plaintext.len() <= GAP_KEY_LEN + SIGNATURE_LEN {
        plaintext.fill(0);
        return Err(GapHistoryError::BadLength);
    }
    let mut signer_leaf_key = [0u8; GAP_KEY_LEN];
    signer_leaf_key.copy_from_slice(&plaintext[..GAP_KEY_LEN]);
    let verified = VerifyingKey::from_bytes(&signer_leaf_key)
        .ok()
        .filter(|k| !k.is_weak())
        .zip(Signature::from_bytes(&plaintext[GAP_KEY_LEN..GAP_KEY_LEN + SIGNATURE_LEN]).ok())
        .is_some_and(|(key, sig)| {
            key.verify(
                &signed_bytes(
                    group_id,
                    gap_pub,
                    &signer_leaf_key,
                    &plaintext[GAP_KEY_LEN + SIGNATURE_LEN..],
                ),
                &sig,
            )
            .is_ok()
        });
    if !verified {
        plaintext.fill(0);
        return Err(GapHistoryError::BadSignature);
    }
    let bundle = plaintext.split_off(GAP_KEY_LEN + SIGNATURE_LEN);
    Ok(OpenedGapHistory {
        signer_leaf_key,
        bundle,
    })
}

/// Where the re-admitter starts the gap's rows: the later of the time the
/// request claims (`claimed_ms`) and a floor this device knows itself — the
/// first time it saw the returning leaf in the roster, else the last message
/// it read from that leaf. With neither known, `None`: send nothing, since
/// the claim alone could reach back to before the device was in the
/// conversation.
#[must_use]
pub const fn gap_history_start(
    claimed_ms: u64,
    leaf_first_seen_ms: Option<u64>,
    leaf_last_write_ms: Option<u64>,
) -> Option<u64> {
    let floor = match (leaf_first_seen_ms, leaf_last_write_ms) {
        (Some(first), _) => first,
        (None, Some(last)) => last,
        (None, None) => return None,
    };
    Some(if claimed_ms > floor {
        claimed_ms
    } else {
        floor
    })
}

/// The fuzz oracle behind `fuzz_api::gap_history_open`: open `bytes` under a
/// fixed secret and group (never accepted: the fuzzer cannot seal to it), and
/// check a bundle this module seals opens back to itself. `true` when every
/// assertion holds.
pub(crate) fn fuzz_open(bytes: &[u8]) -> bool {
    let key = GapKey::from_secret(&[0x42; GAP_KEY_LEN]);
    let group = [0x24; GAP_KEY_LEN];
    if open_gap_history(&[0x42; GAP_KEY_LEN], &group, bytes).is_ok() {
        return false;
    }
    // The signed-plaintext parser behind the box, fed directly: whatever a
    // fuzzer writes is never a valid signature.
    if parse_signed(&group, &key.public(), bytes.to_vec()).is_ok() {
        return false;
    }
    if bytes.is_empty() || bytes.len() > 4096 {
        return true;
    }
    let signer = SigningKey::from_seed(&[0x5A; 32]);
    let signer_pub = signer.verifying_key().to_bytes();
    let Ok(sealed) = seal_gap_history(&signer, &key.public(), &group, bytes) else {
        return false;
    };
    // The signer key is nowhere in the clear.
    let hidden = !sealed.windows(GAP_KEY_LEN).any(|w| w == signer_pub);
    let round_trips = open_gap_history(&[0x42; GAP_KEY_LEN], &group, &sealed)
        .is_ok_and(|opened| opened.bundle == bytes && opened.signer_leaf_key == signer_pub);
    hidden && round_trips
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const GROUP: [u8; 32] = [0xC1; 32];

    fn readmitter() -> SigningKey {
        SigningKey::from_seed(&[0x77; 32])
    }

    fn leaf(k: &SigningKey) -> [u8; 32] {
        k.verifying_key().to_bytes()
    }

    #[test]
    fn only_the_gap_secret_opens_it_and_it_names_its_signer() {
        let gap = GapKey::generate();
        let sealed = seal_gap_history(&readmitter(), &gap.public(), &GROUP, b"bundle").unwrap();
        assert_eq!(sealed[0], GAP_HISTORY_VERSION_V2);
        assert_eq!(sealed.len(), GAP_HISTORY_OVERHEAD + 6);
        let secret = *gap.secret().as_bytes();
        let opened = open_gap_history(&secret, &GROUP, &sealed).unwrap();
        assert_eq!(opened.signer_leaf_key, leaf(&readmitter()));
        assert_eq!(opened.bundle, b"bundle");
        let other = GapKey::generate();
        assert_eq!(
            open_gap_history(other.secret().as_bytes(), &GROUP, &sealed),
            Err(GapHistoryError::NotOpened)
        );
    }

    /// Security review 2026-11-21, second round: the signer key and the
    /// signature are inside the box, so the rendezvous shows neither.
    #[test]
    fn the_signer_is_not_visible_on_the_rendezvous() {
        let gap = GapKey::generate();
        let sealed = seal_gap_history(&readmitter(), &gap.public(), &GROUP, b"bundle").unwrap();
        let signer = leaf(&readmitter());
        assert!(!sealed.windows(32).any(|w| w == signer));
    }

    #[test]
    fn a_bundle_for_one_conversation_never_opens_as_another() {
        let gap = GapKey::generate();
        let sealed = seal_gap_history(&readmitter(), &gap.public(), &GROUP, b"bundle").unwrap();
        assert_eq!(
            open_gap_history(gap.secret().as_bytes(), &[0xC2; 32], &sealed),
            Err(GapHistoryError::NotOpened)
        );
    }

    /// The group exporter the first build used is not what seals the
    /// bundle: a member holding every group secret but not the gap secret
    /// cannot open it.
    #[test]
    fn a_member_without_the_gap_secret_cannot_open_it() {
        let gap = GapKey::generate();
        let sealed = seal_gap_history(&readmitter(), &gap.public(), &GROUP, b"x").unwrap();
        for guess in [[0u8; 32], [0x42; 32], gap.public()] {
            assert!(open_gap_history(&guess, &GROUP, &sealed).is_err());
        }
    }

    /// A member who saw the request seals invented messages with its own
    /// leaf key: the box opens and names that member, so the returning
    /// device's rule (`lakko_mls::Group::gap_history_signer_accepted`) judges
    /// that member, not the re-admitter.
    #[test]
    fn a_bundle_signed_by_another_member_names_that_member() {
        let gap = GapKey::generate();
        let impostor = SigningKey::from_seed(&[0x78; 32]);
        let forged = seal_gap_history(&impostor, &gap.public(), &GROUP, b"invented").unwrap();
        let opened = open_gap_history(gap.secret().as_bytes(), &GROUP, &forged).unwrap();
        assert_eq!(opened.signer_leaf_key, leaf(&impostor));
        assert_ne!(opened.signer_leaf_key, leaf(&readmitter()));
    }

    /// Inside the box the signature cannot be moved: another signer key named
    /// over it, an altered bundle, another group or another gap key all fail.
    #[test]
    fn a_signature_cannot_be_moved_or_the_bundle_altered() {
        let gap = GapKey::generate().public();
        let impostor = SigningKey::from_seed(&[0x78; 32]);
        let plain = |signer: &SigningKey, group: &[u8; 32], gap: &[u8; 32], bundle: &[u8]| {
            let pk = leaf(signer);
            let mut out = pk.to_vec();
            out.extend_from_slice(
                &signer
                    .sign(&signed_bytes(group, gap, &pk, bundle))
                    .to_bytes(),
            );
            out.extend_from_slice(bundle);
            out
        };
        let good = plain(&readmitter(), &GROUP, &gap, b"bundle");
        assert!(parse_signed(&GROUP, &gap, good.clone()).is_ok());
        let mut moved = plain(&impostor, &GROUP, &gap, b"bundle");
        moved[..32].copy_from_slice(&leaf(&readmitter()));
        assert_eq!(
            parse_signed(&GROUP, &gap, moved),
            Err(GapHistoryError::BadSignature)
        );
        let mut altered = good.clone();
        let last = altered.len() - 1;
        altered[last] ^= 0x01;
        assert_eq!(
            parse_signed(&GROUP, &gap, altered),
            Err(GapHistoryError::BadSignature)
        );
        assert_eq!(
            parse_signed(&[0xC2; 32], &gap, good.clone()),
            Err(GapHistoryError::BadSignature)
        );
        assert_eq!(
            parse_signed(&GROUP, &[0x01; 32], good),
            Err(GapHistoryError::BadSignature)
        );
        assert_eq!(
            parse_signed(&GROUP, &gap, vec![0u8; 96]),
            Err(GapHistoryError::BadLength)
        );
    }

    #[test]
    fn version_length_and_key_are_checked() {
        let gap = GapKey::generate();
        let mut sealed = seal_gap_history(&readmitter(), &gap.public(), &GROUP, b"x").unwrap();
        assert_eq!(
            open_gap_history(
                gap.secret().as_bytes(),
                &GROUP,
                &sealed[..GAP_HISTORY_OVERHEAD]
            ),
            Err(GapHistoryError::BadLength)
        );
        assert_eq!(
            open_gap_history(gap.secret().as_bytes(), &GROUP, &[]),
            Err(GapHistoryError::BadLength)
        );
        sealed[0] = 1;
        assert_eq!(
            open_gap_history(gap.secret().as_bytes(), &GROUP, &sealed),
            Err(GapHistoryError::UnknownVersion(1)),
            "the unsigned version 1 is refused"
        );
        assert_eq!(
            seal_gap_history(&readmitter(), &[0u8; 32], &GROUP, b"x"),
            Err(GapHistoryError::BadKey)
        );
        assert_eq!(
            seal_gap_history(&readmitter(), &gap.public(), &GROUP, b""),
            Err(GapHistoryError::BadBundle)
        );
        assert_eq!(
            seal_gap_history(
                &readmitter(),
                &gap.public(),
                &GROUP,
                &vec![0u8; GAP_HISTORY_MAX_BUNDLE_LEN + 1]
            ),
            Err(GapHistoryError::BadBundle)
        );
    }

    #[test]
    fn the_rendezvous_depends_on_the_gap_key_only() {
        let a = GapKey::generate();
        let b = GapKey::generate();
        assert_eq!(
            gap_history_rendezvous(&a.public()),
            gap_history_rendezvous(&a.public())
        );
        assert_ne!(
            gap_history_rendezvous(&a.public()),
            gap_history_rendezvous(&b.public())
        );
        // Golden: pins the domain and the derivation, for the clients.
        assert_eq!(
            hex(&gap_history_rendezvous(&[0x42; 32])),
            "4060ae1473dcf76eab6de38a8bd3d9c60909e338570703d059959404fe5e1019"
        );
    }

    fn hex(b: &[u8]) -> String {
        use core::fmt::Write as _;
        b.iter().fold(String::new(), |mut out, x| {
            let _ = write!(out, "{x:02x}");
            out
        })
    }

    #[test]
    fn the_start_is_clamped_to_what_the_readmitter_saw() {
        // No floor known: send nothing.
        assert_eq!(gap_history_start(100, None, None), None);
        // The claim reaches back before the leaf joined: the join wins.
        assert_eq!(gap_history_start(100, Some(500), None), Some(500));
        assert_eq!(gap_history_start(100, Some(500), Some(50)), Some(500));
        // Without a join time, the last write is the floor.
        assert_eq!(gap_history_start(100, None, Some(300)), Some(300));
        // A later claim stands.
        assert_eq!(gap_history_start(900, Some(500), Some(800)), Some(900));
    }

    proptest! {
        #[test]
        fn the_start_is_never_before_the_floor(
            claimed in any::<u64>(),
            first in proptest::option::of(any::<u64>()),
            last in proptest::option::of(any::<u64>()),
        ) {
            match gap_history_start(claimed, first, last) {
                None => prop_assert!(first.is_none() && last.is_none()),
                Some(start) => {
                    prop_assert!(start >= claimed);
                    let floor = first.or(last).expect("a floor");
                    prop_assert!(start >= floor);
                    prop_assert!(start == claimed || start == floor);
                },
            }
        }

        #[test]
        fn open_never_panics_and_round_trips(bytes in proptest::collection::vec(any::<u8>(), 0..300)) {
            prop_assert!(fuzz_open(&bytes));
        }

        /// Any signer's bundle opens to that signer's key and bundle, and the
        /// sealed bytes never carry the key in the clear.
        #[test]
        fn the_signer_is_reported_and_hidden(
            seed in any::<[u8; 32]>(),
            bundle in proptest::collection::vec(any::<u8>(), 1..200),
        ) {
            let gap = GapKey::generate();
            let signer = SigningKey::from_seed(&seed);
            let sealed = seal_gap_history(&signer, &gap.public(), &GROUP, &bundle).unwrap();
            prop_assert!(!sealed.windows(32).any(|w| w == leaf(&signer)));
            let opened = open_gap_history(gap.secret().as_bytes(), &GROUP, &sealed).unwrap();
            prop_assert_eq!(opened.signer_leaf_key, leaf(&signer));
            prop_assert_eq!(opened.bundle, bundle);
        }
    }
}

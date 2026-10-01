// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Committer liveness beacon + the acting-committer fold
//! (ADR-0045 decision 2, variant 1 — the candidate heartbeat).
//!
//! ## The problem this solves
//!
//! Under [ADR-0045](../../../docs/adr/0045-remote-commit-epoch-fork.md)
//! members do not commit: a member emits an MLS **Proposal** and one
//! elected owner-or-admin sweeps the pending set into a single Commit.
//! Decision 1 settled *who may sweep* — `lakko_mls::group_roles::committer_rank`,
//! a pure total function of the authenticated `0xFF02` role table, so
//! every member derives the identical ranking from identical bytes and
//! the election cannot split-brain on the question of *who rank k is*.
//!
//! Decision 2's rule is:
//!
//! > I sweep if I am the lowest-ranked candidate **currently known to be
//! > active**.
//!
//! Every word of that is free except the last clause. The chat path's
//! existing presence signals — ADR-0024 typing indicators and read
//! receipts — are **positive-only and event-driven**: observing one
//! proves a member is online, observing none proves nothing at all. An
//! admin sitting idle with the group open is exactly the candidate the
//! sweep wants, and it emits nothing. This module supplies the missing
//! piece: a **continuous, unconditional liveness beacon** and the fold
//! that turns a set of observed beacons into the local answer to *am I
//! the acting committer right now?*
//!
//! ## This is NOT a leader-election protocol
//!
//! **Read this before reading anything else, because the obvious reading
//! of the code below is wrong.** The beacon **narrows the window** in
//! which two candidates both act. It does not close it, and nothing here
//! is correct-by-construction:
//!
//! * each candidate folds over **its own** view of who beaconed recently;
//! * those views differ by poll timing, because a beacon is only observed
//!   at the observer's next journal poll;
//! * a beacon is stale by up to one full [`COMMITTER_BEACON_INTERVAL_MS`]
//!   even when the sender is perfectly healthy.
//!
//! So two candidates can still both conclude they are the lowest-ranked
//! active one. **A heartbeat removes the *wait*, not the *race*.** The two
//! mechanisms that make the election *effective* rather than merely
//! probable live outside this module and remain **required**:
//!
//! 1. **Stand-down on observation** — immediately before creating the
//!    commit, re-read the journal head; if a commit has landed at the
//!    current epoch, abandon and re-evaluate.
//! 2. **The bounded rollback** — `Group::stage_pending_proposals` /
//!    `merge_staged_sweep` / `abandon_staged_sweep`, so a committer that
//!    loses the journal order across the publish -> verdict window stays
//!    at epoch N instead of stranding itself at N+1.
//!
//! Anyone who deletes either on the strength of this module has mis-read
//! it.
//!
//! ## Why not `lakko-presence`
//!
//! `crates/lakko-presence` exists, ships blinded per-conversation presence
//! tokens, and is already exported on the wasm FFI. It is nonetheless the
//! **wrong primitive for this signal**, for four reasons, and the
//! reasoning is recorded because the crate's existence makes reaching for
//! it the obvious move:
//!
//! 1. **Its blinding buys nothing here.** A presence token hides *who* is
//!    present from an observer that lacks the group secret. This beacon
//!    rides an **MLS application message**, so the whole payload is
//!    already AEAD-sealed under the group's epoch secret and the Delivery
//!    Service sees ciphertext (invariant #2). The blinding would be
//!    applied *inside* a layer that already provides exactly the
//!    confidentiality it exists to provide.
//! 2. **Attribution runs the wrong way, and is weaker.** `derive_token`
//!    is identified by *recomputing* the expected token for every
//!    candidate `member_pseudonym` and matching — an O(members) scan that
//!    yields a **pseudonym**. The fold needs a **signature key**, to look
//!    up `committer_rank` in the role table. MLS already hands the
//!    receiver the sender leaf for free (`Incoming::Application { sender_leaf }`,
//!    ADR-0023), which resolves to the signature key through `members()`.
//!    A token is also **not authenticating** — every member holds the
//!    seed, so any member can mint any other member's token; the crate
//!    says so itself. The sender leaf is MLS-authenticated. Swapping an
//!    authenticated attribution for an unauthenticated one, to obtain
//!    something the transport already delivered, is a strict loss.
//! 3. **Its clock model contradicts the ADR.** `derive_token` binds a
//!    wall-clock epoch from `current_epoch(now_unix_ms, ..)` — the
//!    **sender's** clock — into the token, and a receiver must recompute
//!    with the same epoch to match at all. ADR-0045 is explicit that
//!    freshness is measured against the DS row's server-assigned
//!    `created_at`, never the sender's clock. Adopting tokens would put
//!    the sender's clock back into the decision as a *matching
//!    precondition*: a candidate with a skewed clock would emit tokens
//!    nobody can match and read as permanently absent — silently, and
//!    exactly when it is the device that should be sweeping.
//! 4. **It is not actually wired.** The crate's own docs place sourcing
//!    `presence_seed` from the MLS exporter outside itself ("lives in
//!    `lakko-mls` once we wire the integration"), and that exporter label
//!    does not exist. Nothing under `apps/web/src` calls it and there is
//!    no UniFFI counterpart. So adopting it is not reuse — it is building
//!    a new MLS exporter, a pseudonym enumeration and a second FFI
//!    surface, in order to obtain a weaker form of an attribution MLS
//!    already provides.
//!
//! **What would make it the right primitive**, recorded so the crate is
//! judged rather than dismissed: if this beacon ever had to travel
//! *outside* MLS — to non-members, or over a channel the server can read —
//! the blinding is precisely the tool, because then there would be no
//! enclosing layer providing confidentiality and no authenticated sender
//! attribution to lean on. Inside MLS there is both.
//!
//! ## Wire shape
//!
//! ```text
//!   plaintext_payload =
//!       COMMITTER_BEACON_ENVELOPE_DOMAIN_V1 || version_byte
//!      └────────── 26 bytes, ASCII ────────┘ └─── 0x01 ───┘
//!
//!                  "lakko/committer-beacon/v1\x00"
//! ```
//!
//! A fixed 27 bytes, and **the payload carries no fields at all**. That
//! is a decision, not an oversight — three things a beacon might
//! plausibly carry were each left out for a reason:
//!
//! * **The sender's identity.** Already MLS-authenticated via the sender
//!   leaf. Naming itself in the payload would add a second, *unauthenticated*
//!   claim of identity next to the authenticated one, which is a spoofing
//!   surface and nothing else.
//! * **A timestamp.** ADR-0045 measures age against the DS row's
//!   server-assigned `created_at`, which every member reads identically
//!   on the identical row. A self-reported time is not merely redundant
//!   but **wrong**: it would let a candidate hold the committer duty by
//!   asserting freshness, which is a liveness claim no peer can check.
//! * **The sender's claimed rank.** The receiver re-derives rank from the
//!   authenticated role table, so a claimed rank would have to be ignored.
//!   Carrying a field every correct implementation must ignore is an
//!   invitation for one implementation to trust it.
//!
//! What remains is the signal itself, whose entire content is *"the
//! device at this sender leaf was foregrounded and able to sweep when the
//! DS stamped this row"*. The version byte survives as the in-payload
//! forward-compatibility knob — the same knob ADR-0044's v2 bump proved
//! load-bearing on the sibling push-routing envelope — so a future
//! revision that does add a field fails closed on an old decoder rather
//! than being mis-read.
//!
//! ## Cross-kind isolation
//!
//! The separator MUST be byte-distinct from every other envelope-domain
//! prefix in this crate; [`tests::separator_distinct_from_sibling_domains`]
//! asserts it. Dispatch in [`crate::incoming_envelope`] is a byte-prefix
//! match, so a distinct prefix guarantees a beacon never cross-dispatches
//! as a sibling envelope, is never rendered as a chat bubble, and never
//! swallows a sibling.

use core::result::Result as CoreResult;

use thiserror::Error;

/// 26-byte domain separator prepended to the beacon payload before it is
/// handed to the MLS application-message send path.
///
/// The trailing `\x00` makes the prefix impossible for a UTF-8 free-text
/// payload to start with, mirroring
/// [`crate::media_state::MEDIA_STATE_ENVELOPE_DOMAIN_V1`]. The printable
/// prefix is `lakko/committer-beacon/v1`.
///
/// **The `_V1` names the envelope FAMILY, not the schema version.** These
/// 26 bytes are frozen: [`crate::incoming_envelope`] dispatches on this
/// exact prefix, so changing them would make a peer fall through to the
/// chat parser and render the beacon as garbage text. The in-payload
/// version byte is the knob that moves.
pub const COMMITTER_BEACON_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/committer-beacon/v1\x00";

/// Schema-version byte, carried as the only byte after the separator.
///
/// Today the sole accepted value is `0x01`. A revision that adds a field
/// bumps this so an old decoder fail-closes rather than mis-reading the
/// new layout.
pub const COMMITTER_BEACON_VERSION_V1: u8 = 0x01;

/// Exact byte length of the payload following the domain separator: the
/// version byte, and nothing else.
///
/// The decoder validates against this **exact** length — anything longer
/// or shorter is fail-closed rejected — for the same reason
/// [`crate::media_state`] does: the payload is a fixed-shape signal, so
/// there is no variable-length blob to bound from below.
pub const COMMITTER_BEACON_PAYLOAD_LEN: usize = 1;

/// How often a candidate re-emits its beacon, in milliseconds.
///
/// # This number is CHOSEN, not measured
///
/// No measurement backs it and none is claimed. What backs it is a budget
/// argument, stated so a later measurement can overturn it:
///
/// * **The beacon's latency budget is not interactive.** Nobody watches
///   it. What waits on it is a membership change that ADR-0045 decision 2
///   deliberately gave **no deadline at all**. So the interval should be
///   sized for litter and privacy, not for responsiveness — which is the
///   opposite of ADR-0024's ~4 s typing throttle, whose whole job is to
///   look instant to a human.
/// * **It is a litter budget.** The beacon is a group-journal row, and
///   journal rows persist to the 30-day prune. ADR-0024 already records
///   journal-resident typing rows as an anti-pattern it accepted
///   grudgingly; a beacon is the same shape but **continuous rather than
///   event-driven**, so it multiplies exactly that cost. At 60 s that is
///   60 rows per hour per candidate per group — roughly 1440 a day for a
///   candidate that leaves the app open. Halving the interval doubles
///   that for no benefit anyone is waiting on.
/// * **It is a privacy budget.** Each row publishes that this candidate
///   had the app foregrounded at that moment, to every member of the
///   group, retained for the TTL. Typing leaks that you were composing; a
///   beacon leaks your activity schedule. The maintainer accepted this on
///   the ground that online/offline visibility is ordinary messenger
///   functionality — but a shorter interval buys a **finer-grained**
///   schedule, which is a real cost with no matching gain.
///
/// The takeover latency that falls out is bounded by
/// [`COMMITTER_BEACON_FRESHNESS_WINDOW_MS`], not by this constant.
pub const COMMITTER_BEACON_INTERVAL_MS: u64 = 60_000;

/// The slowest group-poll cadence in the tree, in milliseconds, used as a
/// term in the freshness window below.
///
/// Android's `pollGroup` awaits a deliberate 30 s long poll, which is the
/// worst case; web and the channel co-publisher loops are faster. This is
/// **observed from the client code, not measured under load** — it is the
/// configured cadence, and a client that ever polls slower than this
/// invalidates the window's derivation, which is exactly why the term is
/// named here instead of being folded silently into one number.
pub const COMMITTER_BEACON_WORST_CASE_POLL_MS: u64 = 30_000;

/// How long a beacon keeps counting its sender as an active candidate,
/// in milliseconds, measured against the **DS row's** age.
///
/// # This number is CHOSEN, not measured — but it is CONSTRUCTED
///
/// It is `2 * INTERVAL + WORST_CASE_POLL`, and each term earns its place:
///
/// * `1 * INTERVAL` — a perfectly healthy candidate's most recent beacon
///   is already up to one full interval old the moment anyone looks.
/// * `+ 1 * INTERVAL` — tolerate exactly **one** lost or delayed beacon,
///   so a single dropped journal round does not evict a live candidate
///   and hand the duty to the rank below it.
/// * `+ WORST_CASE_POLL` — a beacon is only *observed* at the observer's
///   next poll, so on the slowest client the age already includes up to
///   one poll cadence before the observer has any chance to react.
///
/// [`tests::freshness_window_is_the_documented_construction`] pins the
/// arithmetic, so shrinking the window below the tolerate-one-loss point
/// fails loudly instead of quietly making takeovers flap.
///
/// **What it costs:** an offline owner is noticed within one window, so a
/// rank-1 admin takes over within ~2.5 minutes. Against a request that
/// has no deadline, that is cheap. **What it buys:** the window is the
/// only bound on a *positive* observation, which is a strictly better
/// place for a constant than variant 2's stagger, which had to license an
/// inference from *silence*.
pub const COMMITTER_BEACON_FRESHNESS_WINDOW_MS: u64 =
    2 * COMMITTER_BEACON_INTERVAL_MS + COMMITTER_BEACON_WORST_CASE_POLL_MS;

/// A decoded committer-liveness beacon.
///
/// Deliberately field-less: everything a receiver needs — who sent it and
/// how old it is — comes from the MLS sender leaf and the DS row, not
/// from the payload. See the module docs for what was left out and why.
/// It exists as a named type rather than a bare `bool` so a v2 that does
/// carry a field has somewhere to put it without changing every call
/// site's shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CommitterBeacon;

/// One observed beacon, reduced to the three things the fold reads.
///
/// The caller builds this from a journal row it has already decrypted and
/// dispatched: the MLS sender leaf resolves to a signature key through
/// `Group::members()`, that key goes through `Group::committer_rank_of`
/// (the account's rank, held by every leaf of the account, ADR-0050
/// section 2.5a item 6), and the row's server-assigned `created_at` gives
/// the age.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BeaconObservation {
    /// The committer rank the **observer re-derived** for this beacon's
    /// MLS-authenticated sender, or `None` when the sender is not a
    /// candidate at all.
    ///
    /// **Nothing in the payload contributes to this value** — the beacon
    /// carries no identity and no rank claim, so this is derived
    /// end-to-end from authenticated state: the MLS sender leaf, the
    /// group roster, and the `0xFF02` role table. A `None` here is the
    /// ordinary case for a beacon from a plain member (which should not
    /// have been emitted at all, but costs nothing to drop) and for a
    /// sender whose leaf no longer resolves, e.g. one removed since.
    pub rank: Option<u32>,
    /// Age of the beacon's DS row in milliseconds: `now - created_at`,
    /// where `created_at` is the **server-assigned** `TIMESTAMP(3)` every
    /// member reads identically on the identical row.
    ///
    /// The sender's clock never enters this. What remains is the
    /// observer's own offset from the DS clock, which is a real residual
    /// and the reason the window carries slack.
    ///
    /// **A row that appears to come from the future must be clamped to
    /// `0`, not wrapped**, which the unsigned type forces. That is also
    /// the safe direction: an age that reads *too fresh* makes this
    /// device defer and the request wait a little longer, while an age
    /// that reads *too stale* makes it act and risks a double sweep.
    pub age_ms: u64,
    /// The sender's 32-byte leaf **signature key**, the one the MLS
    /// sender leaf resolved to at receipt — the same key the caller ranks
    /// to fill [`Self::rank`], and never a leaf index (OpenMLS reuses a
    /// vacated leaf).
    ///
    /// It breaks the tie between leaves of one rank (ADR-0050 section
    /// 2.5a item 6): every device of a ranked account holds the account's
    /// rank, so the account's devices order themselves by this key. Like
    /// the rank it comes from authenticated state, never from the
    /// field-less payload.
    pub signature_key: [u8; 32],
}

/// The fold's answer to *am I the acting committer right now?*
///
/// A named verdict rather than a `bool` so the reason survives into logs
/// and so a caller cannot accidentally collapse "not a candidate" and
/// "deferring to someone better placed" into one branch — they are
/// different situations with different follow-ups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActingCommitter {
    /// This device is the acting committer: it is a candidate and has
    /// heard no fresh beacon from any better-placed candidate — a lower
    /// rank, or the same rank with a lower signature key.
    ///
    /// **Including when it has heard nothing at all.** Silence means
    /// nobody better placed is running, which makes this device the
    /// answer — not a failure to find one.
    ///
    /// It is still only a licence to *attempt* the sweep. The stand-down
    /// journal re-read and the staged-commit rollback both still run.
    Act,
    /// A better-placed candidate beaconed inside the freshness window,
    /// so this device stands down and re-evaluates on its next poll.
    ///
    /// Better placed means a strictly lower rank, or the same rank and a
    /// bytewise lower signature key (another device of the same account,
    /// ADR-0050 section 2.5a item 6).
    Defer {
        /// The rank of the best-placed fresh observation. Equal to this
        /// device's own rank when it defers to a lower-keyed device of
        /// the same rank. Purely informational — the decision is already
        /// made.
        to_rank: u32,
        /// The signature key of that observation, so a log line answers
        /// *deferring to whom* even between two devices of one account.
        /// Never this device's own key.
        to_signature_key: [u8; 32],
    },
    /// This device is not an owner or admin, so it can never sweep.
    /// Distinct from [`ActingCommitter::Defer`]: there is nothing to
    /// re-evaluate and no beacon this device should be emitting.
    NotACandidate,
}

impl ActingCommitter {
    /// `true` only for [`ActingCommitter::Act`].
    ///
    /// Convenience for the common call site; the variant is still the
    /// thing to log.
    #[must_use]
    pub const fn is_act(self) -> bool {
        matches!(self, Self::Act)
    }
}

/// The fold: decide whether this device is the acting committer.
///
/// Pure, total, panic-free, and free of I/O and clocks — the caller
/// supplies `now` implicitly, as already-computed ages, exactly as
/// [`crate::push_routing::push_routing_secret_is_newer`] takes
/// already-read values rather than reaching for state itself. It lives
/// here, once, and is consumed over the FFI, because three clients each
/// re-deriving it is precisely the drift class that produced ADR-0044's
/// F-A replay.
///
/// # The rules
///
/// 1. **A non-candidate never sweeps.** `own_rank == None` is
///    [`ActingCommitter::NotACandidate`], evaluated first and without
///    looking at a single beacon.
/// 2. **Beacons from non-candidates are ignored**, by re-derived rank —
///    an observation whose `rank` is `None` is dropped. Nothing in the
///    payload could have influenced that, because the payload carries no
///    identity and no rank.
/// 3. **Stale beacons are ignored.** `age_ms > freshness_window_ms` is
///    dropped. Ages come from the DS row, never a sender's clock.
/// 4. **This device's own beacon is ignored**: an observation whose
///    signature key equals `own_signature_key` is dropped whatever its
///    rank, so a device's own echoed-back beacon can never make it stand
///    down for itself.
/// 5. **Only a better-placed candidate defers this device**: a strictly
///    lower rank, or the **same** rank with a **strictly lower**
///    signature key, compared bytewise. Since ADR-0050 section 2.5a item
///    6 every device of a ranked account holds the account's rank (a lost
///    device stays in the roster, so "the lowest leaf key in the roster"
///    could name a device nobody holds any more); the account's acting
///    device is therefore its lowest-keyed device that is **active**,
///    i.e. whose beacon is fresh. The owner is no exception: rank 0 never
///    waits for anyone of a higher rank, but a rank-0 device does defer
///    to a fresh, lower-keyed device of the owner's own account.
/// 6. **Hearing nothing means acting.** A candidate that survives rules
///    2-5 with an empty set is the acting committer. That is the correct
///    answer when nobody better placed is running, and reading it as a
///    failure to elect is the single most likely way to get this wrong.
///
/// When several observations are better placed, [`ActingCommitter::Defer`]
/// names the best of them: the lowest `(rank, signature_key)` pair.
///
/// Two devices sharing one signature key both act (rule 4 makes each
/// ignore the other) — ADR-0045's known residual, repaired by the
/// rollback rather than prevented here. Two devices of one account that
/// do not yet hear each other both act too: that is the ADR-0045 race,
/// resolved by the staged sweep.
///
/// # This does not close the race
///
/// See the module docs. Views differ by poll timing and a beacon is stale
/// by up to one interval, so two candidates can both reach
/// [`ActingCommitter::Act`]. The stand-down re-read and the bounded
/// rollback remain required.
#[must_use]
pub fn acting_committer_verdict(
    own_rank: Option<u32>,
    own_signature_key: &[u8; 32],
    observed: &[BeaconObservation],
    freshness_window_ms: u64,
) -> ActingCommitter {
    // Rule 1 — a plain member is done here, without reading a beacon.
    let Some(own) = own_rank else {
        return ActingCommitter::NotACandidate;
    };

    // Rules 2-5, in one pass: fresh beacons, from re-derived candidates,
    // not this device's own, better placed than this device. Tuples
    // compare rank first and the key second, which is exactly the order.
    let best_active = observed
        .iter()
        .filter(|obs| obs.age_ms <= freshness_window_ms)
        .filter(|obs| obs.signature_key != *own_signature_key)
        .filter_map(|obs| obs.rank.map(|rank| (rank, obs.signature_key)))
        .filter(|candidate| *candidate < (own, *own_signature_key))
        .min();

    // Rule 6 — nothing better placed is running, so this device is it.
    best_active.map_or(ActingCommitter::Act, |(to_rank, to_signature_key)| {
        ActingCommitter::Defer {
            to_rank,
            to_signature_key,
        }
    })
}

/// The send-side throttle: should this device emit a beacon now?
///
/// Pure and total. Kept beside the fold, and consumed over the FFI, for
/// the same anti-drift reason: the interval and the freshness window are
/// two halves of one budget, and a client that re-derives the emit
/// cadence locally can silently fall out of step with the window every
/// other client is judging it against.
///
/// # The rules
///
/// 1. **Only candidates emit.** `own_rank == None` is `false`. A plain
///    member's beacon could never make it the acting committer — the fold
///    drops it by rule 3 — so emitting one is pure journal litter and a
///    pure privacy leak. In a 50-member group with an owner and two
///    admins this is three emitters, not fifty.
/// 2. **The first beacon of a session is immediate.**
///    `last_emitted_at_ms == None` is `true`, so a candidate that has
///    just opened the app announces itself at once rather than being
///    invisible for a whole interval — which is when a stale view is
///    likeliest to hand the duty to the wrong device.
/// 3. **Otherwise, one per [`COMMITTER_BEACON_INTERVAL_MS`].**
///
/// # Clocks
///
/// Both timestamps come from the caller's **own local** clock, and this
/// is the one place in the beacon design where a local clock is
/// legitimate: it governs only this device's own send cadence and feeds
/// no judgement about anybody else. A backward clock jump yields `0`
/// through `saturating_sub`, which suppresses one beacon and self-heals
/// on the next tick — the safe direction, since an extra beacon is litter
/// while a missing one is already covered by the window's
/// tolerate-one-loss slack.
///
/// # What this function cannot know
///
/// **Foregroundedness.** ADR-0045 requires that a beacon mean *able to
/// sweep*, not merely *device awake* — the ADR-0041 background decrypt is
/// read-only-throwaway and structurally cannot commit (C-59), so a
/// beacon emitted from the background would be a lie that costs the group
/// a takeover. The core has no notion of a foreground, so gating on it is
/// a **caller obligation**: call this only from a foregrounded, polling
/// client.
#[must_use]
pub fn should_emit_beacon(
    own_rank: Option<u32>,
    last_emitted_at_ms: Option<u64>,
    now_ms: u64,
) -> bool {
    if own_rank.is_none() {
        return false;
    }
    // `map_or(true, ..)` rather than `is_none_or`, which is stable only
    // since 1.82 and this workspace's MSRV is 1.79.
    last_emitted_at_ms.map_or(true, |last| {
        now_ms.saturating_sub(last) >= COMMITTER_BEACON_INTERVAL_MS
    })
}

/// Compose a committer-beacon envelope.
///
/// The output is the **inner** plaintext the sender hands to the MLS
/// application-message send path, which seals it before it reaches the
/// DS. A fixed 27 bytes: `domain-separator || version-byte`.
///
/// Infallible, and takes no arguments, because the beacon carries no
/// fields — see the module docs.
#[must_use]
pub fn compose_committer_beacon() -> Vec<u8> {
    let mut out = Vec::with_capacity(
        COMMITTER_BEACON_ENVELOPE_DOMAIN_V1.len() + COMMITTER_BEACON_PAYLOAD_LEN,
    );
    out.extend_from_slice(COMMITTER_BEACON_ENVELOPE_DOMAIN_V1);
    out.push(COMMITTER_BEACON_VERSION_V1);
    out
}

/// Discriminated unwrap for the [`crate::incoming_envelope`] dispatcher.
///
/// Returns:
///   * `Ok(Some(CommitterBeacon))` — `plaintext` carried the beacon
///     domain separator and the payload behind it decoded cleanly.
///   * `Ok(None)` — the prefix does not match; the dispatcher falls
///     through to the next classification step.
///   * `Err(CommitterBeaconError)` — the prefix matched but the payload
///     is malformed. A syntactically-marked but semantically-invalid
///     envelope: the caller logs and drops it rather than mis-rendering
///     it as chat text (fail-closed, mirroring the sibling
///     `try_unwrap_*` helpers).
///
/// # Errors
///
/// * [`CommitterBeaconError::BadLength`] — the trailing payload is not
///   exactly [`COMMITTER_BEACON_PAYLOAD_LEN`] bytes.
/// * [`CommitterBeaconError::UnknownVersion`] — the version byte is not
///   [`COMMITTER_BEACON_VERSION_V1`].
pub fn try_unwrap_committer_beacon(
    plaintext: &[u8],
) -> CoreResult<Option<CommitterBeacon>, CommitterBeaconError> {
    if !plaintext.starts_with(COMMITTER_BEACON_ENVELOPE_DOMAIN_V1) {
        return Ok(None);
    }
    let payload = &plaintext[COMMITTER_BEACON_ENVELOPE_DOMAIN_V1.len()..];
    if payload.len() != COMMITTER_BEACON_PAYLOAD_LEN {
        return Err(CommitterBeaconError::BadLength {
            expected: COMMITTER_BEACON_PAYLOAD_LEN,
            got: payload.len(),
        });
    }
    if payload[0] != COMMITTER_BEACON_VERSION_V1 {
        return Err(CommitterBeaconError::UnknownVersion(payload[0]));
    }
    Ok(Some(CommitterBeacon))
}

/// Strict standalone decoder: parse a beacon envelope, treating a
/// non-beacon plaintext as a hard error.
///
/// The fail-closed entry point for a caller that already knows the
/// payload is *supposed* to be a beacon. The dispatcher-facing
/// [`try_unwrap_committer_beacon`] is the helper that returns `Ok(None)`
/// for the fall-through case instead.
///
/// # Errors
///
/// * [`CommitterBeaconError::WrongDomain`] — `bytes` does not start with
///   [`COMMITTER_BEACON_ENVELOPE_DOMAIN_V1`].
/// * [`CommitterBeaconError::BadLength`] /
///   [`CommitterBeaconError::UnknownVersion`] — see
///   [`try_unwrap_committer_beacon`].
pub fn decode_committer_beacon(bytes: &[u8]) -> CoreResult<CommitterBeacon, CommitterBeaconError> {
    match try_unwrap_committer_beacon(bytes)? {
        Some(beacon) => Ok(beacon),
        None => Err(CommitterBeaconError::WrongDomain),
    }
}

/// Errors specific to the committer-beacon decode path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CommitterBeaconError {
    /// The plaintext does not start with the beacon domain separator.
    /// Only surfaced by [`decode_committer_beacon`]; the
    /// dispatcher-facing [`try_unwrap_committer_beacon`] returns
    /// `Ok(None)` so the dispatch chain can continue.
    #[error("plaintext does not carry the committer-beacon domain separator")]
    WrongDomain,

    /// The domain separator matched but the trailing payload is not
    /// exactly [`COMMITTER_BEACON_PAYLOAD_LEN`] bytes.
    #[error("committer-beacon payload is {got} bytes, expected exactly {expected}")]
    BadLength {
        /// Mirror of [`COMMITTER_BEACON_PAYLOAD_LEN`] at the time the
        /// error fired, so the log line is self-contained.
        expected: usize,
        /// Observed payload length after the separator. Always
        /// `!= expected`.
        got: usize,
    },

    /// The version byte is not [`COMMITTER_BEACON_VERSION_V1`].
    #[error("committer-beacon payload carries unknown schema version {0:#04x}")]
    UnknownVersion(u8),
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// This device's own signature key in every fold test.
    const OWN: [u8; 32] = [0x80; 32];
    /// A key that sorts above [`OWN`], so an equal-rank observation under
    /// it never defers this device.
    const HIGHER: [u8; 32] = [0xEE; 32];
    /// A key that sorts below [`OWN`].
    const LOWER: [u8; 32] = [0x10; 32];

    /// Shorthand for a beacon observed `age_ms` ago from a candidate at
    /// `rank`, under a key that sorts above [`OWN`].
    const fn obs(rank: u32, age_ms: u64) -> BeaconObservation {
        obs_k(rank, age_ms, HIGHER)
    }

    /// A beacon observed from a candidate at `rank` under `key`.
    const fn obs_k(rank: u32, age_ms: u64, key: [u8; 32]) -> BeaconObservation {
        BeaconObservation {
            rank: Some(rank),
            age_ms,
            signature_key: key,
        }
    }

    /// Shorthand for a beacon observed from a **non**-candidate.
    const fn non_candidate(age_ms: u64) -> BeaconObservation {
        BeaconObservation {
            rank: None,
            age_ms,
            signature_key: LOWER,
        }
    }

    /// The fold as this device ([`OWN`]) takes it.
    fn verdict(
        own_rank: Option<u32>,
        observed: &[BeaconObservation],
        window: u64,
    ) -> ActingCommitter {
        acting_committer_verdict(own_rank, &OWN, observed, window)
    }

    /// A deferral to the [`obs`] key at `to_rank`.
    const fn defer(to_rank: u32) -> ActingCommitter {
        ActingCommitter::Defer {
            to_rank,
            to_signature_key: HIGHER,
        }
    }

    const WINDOW: u64 = COMMITTER_BEACON_FRESHNESS_WINDOW_MS;

    // -------------------------------------------------------------------
    // Envelope — wire format
    // -------------------------------------------------------------------

    #[test]
    fn compose_then_decode_round_trips() {
        let wrapped = compose_committer_beacon();
        assert!(wrapped.starts_with(COMMITTER_BEACON_ENVELOPE_DOMAIN_V1));
        assert_eq!(decode_committer_beacon(&wrapped).unwrap(), CommitterBeacon);
    }

    #[test]
    fn compose_layout_is_domain_then_version_and_nothing_else() {
        // Pin the exact wire layout — the format is frozen, and the
        // "nothing else" half is the whole point: a field added by
        // accident surfaces here.
        let wrapped = compose_committer_beacon();
        let sep = COMMITTER_BEACON_ENVELOPE_DOMAIN_V1.len();
        assert_eq!(&wrapped[..sep], COMMITTER_BEACON_ENVELOPE_DOMAIN_V1);
        assert_eq!(wrapped[sep], COMMITTER_BEACON_VERSION_V1);
        assert_eq!(wrapped.len(), sep + 1, "the payload is one byte");
        assert_eq!(wrapped.len(), 27, "total envelope is 27 bytes");
    }

    #[test]
    fn the_envelope_is_deterministic() {
        // Two beacons are byte-identical: there is no timestamp, no
        // nonce and no identity in the payload, so nothing can vary.
        // A field that leaked in would almost certainly break this.
        assert_eq!(compose_committer_beacon(), compose_committer_beacon());
    }

    #[test]
    fn domain_separator_is_26_bytes_and_ends_with_null() {
        assert_eq!(COMMITTER_BEACON_ENVELOPE_DOMAIN_V1.len(), 26);
        assert_eq!(*COMMITTER_BEACON_ENVELOPE_DOMAIN_V1.last().unwrap(), 0u8);
        let printable = &COMMITTER_BEACON_ENVELOPE_DOMAIN_V1[..25];
        assert_eq!(
            core::str::from_utf8(printable).unwrap(),
            "lakko/committer-beacon/v1",
        );
    }

    // -------------------------------------------------------------------
    // Envelope — fail-closed decode
    // -------------------------------------------------------------------

    #[test]
    fn try_unwrap_returns_none_for_non_beacon_plaintext() {
        assert!(try_unwrap_committer_beacon(b"hi alice it is bob")
            .unwrap()
            .is_none());
    }

    #[test]
    fn decode_rejects_non_beacon_plaintext() {
        assert_eq!(
            decode_committer_beacon(b"hi alice it is bob").unwrap_err(),
            CommitterBeaconError::WrongDomain,
        );
    }

    #[test]
    fn empty_plaintext_falls_through_rather_than_reading_as_truncated() {
        assert!(try_unwrap_committer_beacon(&[]).unwrap().is_none());
        assert_eq!(
            decode_committer_beacon(&[]).unwrap_err(),
            CommitterBeaconError::WrongDomain,
        );
    }

    #[test]
    fn decode_rejects_the_bare_separator_with_no_version_byte() {
        // The domain matched, so this is NOT a fall-through — it is a
        // malformed beacon and must be an Err, not Ok(None).
        let wrapped = COMMITTER_BEACON_ENVELOPE_DOMAIN_V1.to_vec();
        assert_eq!(
            decode_committer_beacon(&wrapped).unwrap_err(),
            CommitterBeaconError::BadLength {
                expected: 1,
                got: 0
            },
        );
    }

    #[test]
    fn decode_rejects_overlong_payload() {
        // The exact-length gate: a trailing byte after the version must
        // not be silently ignored, or a v2 field would be readable by a
        // v1 decoder that has no idea what it means.
        let mut wrapped = COMMITTER_BEACON_ENVELOPE_DOMAIN_V1.to_vec();
        wrapped.extend_from_slice(&[COMMITTER_BEACON_VERSION_V1, 0xFF]);
        assert_eq!(
            decode_committer_beacon(&wrapped).unwrap_err(),
            CommitterBeaconError::BadLength {
                expected: 1,
                got: 2
            },
        );
    }

    #[test]
    fn decode_rejects_unknown_version() {
        let mut wrapped = COMMITTER_BEACON_ENVELOPE_DOMAIN_V1.to_vec();
        wrapped.push(0x02);
        assert_eq!(
            decode_committer_beacon(&wrapped).unwrap_err(),
            CommitterBeaconError::UnknownVersion(0x02),
        );
    }

    #[test]
    fn decode_rejects_a_zero_version_byte() {
        // 0x00 is a plausible accident (zeroed buffer); it must not read
        // as v1.
        let mut wrapped = COMMITTER_BEACON_ENVELOPE_DOMAIN_V1.to_vec();
        wrapped.push(0x00);
        assert_eq!(
            decode_committer_beacon(&wrapped).unwrap_err(),
            CommitterBeaconError::UnknownVersion(0x00),
        );
    }

    // -------------------------------------------------------------------
    // Envelope — cross-kind isolation
    // -------------------------------------------------------------------

    #[test]
    fn separator_distinct_from_sibling_domains() {
        // Cross-kind isolation across every envelope-domain constant in
        // the crate — a future collision surfaces here.
        let other_domains: &[&[u8]] = &[
            crate::call_welcome::CALL_WELCOME_ENVELOPE_DOMAIN_V1,
            crate::call_kp_forward::CALL_KP_FORWARD_ENVELOPE_DOMAIN_V1,
            crate::media_state::MEDIA_STATE_ENVELOPE_DOMAIN_V1,
            crate::push_routing::PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1,
            crate::profile::PROFILE_ENVELOPE_DOMAIN_V1,
            crate::shared_channels::SHARED_CHANNELS_ENVELOPE_DOMAIN_V1,
            crate::group_name::GROUP_NAME_ENVELOPE_DOMAIN_V1,
            crate::pin::PIN_ENVELOPE_DOMAIN_V1,
            crate::introduction::INTRODUCTION_OFFER_ENVELOPE_DOMAIN_V1,
            crate::introduction::INTRODUCTIONS_ENVELOPE_DOMAIN_V1,
            crate::visibility::VISIBILITY_ENVELOPE_DOMAIN_V1,
            crate::visibility::REACH_STATE_ENVELOPE_DOMAIN_V1,
            crate::committer_delegation::COMMITTER_DELEGATION_ENVELOPE_DOMAIN_V1,
            crate::profile_request::PROFILE_REQUEST_ENVELOPE_DOMAIN_V1,
            b"lakko-invite-revoke/v1\x00",
            b"lakko-call-invite-revoke/v1\x00",
            crate::invite::JOINER_TAG_SALT_DOMAIN_V1,
        ];
        for d in other_domains {
            assert_ne!(
                *d, COMMITTER_BEACON_ENVELOPE_DOMAIN_V1,
                "beacon separator must be byte-distinct from {d:?}",
            );
            // Prefix dispatch: neither may be a prefix of the other, or
            // one kind would swallow the other before it is reached.
            assert!(
                !d.starts_with(COMMITTER_BEACON_ENVELOPE_DOMAIN_V1)
                    && !COMMITTER_BEACON_ENVELOPE_DOMAIN_V1.starts_with(d),
                "neither separator may prefix the other: {d:?}",
            );
        }
    }

    #[test]
    fn beacon_does_not_cross_dispatch_as_a_sibling() {
        use crate::call_kp_forward::{try_unwrap_call_kp_forward, wrap_call_kp_forward};
        use crate::call_welcome::{try_unwrap_call_welcome, wrap_call_welcome};
        use crate::media_state::{compose_media_state, try_unwrap_media_state};
        use crate::push_routing::{compose_push_routing_secret, try_unwrap_push_routing_secret};

        let beacon = compose_committer_beacon();
        assert!(try_unwrap_call_welcome(&beacon).unwrap().is_none());
        assert!(try_unwrap_call_kp_forward(&beacon).unwrap().is_none());
        assert!(try_unwrap_media_state(&beacon).unwrap().is_none());
        assert!(try_unwrap_push_routing_secret(&beacon).unwrap().is_none());

        // ...and no sibling reads as a beacon.
        let welcome = wrap_call_welcome(&vec![0xABu8; 256]);
        assert!(try_unwrap_committer_beacon(&welcome).unwrap().is_none());
        let kp = wrap_call_kp_forward(&vec![0xCDu8; 512]);
        assert!(try_unwrap_committer_beacon(&kp).unwrap().is_none());
        let media = compose_media_state(true);
        assert!(try_unwrap_committer_beacon(&media).unwrap().is_none());
        let secret = compose_push_routing_secret(7, &[0x11u8; 32]);
        assert!(try_unwrap_committer_beacon(&secret).unwrap().is_none());
    }

    // -------------------------------------------------------------------
    // Constants — the chosen numbers, pinned as a construction
    // -------------------------------------------------------------------

    #[test]
    fn freshness_window_is_the_documented_construction() {
        // window = 2 * interval + worst-case poll. Pinned so a later edit
        // that shrinks the window has to confront the derivation rather
        // than quietly make takeovers flap.
        assert_eq!(
            COMMITTER_BEACON_FRESHNESS_WINDOW_MS,
            2 * COMMITTER_BEACON_INTERVAL_MS + COMMITTER_BEACON_WORST_CASE_POLL_MS,
        );
        assert_eq!(COMMITTER_BEACON_INTERVAL_MS, 60_000);
        assert_eq!(COMMITTER_BEACON_WORST_CASE_POLL_MS, 30_000);
        assert_eq!(COMMITTER_BEACON_FRESHNESS_WINDOW_MS, 150_000);
    }

    #[test]
    fn freshness_window_tolerates_a_full_missed_beacon() {
        // The load-bearing inequality: a candidate that loses exactly one
        // beacon on the slowest client must still read as active. If the
        // window ever drops to <= one interval, a perfectly healthy
        // candidate is evicted on ordinary jitter.
        const {
            assert!(
                COMMITTER_BEACON_FRESHNESS_WINDOW_MS
                    >= 2 * COMMITTER_BEACON_INTERVAL_MS + COMMITTER_BEACON_WORST_CASE_POLL_MS,
            );
        }
        let healthy_but_lost_one = 2 * COMMITTER_BEACON_INTERVAL_MS;
        assert_eq!(
            verdict(Some(2), &[obs(1, healthy_but_lost_one)], WINDOW),
            defer(1),
            "one lost beacon must not evict a live candidate",
        );
    }

    // -------------------------------------------------------------------
    // The fold — rule 1: a non-candidate never sweeps
    // -------------------------------------------------------------------

    #[test]
    fn a_plain_member_is_never_a_candidate() {
        // Even with a wide-open field: no beacons at all, which for a
        // candidate would mean Act.
        assert_eq!(verdict(None, &[], WINDOW), ActingCommitter::NotACandidate,);
        assert_eq!(
            verdict(None, &[obs(0, 0), obs(1, 0)], WINDOW),
            ActingCommitter::NotACandidate,
        );
    }

    // -------------------------------------------------------------------
    // The fold — the owner never waits for a higher rank
    // -------------------------------------------------------------------

    #[test]
    fn the_owner_never_waits_for_a_higher_rank() {
        // Rank 0 against a field of fresh admins and fresh higher-keyed
        // rank-0 devices: nothing is better placed, so Act.
        assert_eq!(verdict(Some(0), &[], WINDOW), ActingCommitter::Act);
        assert_eq!(
            verdict(Some(0), &[obs(0, 0), obs(1, 0), obs(2, 0)], WINDOW),
            ActingCommitter::Act,
        );
    }

    #[test]
    fn an_owner_device_defers_to_a_lower_keyed_device_of_the_owner() {
        // ADR-0050 section 2.5a item 6: every device of the owner's
        // account holds rank 0; the lowest-keyed active one acts.
        assert_eq!(
            verdict(Some(0), &[obs_k(0, 0, LOWER)], WINDOW),
            ActingCommitter::Defer {
                to_rank: 0,
                to_signature_key: LOWER,
            },
        );
    }

    // -------------------------------------------------------------------
    // The fold — rule 6: hearing nothing means acting
    // -------------------------------------------------------------------

    #[test]
    fn a_candidate_that_hears_nothing_is_the_acting_committer() {
        // The single most misreadable rule in the module: silence is an
        // answer, not a failure to elect.
        assert_eq!(verdict(Some(3), &[], WINDOW), ActingCommitter::Act,);
    }

    #[test]
    fn a_candidate_that_hears_only_stale_beacons_acts() {
        // The owner and every admin below this device went quiet: this
        // is the takeover case, and it must land on Act.
        let stale = WINDOW + 1;
        assert_eq!(
            verdict(
                Some(3),
                &[obs(0, stale), obs(1, stale), obs(2, stale)],
                WINDOW
            ),
            ActingCommitter::Act,
        );
    }

    // -------------------------------------------------------------------
    // The fold — freshness and strict ordering
    // -------------------------------------------------------------------

    #[test]
    fn a_fresh_lower_ranked_beacon_defers_this_device() {
        assert_eq!(verdict(Some(2), &[obs(0, 0)], WINDOW), defer(0),);
    }

    #[test]
    fn the_freshness_boundary_is_inclusive_and_discriminates() {
        // Exactly at the window: still fresh, so defer.
        assert_eq!(verdict(Some(2), &[obs(1, WINDOW)], WINDOW), defer(1),);
        // One millisecond past: stale, so act. The pair is what makes
        // this a boundary test rather than two independent assertions —
        // an off-by-one in the comparison flips exactly one of them.
        assert_eq!(
            verdict(Some(2), &[obs(1, WINDOW + 1)], WINDOW),
            ActingCommitter::Act,
        );
    }

    #[test]
    fn a_higher_ranked_beacon_never_defers_this_device() {
        // An admin below us in the order is running. We still outrank it,
        // so we act and it defers to us.
        assert_eq!(
            verdict(Some(1), &[obs(2, 0), obs(5, 0)], WINDOW),
            ActingCommitter::Act,
        );
    }

    #[test]
    fn an_equal_ranked_beacon_with_a_higher_key_never_defers_this_device() {
        // Another device of this account (or of an account sharing the
        // rank — impossible today, ranks are a permutation over accounts)
        // with a higher key: this device is the better placed of the two.
        assert_eq!(verdict(Some(2), &[obs(2, 0)], WINDOW), ActingCommitter::Act);
    }

    #[test]
    fn an_equal_ranked_beacon_with_a_lower_key_defers_this_device() {
        // The new tie-break: the account's lowest-keyed ACTIVE device acts.
        assert_eq!(
            verdict(Some(2), &[obs_k(2, 0, LOWER)], WINDOW),
            ActingCommitter::Defer {
                to_rank: 2,
                to_signature_key: LOWER,
            },
        );
    }

    #[test]
    fn a_stale_equal_ranked_beacon_with_a_lower_key_is_ignored() {
        // The lost device: still in the roster, sorts lower, but silent.
        // Its last beacon ages out and this device takes over — the whole
        // point of choosing among ACTIVE devices.
        assert_eq!(
            verdict(Some(2), &[obs_k(2, WINDOW + 1, LOWER)], WINDOW),
            ActingCommitter::Act,
        );
        // At the window edge it is still fresh.
        assert_eq!(
            verdict(Some(2), &[obs_k(2, WINDOW, LOWER)], WINDOW),
            ActingCommitter::Defer {
                to_rank: 2,
                to_signature_key: LOWER,
            },
        );
    }

    #[test]
    fn this_devices_own_beacon_is_ignored_whatever_its_rank() {
        // A device observing its OWN beacon echoed back off the journal
        // must not stand down for itself, or a lone candidate would
        // deadlock the group forever. Ignored even under a lower rank
        // (a rank taken under a stale table cannot defer it to itself).
        assert_eq!(
            verdict(Some(2), &[obs_k(2, 0, OWN)], WINDOW),
            ActingCommitter::Act
        );
        assert_eq!(
            verdict(Some(2), &[obs_k(0, 0, OWN)], WINDOW),
            ActingCommitter::Act
        );
        assert_eq!(
            verdict(Some(0), &[obs_k(0, 0, OWN)], WINDOW),
            ActingCommitter::Act
        );
    }

    #[test]
    fn defer_names_the_best_placed_observation_by_rank_then_key() {
        // A lower rank beats a lower key; among equal ranks the lower key
        // is named.
        assert_eq!(
            verdict(
                Some(3),
                &[
                    obs_k(3, 0, LOWER),
                    obs_k(1, 0, HIGHER),
                    obs_k(1, 0, [0x50; 32])
                ],
                WINDOW
            ),
            ActingCommitter::Defer {
                to_rank: 1,
                to_signature_key: [0x50; 32],
            },
        );
    }

    #[test]
    fn defer_reports_the_lowest_active_rank() {
        // Several better-placed candidates are live; the verdict names
        // the best of them, not merely the first seen. Deliberately
        // supplied out of order.
        assert_eq!(
            verdict(Some(9), &[obs(4, 0), obs(1, 10), obs(7, 0)], WINDOW),
            defer(1),
        );
    }

    #[test]
    fn a_stale_better_candidate_yields_to_a_fresh_worse_one() {
        // Rank 1 is fresh, rank 0 went quiet. We are rank 5, so we still
        // defer — but to 1, not to 0. This discriminates a filter that
        // computes the minimum BEFORE applying freshness.
        assert_eq!(
            verdict(Some(5), &[obs(0, WINDOW + 1), obs(1, 0)], WINDOW),
            defer(1),
        );
    }

    // -------------------------------------------------------------------
    // The fold — rule 2: non-candidate beacons are ignored
    // -------------------------------------------------------------------

    #[test]
    fn a_beacon_from_a_non_candidate_is_ignored() {
        // A plain member (or a sender whose leaf no longer resolves)
        // emitted a beacon it should not have. It carries no rank, so it
        // cannot displace anyone. This is the test that catches an
        // implementation defaulting an absent rank to 0 — which would
        // hand a plain member a veto over every sweep in the group.
        assert_eq!(
            verdict(Some(3), &[non_candidate(0)], WINDOW),
            ActingCommitter::Act,
        );
        assert_eq!(
            verdict(
                Some(3),
                &[non_candidate(0), non_candidate(0), obs(4, 0)],
                WINDOW,
            ),
            ActingCommitter::Act,
        );
    }

    // -------------------------------------------------------------------
    // The fold — totality
    // -------------------------------------------------------------------

    #[test]
    fn the_fold_is_total_at_the_numeric_extremes() {
        // No panic, no overflow, no surprise: u32::MAX ranks, u64::MAX
        // ages and a zero-width window are all ordinary inputs.
        assert_eq!(
            verdict(Some(u32::MAX), &[obs(0, u64::MAX)], u64::MAX),
            defer(0),
        );
        assert_eq!(
            verdict(Some(u32::MAX), &[obs(0, 1)], 0),
            ActingCommitter::Act,
            "a zero window makes every beacon but an age-0 one stale",
        );
        assert_eq!(
            verdict(Some(1), &[obs(0, 0)], 0),
            defer(0),
            "an age-0 beacon is fresh even in a zero-width window",
        );
    }

    #[test]
    fn is_act_agrees_with_the_variant() {
        assert!(ActingCommitter::Act.is_act());
        assert!(!defer(0).is_act());
        assert!(!ActingCommitter::NotACandidate.is_act());
    }

    // -------------------------------------------------------------------
    // The send-side throttle
    // -------------------------------------------------------------------

    #[test]
    fn a_non_candidate_never_emits() {
        // Not even the first-of-session beacon: a plain member's beacon
        // is pure litter and pure leak, since the fold drops it anyway.
        assert!(!should_emit_beacon(None, None, 0));
        assert!(!should_emit_beacon(None, Some(0), u64::MAX));
    }

    #[test]
    fn the_first_beacon_of_a_session_is_immediate() {
        assert!(should_emit_beacon(Some(0), None, 0));
        assert!(should_emit_beacon(Some(7), None, 12_345));
    }

    #[test]
    fn the_emit_interval_boundary_discriminates() {
        let last = 1_000_000u64;
        assert!(
            !should_emit_beacon(Some(1), Some(last), last + COMMITTER_BEACON_INTERVAL_MS - 1),
            "one millisecond early must not emit",
        );
        assert!(
            should_emit_beacon(Some(1), Some(last), last + COMMITTER_BEACON_INTERVAL_MS),
            "exactly at the interval must emit",
        );
    }

    #[test]
    fn a_backward_clock_jump_suppresses_rather_than_spams() {
        // saturating_sub yields 0, which is below the interval, so the
        // beacon is skipped and self-heals on the next tick. The unsafe
        // direction would be wrapping to a huge elapsed time and
        // emitting on every single tick.
        assert!(!should_emit_beacon(Some(1), Some(u64::MAX), 0));
        assert!(!should_emit_beacon(Some(1), Some(10_000), 9_999));
    }

    #[test]
    fn the_throttle_and_the_window_stay_in_one_budget() {
        // The two constants are halves of one budget: an emit cadence
        // slower than the window would make a healthy candidate read as
        // absent to everyone, permanently.
        const {
            assert!(COMMITTER_BEACON_INTERVAL_MS < COMMITTER_BEACON_FRESHNESS_WINDOW_MS);
        }
    }
}

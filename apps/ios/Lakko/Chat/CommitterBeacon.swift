// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0045 S4b — the committer liveness beacon: emit it, observe it, and let
/// the lowest-ranked *active* candidate take the sweep. iOS mirror of the web
/// reference `apps/web/src/chat/committer-beacon.ts`.
///
/// ## What this closes
///
/// S4a made admins propose and the **owner** sweep. Rank 0 acts without
/// consulting anything, so an admin-only group — an owner who has not opened
/// the app since the request landed — simply waits, with no deadline. That is
/// the designed behaviour, not a bug, but it is also the whole of the waiting:
/// every candidate is willing and none of them knows it is allowed.
///
/// This slice supplies the missing fact. A candidate announces itself on the
/// group journal; every other candidate hears it, ranks its sender, and stands
/// down only for someone **strictly** better placed. Hearing nothing means
/// *act* — silence is the answer that nobody better placed is running, not a
/// failure to elect.
///
/// ## The rules live in the core; this module supplies what the core cannot see
///
/// `shouldEmitBeacon` and `actingCommitterVerdict` are pure, total and
/// clock-free, and both are consumed over the FFI rather than reimplemented
/// here — three clients each hand-rolling one rule is exactly the drift class
/// that produced ADR-0044's F-A replay. What this module owns is the three
/// things the core deliberately cannot supply:
///
/// 1. **WHO** — the beacon envelope is field-less, so the sender comes from the
///    MLS-authenticated sender leaf, resolved through `members()` to a
///    signature key and ranked by `Group.committerRank`.
/// 2. **HOW OLD** — from the DS row's server-assigned `created_at`, which every
///    member reads identically. The payload carries no timestamp precisely so a
///    candidate cannot assert its own freshness: that would be a liveness claim
///    no peer can check, i.e. a duty nobody could take away.
/// 3. **FOREGROUNDEDNESS** — a caller obligation the core cannot check. See
///    ``beaconEmitVerdict(ownRank:foreground:lastEmittedAtMs:nowMs:throttle:)``.
///
/// ## Rank at evaluation time, never at receipt
///
/// A rank is a **position** in the role table's canonical order (`0` = owner,
/// `k + 1` = the k-th admin), not an identity. Promoting, demoting or removing
/// any admin renumbers every admin sorting after it, so a rank captured when a
/// beacon arrived and compared later against this device's own rank — which is
/// necessarily current — is not merely stale: it is a position in a **different
/// ordering**, and the comparison silently stops meaning anything.
///
/// So ``BeaconObservationStore/record(accountIdB64:groupIdB64:senderKeyB64:atMs:)``
/// persists the sender's 32-byte signature **key** and the row timestamp, and
/// ``projectObservations(_:rankOf:nowMs:)`` ranks the key at the moment the
/// verdict is taken. The key rather than the leaf index for the second half of
/// the same reason: OpenMLS reuses a vacated leaf for the next member added, so
/// a persisted leaf later resolves to somebody else.
///
/// ## This does NOT close the race
///
/// Each candidate folds over its own poll-timed view, and a beacon is stale by
/// up to one interval even when its sender is perfectly healthy, so two
/// candidates can both reach `act`. ADR-0045's stand-down journal re-read and
/// its staged-commit rollback both remain required; `CommitterSweep` still runs
/// them unchanged, and reading this verdict as a licence to delete either would
/// be a mis-reading. A heartbeat removes the *wait*, not the *race*.

/// Why this device did or did not emit a beacon on this tick.
public enum BeaconEmitVerdict: String, Equatable {
    /// Emit — this device is a candidate, foregrounded, and past its throttle.
    case emit
    /// Not an owner or admin. A plain member's beacon can never make it the
    /// acting committer (every receiver's fold drops it), so emitting one is
    /// pure journal litter and a pure activity leak.
    case notACandidate
    /// The chat screen is not on-screen, or the app is not active. See
    /// ``beaconEmitVerdict(ownRank:foreground:lastEmittedAtMs:nowMs:throttle:)``.
    case notForeground
    /// A beacon went out less than one interval ago.
    case throttled
}

/// One projected observation, in the shape the FFI's `BeaconObservation` takes.
/// Both fields are the OBSERVER's own re-derivation — nothing in the beacon
/// payload contributes to either.
public struct ProjectedObservation: Equatable {
    /// The rank re-derived for this beacon's sender AT VERDICT TIME, or `nil`
    /// when that sender is not a candidate (a plain member, a demoted admin, or
    /// a key that no longer resolves).
    public var rank: UInt32?
    /// `now - created_at` on the DS row, in milliseconds. Never negative.
    public var ageMs: UInt64

    public init(rank: UInt32?, ageMs: UInt64) {
        self.rank = rank
        self.ageMs = ageMs
    }
}

/// How often this client's emitter WAKES.
///
/// **Not the beacon interval** — that is the core's `committerBeaconIntervalMs`,
/// enforced by `shouldEmitBeacon`, and is never hand-copied. This only bounds
/// how long after a chat screen becomes visible the first beacon waits, so it
/// has to be comfortably inside the interval and is otherwise free to be
/// coarse: each tick derives this device's own rank, which costs an Ed25519 key
/// derivation.
public let beaconTickSeconds: TimeInterval = 15

/// The send-side gate, with the one rule the core cannot check bolted on.
///
/// **The foreground gate is not cosmetic.** A beacon means *able to sweep*, and
/// on this client a sweep runs only at the tail of a poll driven by a visible
/// `ChatScreen`. The ADR-0041 NotificationServiceExtension decrypt is
/// read-only-throwaway and structurally cannot commit (**C-59**) — and it links
/// a slim FFI that deliberately does not even export `composeCommitterBeacon` —
/// so a beacon emitted from a backgrounded app would be a lie that costs the
/// group a takeover: every other candidate would defer to a device that is not
/// going to sweep, and would keep doing so for as long as it kept beaconing.
///
/// The gate runs FIRST and the throttle decides everything else, so the
/// candidate rule and the interval rule stay the core's — the `notACandidate` /
/// `throttled` split below is only a label for the log line, derived after the
/// core has already said no.
public func beaconEmitVerdict(
    ownRank: UInt32?,
    foreground: Bool,
    lastEmittedAtMs: UInt64?,
    nowMs: UInt64,
    throttle: (UInt32?, UInt64?, UInt64) -> Bool
) -> BeaconEmitVerdict {
    if !foreground { return .notForeground }
    if throttle(ownRank, lastEmittedAtMs, nowMs) { return .emit }
    return ownRank == nil ? .notACandidate : .throttled
}

/// Project the observation store into the array the fold consumes, ranking
/// every sender AGAINST THE CURRENT ROLE TABLE.
///
/// Returns the live keys alongside, so the caller can prune in the same pass:
/// an entry whose sender no longer ranks is a member who was demoted, removed,
/// or whose key no longer resolves, and it will never contribute again.
///
/// A row that appears to come from the FUTURE is clamped to age `0` rather than
/// wrapping. That is the safe direction as the FFI states it — an age that
/// reads too fresh makes this device defer (it waits), while one that reads too
/// stale makes it act (it may double-sweep). `ageMs` is a `UInt64` on the ABI,
/// and a negative one would not merely be wrong but would sit *above* every
/// freshness window forever once wrapped.
public func projectObservations(
    _ observations: [String: Int64],
    rankOf: (String) -> UInt32?,
    nowMs: Int64
) -> (projected: [ProjectedObservation], liveKeys: [String]) {
    var projected: [ProjectedObservation] = []
    var liveKeys: [String] = []
    // Sorted so the projection — and therefore any log line built from it — is
    // deterministic; the fold itself is order-independent.
    for senderKeyB64 in observations.keys.sorted() {
        guard let atMs = observations[senderKeyB64] else { continue }
        // RULE 1: ranked HERE, at verdict time, never at receipt.
        guard let rank = rankOf(senderKeyB64) else { continue }
        liveKeys.append(senderKeyB64)
        let age = nowMs - atMs
        projected.append(ProjectedObservation(rank: rank, ageMs: age < 0 ? 0 : UInt64(age)))
    }
    return (projected, liveKeys)
}

// MARK: - The observation store

/// Per-`(account, group)` record of `senderSignatureKeyB64 -> DS row ms`.
///
/// `UserDefaults`-backed, matching `PeerPinStore` and `PushRoutingSecretStore`:
/// a beacon observation is a public artefact about *who had the app open*, not
/// key material, and losing the store costs a takeover's promptness rather than
/// its correctness.
public final class BeaconObservationStore {

    private static let prefix = "lakko.committer-beacons.v1"

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    private func storeKey(accountIdB64: String, groupIdB64: String) -> String {
        "\(Self.prefix).\(accountIdB64).\(groupIdB64)"
    }

    /// Read this group's observations.
    ///
    /// Never throws: a corrupt or absent row reads as empty, which degrades to
    /// "heard nothing" — and hearing nothing means *act*. That is the correct
    /// direction for a store failure: the sweep's stand-down and rollback
    /// already handle two candidates acting, whereas a store failure that made
    /// this device defer would make it wait on evidence it does not have.
    public func load(accountIdB64: String, groupIdB64: String) -> [String: Int64] {
        guard let raw = defaults.dictionary(
            forKey: storeKey(accountIdB64: accountIdB64, groupIdB64: groupIdB64)
        ) else {
            return [:]
        }
        var out: [String: Int64] = [:]
        for (key, value) in raw {
            if let n = value as? NSNumber { out[key] = n.int64Value }
        }
        return out
    }

    private func save(accountIdB64: String, groupIdB64: String, _ observations: [String: Int64]) {
        var raw: [String: NSNumber] = [:]
        for (key, value) in observations { raw[key] = NSNumber(value: value) }
        defaults.set(raw, forKey: storeKey(accountIdB64: accountIdB64, groupIdB64: groupIdB64))
    }

    /// Record that a beacon landed.
    ///
    /// **The NEWEST timestamp wins** — the opposite of `PendingRequests`, where
    /// the first observation of a request wins so a re-broadcast cannot reset
    /// its age. Here recency IS the signal: the whole question is *how long
    /// since this candidate last proved it was running*, so an older stamp must
    /// never overwrite a newer one (a re-fetched journal batch would otherwise
    /// age a live candidate out and hand its duty away).
    ///
    /// A row whose timestamp does not parse is DROPPED by the caller rather
    /// than stored. There is no age to measure, so it is no evidence of
    /// liveness; storing it as "fresh" would defer this device forever with
    /// nothing to age out, and storing it as "stale" is what dropping it
    /// already achieves.
    ///
    /// The store is naturally bounded by the candidate count (one entry per
    /// signature key, and ``projectObservations(_:rankOf:nowMs:)`` prunes
    /// anything that no longer ranks), so it needs no age-based GC of its own.
    public func record(
        accountIdB64: String,
        groupIdB64: String,
        senderKeyB64: String,
        atMs: Int64
    ) {
        var observations = load(accountIdB64: accountIdB64, groupIdB64: groupIdB64)
        if let existing = observations[senderKeyB64], existing >= atMs { return }
        observations[senderKeyB64] = atMs
        save(accountIdB64: accountIdB64, groupIdB64: groupIdB64, observations)
    }

    /// Drop every observation whose sender no longer ranks — see
    /// ``projectObservations(_:rankOf:nowMs:)``, which supplies `liveKeys` from
    /// the same pass.
    public func prune(accountIdB64: String, groupIdB64: String, liveKeys: [String]) {
        var observations = load(accountIdB64: accountIdB64, groupIdB64: groupIdB64)
        let live = Set(liveKeys)
        let before = observations.count
        observations = observations.filter { live.contains($0.key) }
        if observations.count != before {
            save(accountIdB64: accountIdB64, groupIdB64: groupIdB64, observations)
        }
    }

    /// Drop every observation for one account — `recover` / `forgetDevice`
    /// hygiene, the same discipline the commit outbox follows.
    public func clearForAccount(accountIdB64: String) {
        let needle = "\(Self.prefix).\(accountIdB64)."
        for key in defaults.dictionaryRepresentation().keys where key.hasPrefix(needle) {
            defaults.removeObject(forKey: key)
        }
    }
}

// MARK: - The composed decision

/// The whole receive-side decision, composed: read the store, rank every sender
/// against the CURRENT role table, prune what no longer ranks, and hand the
/// projection to the core's fold.
///
/// `fold` is the FFI `actingCommitterVerdict` behind a thin adapter, injected so
/// this composition is testable without a live `Group`.
@MainActor
public func decideActingCommitter(
    store: BeaconObservationStore,
    accountIdB64: String,
    groupIdB64: String,
    ownRank: UInt32?,
    rankOf: (String) -> UInt32?,
    nowMs: Int64,
    freshnessWindowMs: UInt64,
    fold: (UInt32?, [ProjectedObservation], UInt64) -> ActingCommitter
) -> ActingCommitter {
    let observations = store.load(accountIdB64: accountIdB64, groupIdB64: groupIdB64)
    let (projected, liveKeys) = projectObservations(observations, rankOf: rankOf, nowMs: nowMs)
    store.prune(accountIdB64: accountIdB64, groupIdB64: groupIdB64, liveKeys: liveKeys)
    return fold(ownRank, projected, freshnessWindowMs)
}

/// The production `fold` — the core's own rule, called over the FFI so no client
/// re-implements it.
public func coreActingCommitterFold(
    ownRank: UInt32?,
    observed: [ProjectedObservation],
    freshnessWindowMs: UInt64
) -> ActingCommitter {
    actingCommitterVerdict(
        ownRank: ownRank,
        observed: observed.map { BeaconObservation(rank: $0.rank, ageMs: $0.ageMs) },
        freshnessWindowMs: freshnessWindowMs
    )
}

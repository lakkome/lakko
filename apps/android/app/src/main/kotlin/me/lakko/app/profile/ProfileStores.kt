// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.profile

/**
 * ADR-0048 §3 — the per-signature-key profile store and the per
 * `(key, username)` verification cache.
 *
 * ## Keyed on the signature key, never a leaf
 *
 * A profile received in a group is stored under the sender's Ed25519
 * signature key resolved from the MLS sender leaf AT RECEIPT (C-68) — the
 * caller hands in that key, never a leaf index and never a key the envelope
 * might name (the envelope names none). A profile from a contact request is
 * stored under `ContactRequest.from_pubkey`, which the request signature
 * authenticates. The key string is standard base64, the one spelling
 * [me.lakko.app.chat.MemberIdentity] and the accepted-contact store use.
 *
 * ## Strictly newer only
 *
 * A claim replaces the stored one only when the core's `profileIsNewer` says
 * so — strictly greater `updated_at_ms`; equal is not newer, so a re-broadcast
 * (every group republishes on growth) is a no-op rather than a write. The
 * order is injected so a host test runs without the `.so`.
 *
 * Both stores are pure over a [ProfilePersistence] port, so the ordering and
 * keying rules are testable in a plain JVM.
 */

/** Durable backing for the two stores. Reads return an empty map on any
 *  decode failure; writes are best-effort. */
public interface ProfilePersistence {
    public fun loadProfiles(): Map<String, PeerProfile>
    public fun saveProfiles(profiles: Map<String, PeerProfile>)
    public fun loadVerifications(): Map<String, UsernameVerification>
    public fun saveVerifications(verifications: Map<String, UsernameVerification>)
}

/** A [ProfilePersistence] that keeps nothing (tests, and a missing Context). */
public class InMemoryProfilePersistence : ProfilePersistence {
    private var profiles: Map<String, PeerProfile> = emptyMap()
    private var verifications: Map<String, UsernameVerification> = emptyMap()

    override fun loadProfiles(): Map<String, PeerProfile> = profiles
    override fun saveProfiles(profiles: Map<String, PeerProfile>) {
        this.profiles = profiles
    }
    override fun loadVerifications(): Map<String, UsernameVerification> = verifications
    override fun saveVerifications(verifications: Map<String, UsernameVerification>) {
        this.verifications = verifications
    }
}

/** Outcome of offering a received profile to [PeerProfileStore.offer]. */
public enum class ProfileOffer {
    /** Stored: no profile was held for the key, or this one is strictly newer. */
    STORED,

    /** Dropped: the held profile is as new or newer. */
    NOT_NEWER,

    /** Dropped: no signature key to bind it to (C-68 — never stored unbound). */
    UNATTRIBUTED,
}

/** The per-signature-key profile store. Thread-safe. */
public class PeerProfileStore(
    private val persistence: ProfilePersistence,
    private val isNewer: (incoming: ULong, stored: ULong) -> Boolean =
        { incoming, stored -> uniffi.lakko_ffi_uniffi.profileIsNewer(incoming, stored) },
) {
    private val lock = Any()
    private var profiles: Map<String, PeerProfile> = persistence.loadProfiles()

    /** The profile held for [signatureKeyB64], or `null`. */
    public fun get(signatureKeyB64: String): PeerProfile? =
        synchronized(lock) { profiles[signatureKeyB64] }

    /** Every held profile (a snapshot). */
    public fun all(): Map<String, PeerProfile> = synchronized(lock) { profiles }

    /** Signature key -> account id hex, for every held profile that has one
     *  (the ADR-0021 call-roster sources). */
    public fun accountIdsByKey(): Map<String, String> = synchronized(lock) {
        profiles.mapNotNull { (key, p) -> p.accountIdHex?.let { key to it } }.toMap()
    }

    /**
     * Offer a received [profile] from [signatureKeyB64]. Stores it only when
     * nothing is held or it is strictly newer than what is.
     *
     * A newer profile that carries no account id keeps the id already held for
     * the key: the id names the account, which a rename does not change, and
     * dropping it would take the person's name off their call tile.
     */
    public fun offer(signatureKeyB64: String?, profile: PeerProfile): ProfileOffer {
        if (signatureKeyB64.isNullOrEmpty()) return ProfileOffer.UNATTRIBUTED
        synchronized(lock) {
            val held = profiles[signatureKeyB64]
            if (held != null && !isNewer(profile.updatedAtMs, held.updatedAtMs)) {
                // The same profile sent again (a growth or request answer
                // carries the stamp it was set at) is not newer, but it can
                // carry an account id the held row never had: a row filed
                // before the envelope carried one. Take the id and nothing
                // else, or the call roster can never name a person whose
                // profile predates the field.
                val learned = profile.accountIdHex
                if (held.accountIdHex != null || learned == null ||
                    held.updatedAtMs != profile.updatedAtMs
                ) {
                    return ProfileOffer.NOT_NEWER
                }
                profiles = profiles + (signatureKeyB64 to held.copy(accountIdHex = learned))
                persistence.saveProfiles(profiles)
                return ProfileOffer.STORED
            }
            val merged = if (profile.accountIdHex == null && held?.accountIdHex != null) {
                profile.copy(accountIdHex = held.accountIdHex)
            } else {
                profile
            }
            profiles = profiles + (signatureKeyB64 to merged)
            persistence.saveProfiles(profiles)
            return ProfileOffer.STORED
        }
    }

    /** Drop everything (forget device). */
    public fun clear() {
        synchronized(lock) {
            profiles = emptyMap()
            persistence.saveProfiles(profiles)
        }
    }
}

/** The per `(signature key, username)` verification cache. Thread-safe. */
public class UsernameVerificationStore(private val persistence: ProfilePersistence) {
    private val lock = Any()
    private var results: Map<String, UsernameVerification> = persistence.loadVerifications()

    /** The cached result for the pair, or `null` when never verified. */
    public fun get(signatureKeyB64: String, username: String): UsernameVerification? =
        synchronized(lock) { results[pairKey(signatureKeyB64, username)] }

    /** Record a definitive result for the pair. */
    public fun put(signatureKeyB64: String, username: String, result: UsernameVerification) {
        synchronized(lock) {
            results = results + (pairKey(signatureKeyB64, username) to result)
            persistence.saveVerifications(results)
        }
    }

    /** Drop everything (forget device). */
    public fun clear() {
        synchronized(lock) {
            results = emptyMap()
            persistence.saveVerifications(results)
        }
    }

    public companion object {
        /** The cache key. `|` is outside both the base64 alphabet and the
         *  username character set, so the pair cannot be ambiguous. */
        public fun pairKey(signatureKeyB64: String, username: String): String =
            "$signatureKeyB64|$username"
    }
}

// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.profile

import android.content.Context
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.serialization.Serializable
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import java.util.Base64

/**
 * ADR-0048 — the one place names are learned, verified and resolved.
 *
 * Holds the per-key [PeerProfileStore], the per-pair
 * [UsernameVerificationStore], the viewer's local renames, and the
 * [UsernameVerifier]; exposes [resolve] (the UX §3 precedence) and a [version]
 * flow every screen collects so a name that arrives or verifies updates in
 * place.
 *
 * ## Foreground only
 *
 * [onProfileReceived] writes and may start a discovery lookup, so it is called
 * only from the foreground receive paths (the chat poll, the sweep's apply
 * pass, the contact-inbox poll). The push background-decrypt path does NOT use
 * this class: it reads the persisted stores through [ProfileNameSnapshot],
 * which has no write path and no network (C-59).
 *
 * One process-wide instance ([get]) because the chat repository, the contacts
 * repository and every screen must agree on the same names.
 */
public class ProfileDirectory internal constructor(
    private val persistence: ProfilePersistence,
    private val scope: CoroutineScope,
    /** Where the retired device-wide rows live, removed on forget; `null`
     *  in host tests. */
    private val persistenceContext: Context? = null,
    private val isNewer: (ULong, ULong) -> Boolean =
        { incoming, stored -> uniffi.lakko_ffi_uniffi.profileIsNewer(incoming, stored) },
    passesRule: ((String) -> Boolean)? = null,
    initialRenames: Map<String, String> = emptyMap(),
) {
    public val profiles: PeerProfileStore = PeerProfileStore(persistence, isNewer)
    public val verifications: UsernameVerificationStore = UsernameVerificationStore(persistence)

    @Volatile
    private var resolver: (suspend (String) -> UsernameResolution)? = null

    private val verifier: UsernameVerifier = if (passesRule != null) {
        UsernameVerifier(verifications, ::resolveViaAttached, passesRule)
    } else {
        UsernameVerifier(verifications, ::resolveViaAttached)
    }

    @Volatile
    private var renames: Map<String, String> = initialRenames

    /** ADR-0073 phase 2 / UX 16.8 — a flagged contact's NEW key, aliased to
     *  the key the person was known by, so a bubble from the new keys names
     *  the person rather than `Member`. Standard base64 on both sides. */
    @Volatile
    private var aliases: Map<String, String> = emptyMap()

    private val mutableVersion = MutableStateFlow(0L)

    /** Bumped on every change that can alter a resolved name. */
    public val version: StateFlow<Long> = mutableVersion.asStateFlow()

    private suspend fun resolveViaAttached(username: String, signatureKeyB64: String): UsernameResolution {
        val certify = certificateVerifier
        if (certify != null) {
            // ADR-0060 §5 — the username is shown only on its certificate. A
            // profile without one (or without the account id it binds) is not
            // a refusal: it is "not yet", so nothing is cached as failed and
            // a later profile carrying the certificate verifies.
            val profile = profiles.get(signatureKeyB64)
                ?.takeIf { it.username == username } ?: return UsernameResolution.Unavailable(null)
            val cert = profile.usernameCertificateHex ?: return UsernameResolution.Unavailable(null)
            val account = profile.accountIdHex ?: return UsernameResolution.Unavailable(null)
            return certify(username, cert, account, signatureKeyB64)
        }
        return resolver?.invoke(username) ?: UsernameResolution.Unavailable(null)
    }

    @Volatile
    private var certificateVerifier: (
        suspend (username: String, certificateHex: String, accountIdHex: String, signatureKeyB64: String) ->
        UsernameResolution
    )? = null

    /**
     * ADR-0060 §5 — verify a claimed username by its certificate (one OPRF
     * evaluation plus the certificate check, and the account's published key
     * compared with the sender's) instead of a public lookup, which a person
     * who is not in Everyone no longer answers. Replaces the lookup resolver.
     */
    public fun attachCertificateVerifier(
        verify: suspend (username: String, certificateHex: String, accountIdHex: String, signatureKeyB64: String) ->
        UsernameResolution,
    ) {
        certificateVerifier = verify
        verifyPending()
    }

    /**
     * Wire the discovery resolve path (the contacts repository's plain
     * lookup -> bundle -> key resolve, which writes no pin) and verify every
     * stored claim that is not yet verified.
     */
    public fun attachUsernameResolver(resolve: suspend (String) -> UsernameResolution) {
        resolver = resolve
        verifyPending()
    }

    /** Replace the viewer's local renames (signature key base64 -> name). */
    public fun setRenames(byKey: Map<String, String>) {
        if (byKey == renames) return
        renames = byKey
        bump()
    }

    /** ADR-0073 phase 2 — replace the key aliases (new key -> previous key).
     *  A key with a rename or a profile of its own is never aliased away. */
    public fun setAliases(byKey: Map<String, String>) {
        if (byKey == aliases) return
        aliases = byKey
        bump()
    }

    /**
     * Store a received profile bound to [signatureKeyB64] (resolved at receipt,
     * C-68) when strictly newer, then verify its username claim.
     */
    public fun onProfileReceived(signatureKeyB64: String?, profile: PeerProfile): ProfileOffer {
        val offer = profiles.offer(signatureKeyB64, profile)
        if (offer == ProfileOffer.STORED) {
            bump()
            val key = signatureKeyB64 ?: return offer
            profile.username?.let { verifyInBackground(key, it) }
        }
        return offer
    }

    /**
     * ADR-0050 section 2.9 (state bundle v3) — store a profile a pairing
     * bundle carried for [accountIdB64], under the same strictly-newer rule
     * as a received one. When that account is the one signed in it goes
     * through [onProfileReceived] (so screens re-read and the username is
     * verified); otherwise it is filed in that account's rows directly, which
     * the live store reads when the account binds.
     */
    public fun offerForAccount(accountIdB64: String, signatureKeyB64: String, profile: PeerProfile): ProfileOffer {
        val account = me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(accountIdB64)
        val bound = persistence.currentAccount()?.let { me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(it) }
        if (bound == account) return onProfileReceived(signatureKeyB64, profile)
        val fixed = object : ProfilePersistence by persistence {
            override fun currentAccount(): String = account
        }
        val offer = PeerProfileStore(fixed, isNewer).offer(signatureKeyB64, profile)
        if (offer == ProfileOffer.STORED) bump()
        return offer
    }

    /** Verify [signatureKeyB64]'s current claim if it has one and it is not
     *  yet verified (e.g. a contact detail opened while a lookup failed). */
    public fun ensureVerified(signatureKeyB64: String) {
        val username = profiles.get(signatureKeyB64)?.username ?: return
        if (verifications.get(signatureKeyB64, username) == null) {
            verifyInBackground(signatureKeyB64, username)
        }
    }

    private fun verifyPending() {
        for ((key, profile) in profiles.all()) {
            val username = profile.username ?: continue
            if (verifications.get(key, username) == null) verifyInBackground(key, username)
        }
    }

    /** Pairs a retry loop is already waiting on, so a burst of profile
     *  changes does not start several. */
    private val retrying: MutableSet<String> = java.util.Collections.synchronizedSet(HashSet())

    private fun verifyInBackground(signatureKeyB64: String, username: String) {
        if (resolver == null) return
        val pair = "$signatureKeyB64|$username"
        scope.launch {
            var failures = 0
            try {
                while (true) {
                    val outcome = verifier.verify(signatureKeyB64, username)
                    if (outcome != VerifyOutcome.UNAVAILABLE) {
                        bump()
                        return@launch
                    }
                    // A check that could not complete (a 429, the network) is
                    // tried again by itself; the name shows without the
                    // username meanwhile.
                    if (failures == 0 && !retrying.add(pair)) return@launch
                    failures += 1
                    kotlinx.coroutines.delay(usernameRetryDelayMs(failures))
                    if (profiles.get(signatureKeyB64)?.username != username) return@launch
                }
            } finally {
                if (failures > 0) retrying.remove(pair)
            }
        }
    }

    /** The UX §3 resolution for a signature key (standard base64). */
    public fun resolve(signatureKeyB64: String): ResolvedName {
        val keyBytes = try {
            Base64.getDecoder().decode(signatureKeyB64)
        } catch (_: IllegalArgumentException) {
            ByteArray(0)
        }
        val own = resolveName(
            localRename = renames[signatureKeyB64],
            profile = profiles.get(signatureKeyB64),
            verificationOf = { username -> verifications.get(signatureKeyB64, username) },
            signatureKey = keyBytes,
        )
        if (!own.isPlaceholder) return own
        // ADR-0073 phase 2 / UX 16.8 — nothing is known under the new key
        // yet: fall through to the person it belongs to, one level deep.
        val previous = aliases[signatureKeyB64] ?: return own
        val previousBytes = try {
            Base64.getDecoder().decode(previous)
        } catch (_: IllegalArgumentException) {
            return own
        }
        val inherited = resolveName(
            localRename = renames[previous],
            profile = profiles.get(previous),
            verificationOf = { username -> verifications.get(previous, username) },
            signatureKey = previousBytes,
        )
        return if (inherited.isPlaceholder) own else inherited
    }

    /** [resolve] for raw key bytes. */
    public fun resolve(signatureKey: ByteArray): ResolvedName =
        resolve(Base64.getEncoder().encodeToString(signatureKey))

    /**
     * The username the stored profile of [signatureKeyB64] CLAIMS, verified or
     * not, or `null`. Never shown with `@` unless [resolve] says it is
     * verified; a call tile shows it as plain text while it is not
     * (`me.lakko.app.calls.callTileName`).
     */
    public fun claimedUsernameOf(signatureKeyB64: String): String? =
        profiles.get(signatureKeyB64)?.username?.takeIf { it.isNotEmpty() }

    /**
     * Signature key (standard base64) -> account id hex for every stored
     * profile that carried one (ADR-0048 v2): the second source, beside the
     * accepted contacts, the group-call roster derives ADR-0021 peer tags from.
     * A profile arriving mid-call bumps [version], so a caller that rebuilds
     * on it names a tile already on screen.
     */
    public fun accountIdsByKey(): Map<String, String> = profiles.accountIdsByKey()

    /** Forget every profile and verification learned by [accountIdB64]
     *  (forget device). Other accounts' names stay. */
    public fun forgetAccount(accountIdB64: String) {
        val account = me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(accountIdB64)
        profiles.clear(account)
        verifications.clear(account)
        SharedPrefsProfilePersistence.deleteRetiredGlobalRows(persistenceContext)
        bump()
    }

    /**
     * Re-read the names for the account now signed in: a session landing or
     * ending changes which account's profiles resolve. The stores reload on
     * their next read by themselves; this bumps [version] so screens re-read.
     */
    public fun onAccountChanged() {
        bump()
    }

    private fun bump() {
        mutableVersion.update { it + 1 }
    }

    public companion object {
        @Volatile
        private var instance: ProfileDirectory? = null

        /**
         * Forget device: [forgetAccount] on the live directory, if one was
         * made, and [accountIdB64]'s rows on disk either way (with the retired
         * device-wide rows). The one caller is
         * [me.lakko.app.account.AccountLocalWipe].
         */
        public fun forgetAccountOnDevice(context: Context, accountIdB64: String) {
            instance?.forgetAccount(accountIdB64)
            val app = context.applicationContext
            val persistence = SharedPrefsProfilePersistence(app)
            persistence.saveProfiles(accountIdB64, emptyMap())
            persistence.saveVerifications(accountIdB64, emptyMap())
            SharedPrefsProfilePersistence.deleteRetiredGlobalRows(app)
        }

        /** The process-wide directory, created on first use. */
        public fun get(context: Context): ProfileDirectory {
            instance?.let { return it }
            return synchronized(this) {
                instance ?: ProfileDirectory(
                    persistence = SharedPrefsProfilePersistence(context.applicationContext),
                    scope = CoroutineScope(SupervisorJob() + Dispatchers.IO),
                    persistenceContext = context.applicationContext,
                    initialRenames = renamesFromAcceptedContacts(context.applicationContext),
                ).also { instance = it }
            }
        }

        /** Renames keyed by signature key, from [accountIdB64]'s accepted
         *  contacts, or the bound (signed-in) account's when `null`; empty
         *  when neither names an account. */
        internal fun renamesFromAcceptedContacts(
            context: Context,
            accountIdB64: String? = null,
        ): Map<String, String> =
            try {
                val contacts = if (accountIdB64 != null) {
                    me.lakko.app.contacts.AcceptedContactsStore.load(context, accountIdB64)
                } else {
                    me.lakko.app.contacts.AcceptedContactsStore.load(context)
                }
                contacts
                    .mapNotNull { c ->
                        val name = c.displayName?.trim()?.takeIf { it.isNotEmpty() }
                            ?: return@mapNotNull null
                        Base64.getEncoder().encodeToString(c.request.fromPubkey) to name
                    }
                    .toMap()
            } catch (_: Throwable) {
                emptyMap()
            }
    }
}

/**
 * READ-ONLY name lookup for the push background-decrypt path (C-59): loads the
 * persisted stores once and resolves, with no write, no verification and no
 * network. A name the app has not already learned resolves to the placeholder.
 */
public class ProfileNameSnapshot private constructor(
    private val profiles: Map<String, PeerProfile>,
    private val verifications: Map<String, UsernameVerification>,
    private val renames: Map<String, String>,
) {
    /** The resolved name for a signature key, standard base64. */
    public fun resolve(signatureKeyB64: String): ResolvedName {
        val keyBytes = try {
            Base64.getDecoder().decode(signatureKeyB64)
        } catch (_: IllegalArgumentException) {
            ByteArray(0)
        }
        return resolveName(
            localRename = renames[signatureKeyB64],
            profile = profiles[signatureKeyB64],
            verificationOf = { username ->
                verifications[UsernameVerificationStore.pairKey(signatureKeyB64, username)]
            },
            signatureKey = keyBytes,
        )
    }

    /** `true` when the viewer knows SOMETHING about this person (a rename, a
     *  profile name or a verified username). */
    public fun isKnown(signatureKeyB64: String): Boolean = !resolve(signatureKeyB64).isPlaceholder

    public companion object {
        /** Load a snapshot. Never writes. The profiles, verifications and
         *  contact renames are [accountIdB64]'s (the bound account's when
         *  `null`); with neither, the snapshot is empty. */
        public fun load(context: Context, accountIdB64: String? = null): ProfileNameSnapshot {
            val persistence = SharedPrefsProfilePersistence(context.applicationContext)
            val account = accountIdB64?.let { me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(it) }
                ?: persistence.currentAccount()
            return ProfileNameSnapshot(
                profiles = account?.let { persistence.loadProfiles(it) } ?: emptyMap(),
                verifications = account?.let { persistence.loadVerifications(it) } ?: emptyMap(),
                renames = ProfileDirectory.renamesFromAcceptedContacts(context.applicationContext, accountIdB64),
            )
        }
    }
}

/**
 * [ProfilePersistence] over plain `SharedPreferences` (JSON), one pair of rows
 * per account: `profiles.v2:<accountIdB64>` and `verifications.v2:<accountIdB64>`
 * (standard base64). The account is the one bound in
 * [me.lakko.app.contacts.AcceptedContactsStore] (the signed-in one). The
 * device-wide `profiles.v1` / `verifications.v1` rows of builds before
 * 2026-09-30 are never read; forget device removes them.
 */
internal class SharedPrefsProfilePersistence(private val context: Context) : ProfilePersistence {
    private val json = Json { ignoreUnknownKeys = true }

    @Serializable
    private data class StoredProfile(
        val displayName: String? = null,
        val username: String? = null,
        /** Stored as the unsigned value's bit pattern. */
        val updatedAtMs: Long = 0L,
        /** Absent on a row written before ADR-0048 v2; reads as `null`. */
        val accountIdHex: String? = null,
        /** ADR-0060 — the username certificate, hex; absent before v3. */
        val usernameCertificateHex: String? = null,
        /** ADR-0084 — the profile photo references (they carry the AEAD
         *  keys), sealed under the device's avatar key; absent before v4. */
        val avatarSealed: String? = null,
    )

    private fun prefs() = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    override fun currentAccount(): String? = me.lakko.app.contacts.AcceptedContactsStore.boundAccountB64

    override fun loadProfiles(account: String): Map<String, PeerProfile> {
        val raw = prefs().getString(profilesKey(account), null) ?: return emptyMap()
        return try {
            json.decodeFromString<Map<String, StoredProfile>>(raw).mapValues { (_, s) ->
                PeerProfile(
                    s.displayName,
                    s.username,
                    s.updatedAtMs.toULong(),
                    s.accountIdHex,
                    s.usernameCertificateHex,
                    me.lakko.app.avatar.openAvatarRefs(me.lakko.app.avatar.KeystoreAvatarSeal, s.avatarSealed),
                )
            }
        } catch (_: Exception) {
            emptyMap()
        }
    }

    override fun saveProfiles(account: String, profiles: Map<String, PeerProfile>) {
        if (profiles.isEmpty()) {
            prefs().edit().remove(profilesKey(account)).apply()
            return
        }
        val stored = profiles.mapValues { (_, p) ->
            StoredProfile(p.displayName, p.username, p.updatedAtMs.toLong(), p.accountIdHex, p.usernameCertificateHex,
                me.lakko.app.avatar.sealAvatarRefs(me.lakko.app.avatar.KeystoreAvatarSeal, p.avatar))
        }
        prefs().edit().putString(profilesKey(account), json.encodeToString(stored)).apply()
    }

    override fun loadVerifications(account: String): Map<String, UsernameVerification> {
        val raw = prefs().getString(verificationsKey(account), null) ?: return emptyMap()
        return try {
            json.decodeFromString<Map<String, String>>(raw).mapNotNull { (k, v) ->
                val result = UsernameVerification.entries.firstOrNull { it.name == v }
                    ?: return@mapNotNull null
                k to result
            }.toMap()
        } catch (_: Exception) {
            emptyMap()
        }
    }

    override fun saveVerifications(account: String, verifications: Map<String, UsernameVerification>) {
        if (verifications.isEmpty()) {
            prefs().edit().remove(verificationsKey(account)).apply()
            return
        }
        val stored = verifications.mapValues { (_, v) -> v.name }
        prefs().edit().putString(verificationsKey(account), json.encodeToString(stored)).apply()
    }

    companion object {
        const val PREFS = "lakko-peer-profiles"

        /** The rows one account's names live under. Visible for tests. */
        fun profilesKey(account: String): String =
            "profiles.v2:" + me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(account)

        fun verificationsKey(account: String): String =
            "verifications.v2:" + me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(account)

        /** The device-wide rows of builds before 2026-09-30. Never read. */
        private const val RETIRED_PROFILES = "profiles.v1"
        private const val RETIRED_VERIFICATIONS = "verifications.v1"

        /** Remove the retired device-wide rows, if a pre-2026-09-30 build left
         *  them. Best-effort; no-op without a [context]. */
        fun deleteRetiredGlobalRows(context: Context?) {
            val ctx = context ?: return
            try {
                ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit()
                    .remove(RETIRED_PROFILES)
                    .remove(RETIRED_VERIFICATIONS)
                    .apply()
            } catch (_: Throwable) {
                // best-effort
            }
        }
    }
}

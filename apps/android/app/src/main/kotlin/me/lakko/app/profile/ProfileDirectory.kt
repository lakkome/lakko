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

    /** ADR-0079 — what each pending contact's invite link states about the
     *  inviter (display name, username), keyed by the inviter's account key
     *  (standard base64): the profile input of a key nothing else names. */
    @Volatile
    private var pendingStatements: Map<String, PeerProfile> = emptyMap()

    /** The profile input for [signatureKeyB64]: the received profile, else
     *  the statement of a pending contact's link. A received profile always
     *  wins: it is the person's current statement. */
    private fun profileFor(signatureKeyB64: String): PeerProfile? =
        profiles.get(signatureKeyB64) ?: pendingStatements[signatureKeyB64]

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

    /**
     * ADR-0079 — replace the pending contacts whose link statements name a
     * key no rename and no received profile names (the precedence is local
     * rename > received profile > the link's statement > placeholder). Called
     * wherever the pending list changes, so a roster re-reads when a pending
     * contact is added, removed or handed over.
     *
     * The stated username is shown only when this `(key, username)` pair is
     * ALREADY verified (from a profile that carried a certificate). No check
     * is started for it: verification needs the discovery-signed certificate
     * a profile envelope carries (ADR-0060 section 5), and an invite link
     * carries none, so a check could only end "not yet" or, through the
     * lookup path, as a false "not verified". A link that states only a
     * username therefore stays the placeholder until a profile arrives.
     */
    public fun setPendingContacts(list: List<me.lakko.app.contacts.PendingContact>) {
        val next = pendingStatementsOf(list)
        if (next == pendingStatements) return
        pendingStatements = next
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
     * A person this account found by search answered its request, so they are
     * a contact before their profile has arrived: file what the Sent record
     * held at stamp 0, so the row is not a stand-in meanwhile
     * (`docs/ux/who-can-find-me.md` section 6.1). Any profile the person sends
     * that says something replaces it, whatever its stamp
     * ([PeerProfileStore.offer]), and one held already stays. It persists as
     * any profile does.
     *
     * [verifiedUsername] was verified when the person was found. That is
     * recorded with it: this entry carries no certificate, so a check could
     * never complete, and it would retry until the real profile came.
     */
    public fun onFoundPersonAnswered(
        signatureKeyB64: String,
        name: String?,
        verifiedUsername: String?,
    ): ProfileOffer {
        val stated = PeerProfile(displayName = name, username = verifiedUsername, updatedAtMs = 0uL)
        val offer = profiles.offer(signatureKeyB64, stated)
        if (offer == ProfileOffer.STORED) {
            verifiedUsername?.let { verifications.put(signatureKeyB64, it, UsernameVerification.VERIFIED) }
            bump()
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

    /**
     * ADR-0101 section 8 - the profile envelope held for [signatureKeyB64]
     * exactly as it was received, with its stamp, or `null` when none is held.
     */
    public fun heldEnvelopeOf(signatureKeyB64: String): HeldEnvelope? {
        val held = profiles.get(signatureKeyB64) ?: return null
        val b64 = held.envelopeB64 ?: return null
        return try {
            HeldEnvelope(java.util.Base64.getDecoder().decode(b64), held.updatedAtMs)
        } catch (_: IllegalArgumentException) {
            null
        }
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

    /**
     * ADR-0098 — whether accounts still exist, as this device knows it. `null`
     * until [attachDeletions] (and in host tests): nobody is deleted and a
     * verified username shows as before.
     */
    @Volatile
    public var deletions: AccountDeletionChecks? = null
        private set

    /** The signed-in account's id (hex), for [isOwnKey]; `null` when unknown. */
    @Volatile
    private var ownAccountIdHex: () -> String? = { null }

    /** Wire the ADR-0098 state. Re-reads every name. */
    public fun attachDeletions(checks: AccountDeletionChecks, ownAccountIdHex: () -> String?) {
        deletions = checks
        this.ownAccountIdHex = ownAccountIdHex
        bump()
    }

    /** Re-read every name (an ADR-0098 change). */
    public fun namesChanged() {
        bump()
    }

    /** Whether [signatureKeyB64] (or the key it is aliased to) is shown as
     *  Deleted account (ADR-0098). */
    public fun isDeleted(signatureKeyB64: String): Boolean {
        val book = deletions?.book ?: return false
        if (book.isDeleted(signatureKeyB64)) return true
        val previous = aliases[signatureKeyB64] ?: return false
        return book.isDeleted(previous)
    }

    /** [isDeleted] for raw key bytes. */
    public fun isDeleted(signatureKey: ByteArray): Boolean =
        isDeleted(Base64.getEncoder().encodeToString(signatureKey))

    /** ADR-0098 section 2 — [signatureKeyB64] is on screen: ask the directory
     *  about them when a check is due. */
    public fun noteOnScreen(signatureKeyB64: String) {
        deletions?.onScreen(signatureKeyB64)
    }

    /** The signed-in account's own key: its username is never held back. */
    private fun isOwnKey(signatureKeyB64: String): Boolean {
        val own = ownAccountIdHex() ?: return false
        return profiles.get(signatureKeyB64)?.accountIdHex == own
    }

    /** ADR-0098 section 3 — `mayShowUsername` for [key] now. */
    private fun showsUsername(key: String): Boolean {
        val book = deletions?.book ?: return true
        if (isOwnKey(key)) return true
        return try {
            book.mayShowUsername(key)
        } catch (_: Throwable) {
            false
        }
    }

    /** The UX §3 resolution for a signature key (standard base64). A deleted
     *  person resolves to [deletedName] whatever else is known (ADR-0098). */
    public fun resolve(signatureKeyB64: String): ResolvedName {
        if (isDeleted(signatureKeyB64)) return deletedName(keyBytesOf(signatureKeyB64))
        return resolveThroughAlias(signatureKeyB64, aliases) { key ->
            resolveName(
                localRename = renames[key],
                profile = profileFor(key),
                verificationOf = { username -> verifications.get(key, username) },
                signatureKey = keyBytesOf(key),
                showUsername = showsUsername(key),
            )
        }
    }

    /**
     * Whether this person's own profile is still owed: nothing names them, or
     * only what [onFoundPersonAnswered] filed. The name found stands in on the
     * row, and the daily profile request (ADR-0048 trigger 4) must go on asking
     * for the real one, or a contact whose Welcome failed never gets it. A
     * local rename names them as it always did.
     */
    public fun awaitsProfile(signatureKey: ByteArray): Boolean {
        val keyB64 = Base64.getEncoder().encodeToString(signatureKey)
        if (resolve(keyB64).isPlaceholder) return true
        return renames[keyB64] == null && profiles.get(keyB64)?.isFoundName == true
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

    /** The account id (hex) the stored profile of [signatureKeyB64] claims
     *  (ADR-0048 v2), or `null`: how a group member who is not a contact is
     *  asked about (ADR-0098 section 2). */
    public fun accountIdHexOf(signatureKeyB64: String): String? = profiles.get(signatureKeyB64)?.accountIdHex

    /**
     * ADR-0092 section 7 — whether the newest profile held from
     * [signatureKeyB64] lets others share the person's contact card. A person
     * who never sent a profile counts as shareable.
     */
    public fun isCardShareable(signatureKeyB64: String): Boolean =
        profiles.get(signatureKeyB64)?.cardShareable ?: true

    /** Forget every profile and verification learned by [accountIdB64]
     *  (forget device). Other accounts' names stay. */
    public fun forgetAccount(accountIdB64: String) {
        val account = me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(accountIdB64)
        profiles.clear(account)
        verifications.clear(account)
        deletions?.book?.forget()
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
            // ADR-0098 — the per-person deletion records and the owed group
            // removals go with the account.
            SharedPrefsPersonDeletionPersistence.delete(app, accountIdB64)
            DeletedMemberRemovals.deleteOnDevice(app, accountIdB64)
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
                ).also {
                    instance = it
                    attachAccountDeletions(context.applicationContext, it)
                }
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
public class ProfileNameSnapshot internal constructor(
    private val profiles: Map<String, PeerProfile>,
    private val verifications: Map<String, UsernameVerification>,
    private val renames: Map<String, String>,
    /** ADR-0079 — the pending contacts' link statements, the profile input
     *  of a key with no received profile ([pendingStatementsOf]). */
    private val pendingStatements: Map<String, PeerProfile> = emptyMap(),
    /** The keys of one person ([me.lakko.app.contacts.nameAliases]), the
     *  same set the app's directory falls through. */
    private val aliases: Map<String, String> = emptyMap(),
    /** ADR-0098 — what this device recorded about deleted accounts, read
     *  once; `null` = nobody is deleted and usernames show as before. */
    private val deletions: PersonDeletionBook? = null,
) {
    private fun deleted(key: String): Boolean {
        val book = deletions ?: return false
        return try {
            book.isDeleted(key) || (aliases[key]?.let(book::isDeleted) == true)
        } catch (_: Throwable) {
            false
        }
    }

    /** The resolved name for a signature key, standard base64, by the same
     *  rule as [ProfileDirectory.resolve]. */
    public fun resolve(signatureKeyB64: String): ResolvedName {
        if (deleted(signatureKeyB64)) return deletedName(keyBytesOf(signatureKeyB64))
        return resolveThroughAlias(signatureKeyB64, aliases) { key ->
            resolveName(
                localRename = renames[key],
                profile = profiles[key] ?: pendingStatements[key],
                verificationOf = { username ->
                    verifications[UsernameVerificationStore.pairKey(key, username)]
                },
                signatureKey = keyBytesOf(key),
                showUsername = deletions?.let { book ->
                    try {
                        book.mayShowUsername(key)
                    } catch (_: Throwable) {
                        false
                    }
                } ?: true,
            )
        }
    }

    /** `true` when the viewer knows SOMETHING about this person (a rename, a
     *  profile name, a pending contact's link-stated name, or a verified
     *  username). */
    public fun isKnown(signatureKeyB64: String): Boolean = !resolve(signatureKeyB64).isPlaceholder

    /**
     * A profile this background pass read from the journal itself, applied
     * IN MEMORY for the rest of the pass, under the store's own acceptance
     * rule ([PeerProfileStore.offer]: strictly newer, or a found name replaced
     * by a profile that says something). Nothing is written (C-59): the app
     * stores the same row when it polls. A rename still wins, as everywhere.
     */
    public fun withBatchProfile(
        signatureKeyB64: String,
        profile: PeerProfile,
        isNewer: (ULong, ULong) -> Boolean,
    ): ProfileNameSnapshot {
        if (signatureKeyB64.isEmpty()) return this
        val held = profiles[signatureKeyB64]
        val replacesFoundName = held?.isFoundName == true && !profile.isFoundName &&
            (profile.displayName != null || profile.username != null)
        if (held != null && !replacesFoundName && !isNewer(profile.updatedAtMs, held.updatedAtMs)) return this
        return ProfileNameSnapshot(
            profiles = profiles + (signatureKeyB64 to profile),
            verifications = verifications,
            renames = renames,
            pendingStatements = pendingStatements,
            aliases = aliases,
            deletions = deletions,
        )
    }

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
                pendingStatements = account?.let { a ->
                    try {
                        pendingStatementsOf(
                            me.lakko.app.contacts.PendingContactsFiles.onDevice(context.applicationContext).load(a),
                        )
                    } catch (_: Throwable) {
                        emptyMap()
                    }
                } ?: emptyMap(),
                aliases = account?.let { aliasesOnDevice(context.applicationContext, it) } ?: emptyMap(),
                // Read only: the book writes nothing until it is told to.
                deletions = account?.let { a ->
                    PersonDeletionBook(
                        persistence = object : PersonDeletionPersistence {
                            private val inner = SharedPrefsPersonDeletionPersistence(context.applicationContext)
                            override fun currentAccount(): String = a
                            override fun load(account: String) = inner.load(account)
                            override fun save(account: String, records: Map<String, PersonDeletionRecord>) = Unit
                        },
                        rules = FfiAccountDeletionRules,
                    )
                },
            )
        }
    }
}

/** The name aliases of [account] as the app's directory holds them, read
 *  from the stores the contacts repository writes (reads only). */
internal fun aliasesOnDevice(context: Context, account: String): Map<String, String> =
    try {
        val pins = me.lakko.app.contacts.PeerPinStore(context, account)
        me.lakko.app.contacts.nameAliases(
            provenLinks = me.lakko.app.contacts.SharedPrefsProvenLinkStore(context).load(account),
            resetRows = me.lakko.app.contacts.SharedPrefsResetLedgerStore(context).loadAll(account),
            pinnedKeyOf = { id ->
                try {
                    pins.readPin(id)?.pinnedSignaturePubkeyB64
                } catch (_: Throwable) {
                    null
                }
            },
        )
    } catch (_: Throwable) {
        emptyMap()
    }

/** Standard base64 to bytes, empty on a malformed key. */
private fun keyBytesOf(keyB64: String): ByteArray =
    try {
        Base64.getDecoder().decode(keyB64)
    } catch (_: IllegalArgumentException) {
        ByteArray(0)
    }

/**
 * ADR-0073 phase 2 / UX 16.8 — the one fall-through: when nothing is known
 * under [keyB64], the name learned under the key [aliases] maps it to (one
 * level deep), else the placeholder of [keyB64] itself.
 */
internal fun resolveThroughAlias(
    keyB64: String,
    aliases: Map<String, String>,
    resolveOwn: (String) -> ResolvedName,
): ResolvedName {
    val own = resolveOwn(keyB64)
    if (!own.isPlaceholder) return own
    val previous = aliases[keyB64] ?: return own
    val inherited = resolveOwn(previous)
    return if (inherited.isPlaceholder) own else inherited
}

/**
 * ADR-0079 — the link statement of each pending contact as a profile input,
 * keyed by the inviter's account key (standard base64): the display name the
 * link states and the username it states (shown only when that pair is
 * already verified; no check is ever started for it), at stamp 0. A contact whose link states neither is left out, so it does not
 * count as known.
 */
internal fun pendingStatementsOf(list: List<me.lakko.app.contacts.PendingContact>): Map<String, PeerProfile> =
    list.mapNotNull { c ->
        val key = c.signaturePubkeyB64.takeIf { it.isNotEmpty() } ?: return@mapNotNull null
        val name = c.displayName.trim().takeIf { it.isNotEmpty() }
        val username = c.username.trim().takeIf { it.isNotEmpty() }
        if (name == null && username == null) return@mapNotNull null
        key to PeerProfile(displayName = name, username = username, updatedAtMs = 0uL)
    }.toMap()

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
        /** ADR-0101 section 8 - the envelope as received, sealed under the
         *  device's avatar key (it carries the photo references' keys);
         *  absent before the field. */
        val envelopeSealed: String? = null,
        /** ADR-0092 section 7 - whether the person lets others share their
         *  card; absent (a row written before the field) reads as `true`. */
        val cardShareable: Boolean = true,
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
                    cardShareable = s.cardShareable,
                    envelopeB64 = openEnvelope(s.envelopeSealed),
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
                me.lakko.app.avatar.sealAvatarRefs(me.lakko.app.avatar.KeystoreAvatarSeal, p.avatar),
                sealEnvelope(p.envelopeB64),
                cardShareable = p.cardShareable)
        }
        prefs().edit().putString(profilesKey(account), json.encodeToString(stored)).apply()
    }

    /** ADR-0101 section 8 - [envelopeB64] sealed for the row, or `null`. */
    private fun sealEnvelope(envelopeB64: String?): String? {
        val b64 = envelopeB64 ?: return null
        return try {
            val sealed = me.lakko.app.avatar.KeystoreAvatarSeal.seal(java.util.Base64.getDecoder().decode(b64))
                ?: return null
            java.util.Base64.getEncoder().encodeToString(sealed)
        } catch (_: Throwable) {
            null
        }
    }

    /** The inverse of [sealEnvelope]; `null` on any failure. */
    private fun openEnvelope(stored: String?): String? {
        val s = stored ?: return null
        return try {
            val plain = me.lakko.app.avatar.KeystoreAvatarSeal.open(java.util.Base64.getDecoder().decode(s))
                ?: return null
            java.util.Base64.getEncoder().encodeToString(plain)
        } catch (_: Throwable) {
            null
        }
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

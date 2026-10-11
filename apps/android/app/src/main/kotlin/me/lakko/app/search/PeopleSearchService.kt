// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.search

import android.content.Context
import java.util.Base64
import me.lakko.app.chat.AllowanceKind
import me.lakko.app.contacts.ContactsError
import me.lakko.app.contacts.ContactsRepository
import me.lakko.app.discovery.DiscoveryClient
import me.lakko.app.l10n.UiText
import me.lakko.app.profile.ProfileDirectory
import me.lakko.app.reach.ReachManager
import me.lakko.app.ui.contacts.contactsErrorText
import uniffi.lakko_ffi_uniffi.OprfResult

/**
 * People search, wired to the running app: the core, the discovery and DS
 * clients, the profile directory, the address book and the kept reach
 * grants. The Android twin of `apps/web/src/search/wiring.ts`: every decision
 * lives in `PeopleModel.kt`, `PeopleSearch.kt`, `Introductions.kt` and
 * `SendFoundRequest.kt`; this class only answers their questions from real
 * sources, so it holds no rule of its own.
 */
public class PeopleSearchService(
    context: Context,
    private val contacts: ContactsRepository,
) {
    private val appContext = context.applicationContext

    private fun reach(): ReachManager = ReachManager.get(appContext)

    private fun profiles(): ProfileDirectory = ProfileDirectory.get(appContext)

    /** The core's search normaliser, with the short-input fallback. */
    public val normalize: Normalize = searchNormalizer { input ->
        try {
            uniffi.lakko_ffi_uniffi.normalizeNameForSearch(input)
        } catch (_: Throwable) {
            null
        }
    }

    /** This account's key hex, `null` without a reach session. */
    public fun ownKeyHex(): String? = reach().ownAccountKey()?.let(::searchHex)

    /** UX section 6.1 — the network row is offered from the core's minimum. */
    public fun offersNetworkSearch(query: String): Boolean = try {
        val normalised = uniffi.lakko_ffi_uniffi.normalizeNameForSearch(query)
        normalised != null &&
            normalised.codePointCount(0, normalised.length) >= uniffi.lakko_ffi_uniffi.nameSearchMinChars().toInt()
    } catch (_: Throwable) {
        false
    }

    /** The address book: account key base64 -> account id hex (or `null`). */
    private fun contactKeys(): Map<String, String?> =
        contacts.state.value.accepted.associate { c ->
            Base64.getEncoder().encodeToString(c.request.fromPubkey) to
                c.request.fromAccountId?.takeIf { it.size == ACCOUNT_ID_LEN }?.let(::searchHex)
        }

    /** What this device knows about one person (by account key, base64). */
    public fun known(keyB64: String, contactKeys: Map<String, String?> = contactKeys()): KnownPerson {
        val isContact = keyB64 in contactKeys
        val resolved = profiles().resolve(keyB64)
        val verified = resolved.verifiedUsername
        // Named by a rename or a profile name (not only by a username).
        val named = !resolved.isPlaceholder && (verified == null || resolved.primary != "@$verified")
        val keyBytes = try {
            Base64.getDecoder().decode(keyB64)
        } catch (_: IllegalArgumentException) {
            ByteArray(0)
        }
        // Only a rename or a profile name is a held name. A contact without
        // one keeps a fallback (their fingerprint, or their username), which
        // a name the person published beats in the search; anyone else is
        // named only by a name this device holds.
        val name = if (named) resolved.primary else null
        val fallback = when {
            named || !isContact -> null
            resolved.isPlaceholder -> resolved.fingerprint
            else -> resolved.primary
        }
        val kept = try {
            reach().bestHeldGrantWire(keyBytes)?.let { Base64.getEncoder().encodeToString(it) }
        } catch (_: Throwable) {
            null
        }
        return KnownPerson(
            name = name,
            verifiedUsername = verified,
            isContact = isContact,
            keptGrantWireB64 = kept,
            fallbackName = fallback,
        )
    }

    /**
     * The people this device already knows: its contacts and every profile it
     * holds (group and channel members alike). A claimed account id only
     * locates the identity bundle; the send path checks the bundle's key
     * against this key before anything goes out.
     */
    public fun localCandidates(): List<LocalCandidate> {
        val book = contactKeys()
        val keys = LinkedHashMap<String, String?>()
        keys.putAll(book)
        for ((keyB64, profile) in profiles().profiles.all()) {
            if (keys[keyB64] == null) keys[keyB64] = profile.accountIdHex
        }
        return keys.mapNotNull { (keyB64, accountIdHex) ->
            val keyHex = try {
                searchHex(Base64.getDecoder().decode(keyB64))
            } catch (_: IllegalArgumentException) {
                return@mapNotNull null
            }
            val k = known(keyB64, book)
            LocalCandidate(
                keyB64 = keyB64,
                keyHex = keyHex,
                // The local half has no published name to prefer: a contact
                // shows by its fallback.
                name = k.name ?: k.fallbackName,
                verifiedUsername = k.verifiedUsername,
                isContact = k.isContact,
                accountIdHex = accountIdHex,
                keptGrantWireB64 = k.keptGrantWireB64,
            )
        }
    }

    /** The local half, as the person types. */
    public fun local(query: String): List<PersonResult> =
        localPeople(localCandidates(), query, normalize, ownKeyHex())

    /** The network half. Never throws (a cancellation is let through). */
    public suspend fun search(query: String): PeopleSearchOutcome =
        DiscoveryClient().use { discovery -> searchPeople(query, deps(discovery)) }

    private fun deps(discovery: DiscoveryClient): PeopleSearchDeps = object : PeopleSearchDeps {
        override fun canonicalUsername(typed: String): String? = try {
            val bare = uniffi.lakko_ffi_uniffi.bareName(typed)
            if (!uniffi.lakko_ffi_uniffi.looksLikeAName(bare)) null else uniffi.lakko_ffi_uniffi.validateUsername(bare)
        } catch (_: Throwable) {
            null
        }

        override fun queryTerm(query: String): String? = try {
            uniffi.lakko_ffi_uniffi.nameQueryTerm(query)
        } catch (_: Throwable) {
            null
        }

        override fun nameMatches(displayName: String, query: String): Boolean = try {
            uniffi.lakko_ffi_uniffi.nameMatchesQuery(displayName, query)
        } catch (_: Throwable) {
            false
        }

        override suspend fun lookupChannel(handle: String): String? = discovery.lookupChannel(handle)

        override suspend fun usernameOprf(username: String): OprfResult = discovery.oprfUsername(username)

        override suspend fun accountOfUsernameLookupKey(lookupKey: ByteArray): LookupHit? =
            discovery.lookupPublic(lookupKey)?.let { LookupHit(it.accountId, it.publicRecord) }

        override suspend fun nameTermOprf(term: String): OprfResult = discovery.oprfNameTerm(term)

        override suspend fun accountsOfNameTermLookupKey(lookupKey: ByteArray): List<LookupHit> =
            discovery.nameLookup(lookupKey).map { LookupHit(it.accountId, it.publicRecord) }

        override fun openPublicRecord(record: ByteArray, oprfOutput: ByteArray): OpenedPublicRecord? = try {
            uniffi.lakko_ffi_uniffi.openPublicRecord(record, oprfOutput)?.let {
                OpenedPublicRecord(it.identityPk, it.accountId, it.displayName)
            }
        } catch (_: Throwable) {
            null
        }

        override suspend fun identityKeyOf(accountId: ByteArray): ByteArray? =
            discovery.identityFetch(accountId)?.signature_pubkey?.let { Base64.getDecoder().decode(it) }

        override fun known(keyB64: String): KnownPerson = this@PeopleSearchService.known(keyB64)

        override suspend fun openIntroductions(probes: List<IntroductionProbe>): List<OpenedIntroduction> {
            val reach = reach()
            if (reach.ownAccountKey() == null) return emptyList()
            val records = recordsOf(
                bundleWires = reach.introductionBundleWires(),
                offeredRecords = reach.offeredIntroductionRecordsHex().mapNotNull { me.lakko.app.reach.reachUnhex(it) },
                core = FfiIntroductionCore,
            )
            return openMatchingIntroductions(records, probes, FfiIntroductionCore)
        }

        override suspend fun verifyUsername(
            certificate: ByteArray,
            usernameLookupKey: ByteArray,
            accountId: ByteArray,
        ): Boolean = try {
            val key = usernameCertKey(discovery) ?: return false
            uniffi.lakko_ffi_uniffi.verifyUsernameBinding(certificate, key, usernameLookupKey, accountId)
            true
        } catch (cancel: kotlin.coroutines.cancellation.CancellationException) {
            throw cancel
        } catch (_: Throwable) {
            false
        }

        override suspend fun reachCheck(identityPk: ByteArray, grantWireB64: String): Boolean =
            reachCheckWith(identityPk, grantWireB64)

        override fun legOutcomeOf(error: Throwable): LegOutcome =
            if (me.lakko.app.anon.isAnonTokenQuotaExhausted(error)) LegOutcome.ALLOWANCE_USED else LegOutcome.FAILED

        override val ownKeyHex: String? = this@PeopleSearchService.ownKeyHex()
    }

    /** The DS reach check of a person's account mailbox with a grant wire. */
    private suspend fun reachCheckWith(identityPk: ByteArray, grantWireB64: String): Boolean {
        val grant = uniffi.lakko_ffi_uniffi.decodeReachGrant(Base64.getDecoder().decode(grantWireB64))
        // ADR-0066 3.5a rule 1: the person's CURRENT mailbox (a proven
        // predecessor key resolves to its successor; nothing else does).
        val mailbox = reach().mailboxOf(identityPk)
        return me.lakko.app.ds.DsClient().use { ds -> ds.checkReach(mailbox, me.lakko.app.ds.reachSigner(grant.seed)) }
    }

    @Volatile
    private var certKey: ByteArray? = null

    /** The username-certificate verifying key: the build-time pin, else the
     *  well-known key fetched once. */
    private suspend fun usernameCertKey(discovery: DiscoveryClient): ByteArray? {
        certKey?.let { return it }
        val pinned = me.lakko.app.BuildConfig.LAKKO_USERNAME_CERT_KEY
        val key = if (pinned.isNotBlank()) {
            try {
                me.lakko.app.auth.base64UrlDecodeNoPad(pinned).takeIf { it.size == 32 }
            } catch (_: IllegalArgumentException) {
                null
            }
        } else {
            discovery.fetchUsernameCertKey()
        }
        certKey = key
        return key
    }

    /** Send the contact request to a found person. Never throws (a
     *  cancellation is let through). */
    public suspend fun send(person: PersonResult, intro: String): SendOutcome =
        sendFoundRequest(person, intro.toByteArray(Charsets.UTF_8), sendDeps(person.username))

    private fun sendDeps(shownUsername: String?): SendDeps = object : SendDeps {
        override suspend fun reachCheck(identityPk: ByteArray, grantWireB64: String): Boolean =
            reachCheckWith(identityPk, grantWireB64)

        override suspend fun identityFetch(accountId: ByteArray): IdentityKeys? =
            DiscoveryClient().use { it.identityFetch(accountId) }?.let {
                IdentityKeys(Base64.getDecoder().decode(it.signature_pubkey), Base64.getDecoder().decode(it.x25519_pubkey))
            }

        override suspend fun sendByUsername(username: String, intro: ByteArray, grantWireB64: String?) {
            contacts.sendFoundContactRequestByUsername(
                recipientUsername = username,
                introMessage = intro,
                grantWire = grantWireB64?.let { Base64.getDecoder().decode(it) },
            )
        }

        override suspend fun sendToBundle(
            accountId: ByteArray,
            signaturePub: ByteArray,
            x25519Pub: ByteArray,
            intro: ByteArray,
            grantWireB64: String?,
        ) {
            contacts.sendFoundContactRequestToBundle(
                recipientAccountId = accountId,
                recipientSignaturePubkey = signaturePub,
                recipientX25519Pubkey = x25519Pub,
                introMessage = intro,
                grantWire = grantWireB64?.let { Base64.getDecoder().decode(it) },
                shownUsername = shownUsername,
            )
        }

        override fun isRefusal(error: Throwable): Boolean {
            val variant = (error as? ContactsError)?.variant
            return variant is ContactsError.Variant.UsernameUnknown || variant == ContactsError.Variant.ReachRefused
        }

        override fun errorMessage(error: Throwable): UiText {
            // UX 16.1 — a found person who is a contact with reset, unverified
            // keys answers the action's own line.
            val refused = error as? me.lakko.app.contacts.SupersessionRefused
            if (refused != null &&
                me.lakko.app.contacts.refusalIsLoggedReset(refused.reason, refused.loggedReset)
            ) {
                return me.lakko.app.contacts.KeysResetCopy.verifyFirst(shownUsername?.let { "@$it" })
            }
            return contactsErrorText(error, "contacts.error.send", AllowanceKind.LOOKUP)
        }
    }

    private companion object {
        const val ACCOUNT_ID_LEN = 16
    }
}

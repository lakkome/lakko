// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.search

import android.content.Context
import java.util.Base64
import kotlinx.coroutines.flow.StateFlow
import me.lakko.app.chat.AllowanceKind
import me.lakko.app.contacts.ContactsError
import me.lakko.app.contacts.ContactsRepository
import me.lakko.app.contacts.PendingContact
import me.lakko.app.contacts.grantMailboxPkOrNull
import me.lakko.app.discovery.DiscoveryClient
import me.lakko.app.discovery.boundX25519Key
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

    /** The people added through their invite link who have not answered yet. */
    public val invited: StateFlow<List<PendingContact>> get() = contacts.invited

    /** This account's key hex, `null` without a reach session. */
    public fun ownKeyHex(): String? = reach().ownAccountKey()?.let(::searchHex)

    /** What the field remembers for this session (the texts answered, the
     *  requests sent from rows), shared by the Chats and the Contacts field. */
    public val memory: PeopleSearchMemory = PeopleSearchMemory()

    /** The contact state a row reads: who the outgoing list holds, who
     *  became a contact. */
    public val contactsState: kotlinx.coroutines.flow.StateFlow<me.lakko.app.contacts.ContactsState>
        get() = contacts.state

    /**
     * UX section 6.1 — whether the field may ask the network for [bare] (the
     * text with a leading `@` dropped): only when it could be a username or
     * carries a name term. [NETWORK_SEARCH_MIN_CHARS] is the session's.
     */
    public fun asksNetwork(bare: String): Boolean = try {
        val normalised = uniffi.lakko_ffi_uniffi.normalizeNameForSearch(bare)
        normalised != null &&
            normalised.codePointCount(0, normalised.length) >= uniffi.lakko_ffi_uniffi.nameSearchMinChars().toInt() &&
            (canonicalUsernameOf(bare) != null || uniffi.lakko_ffi_uniffi.nameQueryTerm(bare) != null)
    } catch (_: Throwable) {
        false
    }

    private fun canonicalUsernameOf(typed: String): String? = try {
        val bare = uniffi.lakko_ffi_uniffi.bareName(typed)
        if (!uniffi.lakko_ffi_uniffi.looksLikeAName(bare)) null else uniffi.lakko_ffi_uniffi.validateUsername(bare)
    } catch (_: Throwable) {
        null
    }

    /** A search field's network half, on [scope] (the screen's). */
    public fun session(scope: kotlinx.coroutines.CoroutineScope): PeopleSearchSession = PeopleSearchSession(
        scope = scope,
        memory = memory,
        asks = ::asksNetwork,
        search = ::search,
        send = ::send,
        reachCheck = { keyB64, wire -> reachCheckWith(Base64.getDecoder().decode(keyB64), wire) },
        dismissSent = ::dismissSentTo,
    )

    /** Contacts > Sent > Dismiss for this person, by key or by account id. */
    private fun dismissSentTo(person: PersonResult) {
        val sent = contacts.state.value.sent.filter { request ->
            request.recipientKeyB64 == person.keyB64 ||
                person.accountIdHex?.let { hex ->
                    runCatching { searchHex(Base64.getDecoder().decode(request.recipientAccountIdB64)) }.getOrNull() == hex
                } == true
        }
        sent.forEach(contacts::dismissSentRequest)
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
        return KnownPerson(
            name = name,
            verifiedUsername = verified,
            isContact = isContact,
            fallbackName = fallback,
        )
    }

    /** Every grant kept for a person, each once, in the order they are tried. */
    private fun keptGrantWires(key: ByteArray): List<String> = try {
        reach().heldGrantWiresInOrder(key).map { Base64.getEncoder().encodeToString(it) }.distinct()
    } catch (_: Throwable) {
        emptyList()
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
            val key = try {
                Base64.getDecoder().decode(keyB64)
            } catch (_: IllegalArgumentException) {
                return@mapNotNull null
            }
            val keyHex = searchHex(key)
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
                keptGrantWiresB64 = keptGrantWires(key),
            )
        }
    }

    /** The local half, as the person types: [candidates] with what the
     *  Delivery Service said about their kept grants ([checks]), matched. */
    public fun local(
        query: String,
        candidates: List<LocalCandidate>,
        checks: Map<String, KeptGrantCheck>,
    ): List<PersonResult> = localPeople(withKeptGrantChecks(candidates, checks), query, normalize, ownKeyHex())

    /** The network half. Never throws (a cancellation is let through). */
    public suspend fun search(query: String): PeopleSearchOutcome =
        DiscoveryClient().use { discovery -> searchPeople(query, deps(discovery)) }

    private val usernameLookupKeys = KeptUsernameLookupKeys()

    private fun deps(discovery: DiscoveryClient): PeopleSearchDeps = object : PeopleSearchDeps {
        private var recordsHeld = 0

        override fun canonicalUsername(typed: String): String? = canonicalUsernameOf(typed)

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

        override suspend fun usernameLookupKey(username: String): ByteArray =
            usernameLookupKeys.get(username) { discovery.oprfUsername(it).lookupKey }

        override suspend fun accountOfUsernameLookupKey(lookupKey: ByteArray): LookupHit? =
            discovery.lookupPublic(lookupKey)?.let { LookupHit(it.accountId, it.publicRecord) }

        override suspend fun nameTermOprf(term: String): OprfResult = discovery.oprfNameTerm(term)

        override suspend fun accountsOfNameTermLookupKey(lookupKey: ByteArray): List<LookupHit> =
            discovery.nameLookup(lookupKey).map { LookupHit(it.accountId, it.publicRecord) }

        override fun openPublicRecord(record: ByteArray, oprfOutput: ByteArray): OpenedPublicRecord? = try {
            uniffi.lakko_ffi_uniffi.openPublicRecord(record, oprfOutput)?.let {
                OpenedPublicRecord(
                    it.identityPk, it.accountId, it.displayName, it.mailboxBinding,
                    it.username, it.usernameCertificate,
                )
            }
        } catch (_: Throwable) {
            null
        }

        override suspend fun identityKeyOf(accountId: ByteArray): ByteArray? =
            discovery.identityFetch(accountId)?.also(::noteServedProof)
                ?.signature_pubkey?.let { Base64.getDecoder().decode(it) }

        override fun isProvenPredecessor(foundKey: ByteArray, publishedKey: ByteArray): Boolean =
            this@PeopleSearchService.isProvenPredecessor(foundKey, publishedKey)

        override fun known(keyB64: String): KnownPerson = this@PeopleSearchService.known(keyB64)

        override suspend fun openIntroductions(probes: List<IntroductionProbe>): List<OpenedIntroduction> {
            val reach = reach()
            if (reach.ownAccountKey() == null) return emptyList()
            val records = recordsOf(
                bundleWires = reach.introductionBundleWires(),
                offeredRecords = reach.offeredIntroductionRecordsHex().mapNotNull { me.lakko.app.reach.reachUnhex(it) },
                core = FfiIntroductionCore,
            )
            recordsHeld = records.size
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

        override fun noteSearch(counts: SearchCounts) {
            android.util.Log.i(
                "LakkoSearch",
                "people search: records held=$recordsHeld opened=${counts.recordsOpened} " +
                    "matched=${counts.keptAfterMatch} reach refused=${counts.refusedByReachCheck} " +
                    "people=${counts.people} ${reach().storedCounts()}",
            )
        }
    }

    /** The supersession proof each served bundle carried, by the key it
     *  serves (standard base64): public material, kept for this process. */
    private val servedProofs = java.util.concurrent.ConcurrentHashMap<String, ByteArray>()

    private fun noteServedProof(bundle: me.lakko.app.discovery.IdentityFetchResponse) {
        val proof = bundle.supersession_proof?.takeIf { it.isNotEmpty() } ?: return
        try {
            servedProofs[bundle.signature_pubkey] = Base64.getDecoder().decode(proof)
        } catch (_: IllegalArgumentException) {
            // A malformed proof proves nothing.
        }
    }

    /**
     * ADR-0074 section 3.5a - [foundKey] is a proven predecessor of
     * [publishedKey]: a step the gate recorded, or the proof the directory
     * served with [publishedKey] verifies as signed by [foundKey] over
     * exactly that key. Nothing is pinned or recorded here (a search is a
     * read); an unproven change is two people.
     */
    private fun isProvenPredecessor(foundKey: ByteArray, publishedKey: ByteArray): Boolean {
        val foundB64 = Base64.getEncoder().encodeToString(foundKey)
        val publishedB64 = Base64.getEncoder().encodeToString(publishedKey)
        if (contacts.isProvenPredecessor(foundB64, publishedB64)) return true
        val proof = servedProofs[publishedB64] ?: return false
        return try {
            uniffi.lakko_ffi_uniffi.verifySupersession(proof, foundKey, System.currentTimeMillis() / 1000L)
                .contentEquals(publishedKey)
        } catch (_: Throwable) {
            false
        }
    }

    /** The DS reach check of a person's account mailbox with a grant wire. */
    private suspend fun reachCheckWith(identityPk: ByteArray, grantWireB64: String): Boolean {
        val grant = uniffi.lakko_ffi_uniffi.decodeReachGrant(Base64.getDecoder().decode(grantWireB64))
        // ADR-0074 3.5a rule 1: the person's CURRENT key (a proven
        // predecessor key resolves to its successor; nothing else does).
        // ADR-0078: the mailbox is the one the grant's binding names, verified
        // against that key; a grant that does not verify reaches nobody.
        val current = reach().mailboxAccountKey(identityPk)
        val mailboxPk = me.lakko.app.contacts.NativeMailboxCrypto.grantMailboxPkOrNull(grant, current) ?: return false
        val mailbox = me.lakko.app.contacts.NativeMailboxCrypto.address(mailboxPk)
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

    /** Send the contact request to a found person, at once and with no
     *  introduction (UX section 6.1). Never throws (a cancellation is let
     *  through). */
    public suspend fun send(person: PersonResult): SendOutcome =
        sendFoundRequest(person, ByteArray(0), sendDeps(person.username, person.name))

    private fun sendDeps(shownUsername: String?, shownName: String): SendDeps = object : SendDeps {
        override suspend fun reachCheck(identityPk: ByteArray, grantWireB64: String): Boolean =
            reachCheckWith(identityPk, grantWireB64)

        override fun heldGrantWires(identityPk: ByteArray): List<String> = keptGrantWires(identityPk)

        override fun isProvenPredecessor(foundKey: ByteArray, publishedKey: ByteArray): Boolean =
            this@PeopleSearchService.isProvenPredecessor(foundKey, publishedKey)

        override suspend fun identityFetch(accountId: ByteArray): IdentityKeys? =
            DiscoveryClient().use { it.identityFetch(accountId) }?.also(::noteServedProof)?.let {
                // An unbound X25519 key reads as no bundle: nothing is sealed.
                val x25519 = it.boundX25519Key() ?: return@let null
                IdentityKeys(Base64.getDecoder().decode(it.signature_pubkey), x25519)
            }

        override suspend fun sendByUsername(username: String, intro: ByteArray, grantWireB64: String?, bindingB64: String?) {
            contacts.sendFoundContactRequestByUsername(
                recipientUsername = username,
                introMessage = intro,
                grantWire = grantWireB64?.let { Base64.getDecoder().decode(it) },
                mailboxBinding = bindingB64?.let { Base64.getDecoder().decode(it) },
                shownName = shownName,
            )
        }

        override suspend fun sendToBundle(
            accountId: ByteArray,
            signaturePub: ByteArray,
            x25519Pub: ByteArray,
            intro: ByteArray,
            grantWireB64: String?,
            bindingB64: String?,
        ) {
            contacts.sendFoundContactRequestToBundle(
                recipientAccountId = accountId,
                recipientSignaturePubkey = signaturePub,
                recipientX25519Pubkey = x25519Pub,
                introMessage = intro,
                grantWire = grantWireB64?.let { Base64.getDecoder().decode(it) },
                shownUsername = shownUsername,
                mailboxBinding = bindingB64?.let { Base64.getDecoder().decode(it) },
                shownName = shownName,
            )
        }

        override fun isRefusal(error: Throwable): Boolean {
            val variant = (error as? ContactsError)?.variant
            return variant is ContactsError.Variant.UsernameUnknown || variant == ContactsError.Variant.ReachRefused
        }

        override fun errorMessage(error: Throwable): UiText {
            return contactsErrorText(error, "contacts.error.send", AllowanceKind.LOOKUP)
        }

        override fun noteRefusal(reason: String) {
            android.util.Log.w("LakkoSearch", "contact request refused: $reason")
        }
    }

    private companion object {
        const val ACCOUNT_ID_LEN = 16
    }
}

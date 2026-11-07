// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.reach

import uniffi.lakko_ffi_uniffi.FfiContactKeyRowVerdict
import android.content.Context
import android.util.Log
import java.security.SecureRandom
import java.util.concurrent.atomic.AtomicReference
import kotlin.coroutines.cancellation.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import me.lakko.app.ds.DsClient
import me.lakko.app.ds.DsJournalSigner
import me.lakko.app.ds.DsNetworkError
import me.lakko.app.ds.DsReachAuth
import me.lakko.app.ds.DsReachKeyAdd
import me.lakko.app.ds.FfiDsJournalSigner
import me.lakko.app.ds.reachSigner
import me.lakko.app.contacts.MailboxCrypto
import me.lakko.app.contacts.MailboxDest
import me.lakko.app.contacts.NativeMailboxCrypto
import me.lakko.app.contacts.bindingMailboxPkOrNull
import me.lakko.app.contacts.firstVerifiedMailboxPk
import me.lakko.app.contacts.grantMailboxPkOrNull
import me.lakko.app.contacts.verifiedGrantDest
import uniffi.lakko_ffi_uniffi.FfiReachClass
import uniffi.lakko_ffi_uniffi.FfiReachGrant
import uniffi.lakko_ffi_uniffi.FfiReachState
import uniffi.lakko_ffi_uniffi.FfiScopeId
import uniffi.lakko_ffi_uniffi.FfiScopeKind
import uniffi.lakko_ffi_uniffi.FfiVisibilityMode
import uniffi.lakko_ffi_uniffi.FfiVisibilitySetting
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.OprfResult

/** Whether the setting the person chose is in effect on the server
 *  (`docs/ux/who-can-find-me.md` sections 8 and 9). */
public enum class ReachApplyStatus {
    /** What the screen shows is what the server holds. */
    APPLIED,

    /** An update is on its way. */
    APPLYING,

    /** The device could not reach the server; the choice applies later. */
    OFFLINE,

    /** The server refused, or the session is gone. */
    FAILED,
}

/**
 * ADR-0060 — the owner's reach keys and the grants other people handed this
 * device, bound to the signed-in account.
 *
 * - Derives every one of the owner's keys from the account seed and a version
 *   counter (nothing secret of the owner's is stored).
 * - Keeps the owner's mailbox holding exactly [desiredReachKeys]: one
 *   `PUT /ds/mailbox/{hex}/reach` per change, signed by the mailbox key
 *   (ADR-0078).
 * - Keeps the grants received, and picks the one a write into someone's
 *   mailbox is signed with ([reachAuthFor]); the mailbox a write is sealed to
 *   comes from the grant's binding, verified against the account key held
 *   for the person ([mailboxPkFor]).
 * - Holds the "who can find me" setting.
 *
 * The setting and the version counters are shared with the account's other
 * devices (ADR-0060 §7): a local change goes out through [deviceSyncPublisher]
 * as a DeviceGroup service envelope, and one received from a sibling is kept
 * by [adoptDeviceSync] when strictly newer. A device admitting a new one sends
 * it [deviceSyncEnvelopes], each with its original stamp.
 */
public class ReachManager internal constructor(
    private val store: ReachDocStore,
    private val dsFactory: () -> DsClient = { DsClient() },
    private val ownProfiles: OwnProfileAccess? = null,
    /** ADR-0074 section 3.5a - the proven key steps this account recorded
     *  (its contacts' and its own), by the signed-in account id (standard
     *  base64). Only a PROVEN step is ever in it. */
    private val provenLinks: (ownerAccountIdB64: String) -> List<me.lakko.app.contacts.ProvenLink> = { emptyList() },
    /** ADR-0074 section 3.5a - this account's own previous account keys
     *  this device still holds the seed of, newest first, each as
     *  `(signature public key, 32-byte Ed25519 seed)`. A committer
     *  delegation may name any of them. SECRET; the caller of the provider
     *  wipes the seeds. */
    private val ownPredecessorKeys: (ownerAccountIdB64: String) -> List<Pair<ByteArray, ByteArray>> = { emptyList() },
    /** ADR-0078 - the binding check and sealing of mailbox rows. */
    private val mailboxCrypto: MailboxCrypto = NativeMailboxCrypto,
    /** The added account key a committer-delegation envelope names; a throw
     *  drops the envelope. A test injects a fake (the host JVM loads no
     *  native library). */
    private val delegationAddedKey: (envelope: ByteArray) -> ByteArray = {
        uniffi.lakko_ffi_uniffi.decodeCommitterDelegationEnvelope(it).addedAccountKey
    },
) {
    /**
     * ADR-0073 R.4 - the account and generation behind [ownerKey] when this
     * device ACCEPTED a reset that re-pinned the person to it, with the
     * attestation's `signed_at`: what the continuity reach key of that
     * person's new mailbox derives from. Installed by the contacts
     * repository; `null` (or a `null` answer) offers no continuity key.
     */
    @Volatile
    public var acceptedResetOf: ((ownerKey: ByteArray) -> me.lakko.app.contacts.AcceptedResetOfKey?)? = null

    /** The own-profile store (ADR-0048 section 4a), keyed by the raw account
     *  id: the own name travels between the account's devices beside the
     *  setting. */
    public interface OwnProfileAccess {
        /** The stored own profile of [accountId], or `null`. */
        public fun load(accountId: ByteArray): me.lakko.app.profile.OwnProfile?

        /** Apply [transform] to the stored profile of [accountId] as one
         *  read-modify-write; a `null` result stores nothing. Returns whether
         *  something was stored. */
        public fun update(
            accountId: ByteArray,
            transform: (me.lakko.app.profile.OwnProfile?) -> me.lakko.app.profile.OwnProfile?,
        ): Boolean
    }

    private class Binding(
        val accountIdB64: String,
        /** The account identity's 32-byte Ed25519 seed (`reachSeed` input). */
        val accountSeed: ByteArray,
        /** The account identity's signature public key. */
        val accountKey: ByteArray,
        /** The account mailbox address (ADR-0078: a hash of the mailbox key). */
        val mailbox: ByteArray,
        /** The account's 32-byte mailbox public key. */
        val mailboxPk: ByteArray,
        /** The account's 97-byte mailbox binding (the key signed by the
         *  identity key): what every grant and public record carries. */
        val mailboxBinding: ByteArray,
        /** The mailbox-key proof signer of that mailbox. */
        val mailboxSigner: DsJournalSigner,
    )

    private val binding = AtomicReference<Binding?>(null)
    private val lock = Mutex()
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val random = SecureRandom()

    @Volatile
    private var doc: ReachDoc = ReachDoc()

    private val mutableVisibility = MutableStateFlow<FfiVisibilitySetting?>(null)

    /** The setting as the person chose it, `null` before a session binds. */
    public val visibility: StateFlow<FfiVisibilitySetting?> = mutableVisibility.asStateFlow()

    private val mutableStatus = MutableStateFlow(ReachApplyStatus.APPLIED)

    /** Whether [visibility] is in effect on the server. */
    public val applyStatus: StateFlow<ReachApplyStatus> = mutableStatus.asStateFlow()

    /** The Everyone publication did not land, so the person is not findable
     *  the way the setting says (or still findable after leaving it). Shown
     *  through [applyStatus], whose Retry runs the publication again. */
    @Volatile
    private var everyoneStuck: Boolean = false

    /** The account whose backup was just restored, read at the next [bind]. */
    private val restoredBeforeBind = java.util.concurrent.ConcurrentHashMap.newKeySet<String>()

    /** The visibility envelope a restore handed over before a session bound:
     *  a backup composed in that window still carries it. */
    private val restoredVisibility = java.util.concurrent.ConcurrentHashMap<String, ByteArray>()

    /** Called after a setting change landed (profiles republished, Everyone
     *  published or withdrawn). Installed by the chat repository. */
    @Volatile
    public var onVisibilityChanged: (suspend (FfiVisibilitySetting) -> Unit)? = null

    /** ADR-0048 section 4a — the own name was adopted from another of the
     *  account's devices: what follows the name here (the Everyone
     *  publication) is refreshed. Never a group republish. */
    public var onOwnNameAdopted: (suspend () -> Unit)? = null

    /** Sends one `lakko-contacts` envelope to the account's other devices over
     *  the DeviceGroup (ADR-0060 §7). Installed by the DeviceGroup sync
     *  runtime; `null` while none runs, when a change stays on this device
     *  until [deviceSyncEnvelopes] is next sent. */
    @Volatile
    public var deviceSyncPublisher: ((ByteArray) -> Unit)? = null

    /** Envelopes a sibling sent before a session bound here, applied on
     *  [bind] (the newest of each kind wins there by the usual rule). */
    private val pendingDeviceSync = java.util.concurrent.ConcurrentLinkedQueue<ByteArray>()

    // -----------------------------------------------------------------
    // Session
    // -----------------------------------------------------------------

    /**
     * Bind to the signed-in account and reconcile its mailbox. Idempotent.
     *
     * ADR-0074 section 3.5, "Reach": [account] is the identity the directory
     * serves. When it is not the key the stored registrations were made
     * under (the account's keys were rotated, in this session or before a
     * restart), the reach keys MOVE: see [rekeyDecision].
     */
    public fun bind(accountIdB64: String, account: Identity) {
        val current = binding.get()
        val accountKey = account.signaturePublicKey()
        if (current != null && current.accountIdB64 == accountIdB64 && current.accountKey.contentEquals(accountKey)) {
            launchSync()
            return
        }
        val sameAccount = current != null && current.accountIdB64 == accountIdB64
        val ffiSigner = uniffi.lakko_ffi_uniffi.dsMailboxSigner(account)
        val mailboxPk = uniffi.lakko_ffi_uniffi.MailboxKey.fromAccountIdentity(account).use { it.publicKey() }
        val bound = Binding(
            accountIdB64 = accountIdB64,
            accountSeed = account.ed25519Seed(),
            accountKey = accountKey,
            mailbox = ffiSigner.mailboxAddress(),
            mailboxPk = mailboxPk,
            mailboxBinding = uniffi.lakko_ffi_uniffi.signMailboxBinding(account),
            mailboxSigner = FfiDsJournalSigner(ffiSigner),
        )
        // The previous binding's seed is NOT wiped here: a reconcile still
        // running against it would derive its keys from zeroes.
        binding.set(bound)
        // The same account under another key keeps the document it holds:
        // the setting, the version counters and the grants this account
        // HOLDS for other people are filed under the contact, not this key.
        if (!sameAccount) doc = store.load(accountIdB64)
        if (restoredBeforeBind.remove(accountIdB64)) {
            doc = doc.withUnknownEveryoneNote()
            store.save(accountIdB64, doc)
        }
        val boundKeyHex = reachHex(accountKey)
        val decision = rekeyDecision(
            boundKeyHex = boundKeyHex,
            registeredForKeyHex = doc.registeredForKeyHex,
            previousKeyHex = if (sameAccount) current?.accountKey?.let(::reachHex) else null,
        )
        if (decision != ReachRekey.KEEP) {
            // What was registered was registered on the OLD mailbox: nothing
            // is registered on the new one yet, so the next sync registers
            // everything there. The old mailbox and its keys are left as
            // they are.
            doc = if (decision == ReachRekey.MOVE) {
                // Section 3.5a rule 3: the grants every conversation holds
                // for this account open the OLD mailbox, so the profile is
                // owed to all of them again, under this key.
                // Rule 5: every `link` and `reply` key this device minted
                // derives from the OLD seed and named the old mailbox. None
                // of them is registered on the new one: the links are kept
                // aside for the rotating device to revoke at the ledger,
                // the reply keys are dropped.
                doc.copy(
                    registered = emptyList(),
                    registeredOpen = null,
                    registeredForKeyHex = boundKeyHex,
                    profileResendOwedForKeyHex = boundKeyHex,
                    grantsSpendOwedForKeyHex = boundKeyHex,
                    links = emptyList(),
                    replies = emptyList(),
                    spentLinks = spentLinksAfterMove(doc.spentLinks, doc.links),
                )
            } else {
                doc.copy(registeredForKeyHex = boundKeyHex)
            }
            store.save(accountIdB64, doc)
            if (decision == ReachRekey.MOVE) Log.i(LOG_TAG, "reach keys moved to the rotated account key")
        }
        mutableVisibility.value = decodeVisibility(doc.visibilityHex)
        launchSync()
        while (true) {
            val pending = pendingDeviceSync.poll() ?: break
            adoptDeviceSync(pending)
        }
        restoredVisibility.remove(accountIdB64)
    }

    /** Forget the session (sign-out, forget device). The stored document stays
     *  with the account; forget device deletes it through
     *  [me.lakko.app.account.AccountLocalWipe]. */
    public fun clear() {
        val old = binding.getAndSet(null)
        old?.accountSeed?.fill(0)
        pendingDeviceSync.clear()
        doc = ReachDoc()
        everyoneStuck = false
        mutableVisibility.value = null
        mutableStatus.value = ReachApplyStatus.APPLIED
    }

    /** A session with no native identity behind it, for the host suite. */
    internal fun bindForTest(accountIdB64: String, accountKey: ByteArray) {
        binding.set(
            Binding(
                accountIdB64 = accountIdB64,
                accountSeed = ByteArray(32),
                accountKey = accountKey,
                mailbox = ByteArray(32),
                mailboxPk = ByteArray(32),
                mailboxBinding = ByteArray(97),
                mailboxSigner = object : DsJournalSigner {
                    override val epoch: ULong = 0uL
                    override val verifyingKey: ByteArray = ByteArray(32)
                    override fun sign(method: String, path: String, query: String, body: ByteArray): String =
                        error("no signer in a test session")
                    override fun mailboxClaimBody(): String = error("no signer in a test session")
                },
            ),
        )
        doc = store.load(accountIdB64)
    }

    /** The bound account's signature public key, or `null`. */
    public fun ownAccountKey(): ByteArray? = binding.get()?.accountKey?.copyOf()

    /** ADR-0078 - the bound account's 97-byte mailbox binding, or `null`. */
    public fun ownMailboxBinding(): ByteArray? = binding.get()?.mailboxBinding?.copyOf()

    /**
     * The account bound at one moment, for work that is handed something FOR
     * that account and runs later (a launched intake): it passes this back
     * and the manager does nothing when another account, or none, is bound by
     * then. A sign-out and a new sign-in to the SAME account is a new one.
     */
    public class BoundAccount internal constructor(internal val token: Any)

    /** The account bound now, or `null`. */
    public fun boundAccount(): BoundAccount? = binding.get()?.let(::BoundAccount)

    /** Whether [account] is still the bound one. */
    public fun isBound(account: BoundAccount): Boolean = binding.get() === account.token

    // -----------------------------------------------------------------
    // ADR-0074 section 3.5a - the mailbox after a key rotation
    // -----------------------------------------------------------------

    private fun links(): List<me.lakko.app.contacts.ProvenLink> {
        val b = binding.get() ?: return emptyList()
        return try {
            provenLinks(b.accountIdB64)
        } catch (_: Throwable) {
            emptyList()
        }
    }

    /**
     * Rule 1 - the account key whose mailbox is addressed for a person known
     * by [accountKey] (a leaf's binding, a stored contact row, this account's
     * own conversation key): the key its PROVEN steps lead to, else the key
     * itself. Every place that derives a mailbox from an account key goes
     * through this (or [mailboxOf]) first.
     */
    public fun mailboxAccountKey(accountKey: ByteArray): ByteArray = addressedKey(accountKey, links())

    /** [mailboxAccountKey] over lowercase hex. */
    public fun mailboxAccountKeyHex(accountKeyHex: String): String = addressedKeyHex(accountKeyHex, links())

    /**
     * ADR-0078 - the VERIFIED mailbox key of the person known by
     * [accountKey], or `null` when none is known (nothing can be written to
     * them then). This account's own key for this account (a proven
     * predecessor included). For anyone else: the binding of the first grant
     * held for the person that verifies against their CURRENT account key
     * ([mailboxAccountKey]); a grant whose binding does not verify is skipped
     * (one filed under a predecessor names the old mailbox, which is never
     * addressed). The address is never derived from the identity key.
     */
    public fun mailboxPkFor(accountKey: ByteArray): ByteArray? {
        val b = binding.get() ?: return null
        val current = mailboxAccountKey(accountKey)
        if (current.contentEquals(b.accountKey)) return b.mailboxPk.copyOf()
        return firstVerifiedMailboxPk(heldFor(accountKey).map { it.second }, current, mailboxCrypto)
    }

    /** The address [mailboxPkFor] names, or `null`. */
    public fun mailboxAddressFor(accountKey: ByteArray): ByteArray? =
        mailboxPkFor(accountKey)?.let(mailboxCrypto::address)

    /** Rule 2 - every key the person is filed under, current first. */
    private fun personKeys(accountKey: ByteArray): List<String> = personKeysHex(reachHex(accountKey), links())

    /** Rule 3 - whether the profile is still owed to the conversations
     *  under the bound key. */
    public fun profileResendOwed(): Boolean {
        val b = binding.get() ?: return false
        return doc.profileResendOwedForKeyHex == reachHex(b.accountKey)
    }

    /** Rule 3 - every conversation took the re-sent profile. */
    public suspend fun profileResendDone() {
        val b = binding.get() ?: return
        lock.withLock {
            if (binding.get() !== b || doc.profileResendOwedForKeyHex == null) return@withLock
            doc = doc.copy(profileResendOwedForKeyHex = null)
            store.save(b.accountIdB64, doc)
        }
    }

    /** Rule 5 - whether this device still has to spend what it granted
     *  before the keys moved to the bound key (durable; asked at each
     *  session start, on the rotating device and on a kept one). */
    public fun grantsSpendOwed(): Boolean {
        val b = binding.get() ?: return false
        return spendRoundOwed(doc.grantsSpendOwedForKeyHex, reachHex(b.accountKey))
    }

    /** Rule 5 - the spend round found nothing left to do. */
    public suspend fun grantsSpendDone() {
        val b = binding.get() ?: return
        lock.withLock {
            if (binding.get() !== b || doc.grantsSpendOwedForKeyHex == null) return@withLock
            doc = doc.copy(grantsSpendOwedForKeyHex = null)
            store.save(b.accountIdB64, doc)
        }
    }

    /** Rule 5 - the ledger ids (hex) of the links this device minted
     *  before the account's keys moved, still to be revoked. */
    public fun spentLinkBlobsHex(): List<String> = doc.spentLinks.mapNotNull { it.blobIdHex }

    /** Whether any link minted before the keys moved is still recorded. */
    public fun hasSpentLinks(): Boolean = doc.spentLinks.isNotEmpty()

    /** Rule 5 - the spent links were dealt with (revoked where a ledger row
     *  existed): forget them. */
    public suspend fun spentLinksDone() {
        val b = binding.get() ?: return
        lock.withLock {
            if (binding.get() !== b || doc.spentLinks.isEmpty()) return@withLock
            doc = doc.copy(spentLinks = emptyList())
            store.save(b.accountIdB64, doc)
        }
    }

    /** Rule 6 - the address of the person known by [ownerKey] is known
     *  current (their identity bundle just passed the gate): a grant kept
     *  beside a newer one of the same class is dropped. */
    public suspend fun addressKnownCurrent(ownerKey: ByteArray) {
        val b = binding.get() ?: return
        val keys = personKeys(ownerKey)
        lock.withLock {
            if (binding.get() !== b) return@withLock
            val next = withoutPreviousGrants(doc.held, keys)
            if (next === doc.held) return@withLock
            doc = doc.copy(held = next)
            store.save(b.accountIdB64, doc)
        }
    }

    // -----------------------------------------------------------------
    // The owner's keys
    // -----------------------------------------------------------------

    /** The owner's current grant for [kind] over [scope] (empty for
     *  `contact` / `fof`), or `null` before a session binds. SECRET. */
    public fun grant(kind: ReachKind, scope: ByteArray = ByteArray(0)): FfiReachGrant? {
        val b = binding.get() ?: return null
        val slot = ReachSlot(kind, if (kind.scopeless) "" else reachHex(scope))
        return derive(b, slot, versionOf(doc, slot))
    }

    /**
     * A fresh `reply` grant for a request going to [toKey] (ADR-0060 §6),
     * registered with its seven-day expiry BEFORE this returns: a request
     * whose reply key is not on the server would be answered into a closed
     * mailbox. Throws when the registration does not land.
     */
    public suspend fun mintReply(toKey: ByteArray?, nowMs: Long = System.currentTimeMillis()): FfiReachGrant {
        val b = binding.get() ?: throw IllegalStateException("no session")
        val scope = ByteArray(REPLY_SCOPE_LEN).also(random::nextBytes)
        val scopeHex = reachHex(scope)
        lock.withLock {
            if (binding.get() !== b) throw IllegalStateException("no session")
            doc = doc.copy(
                replies = doc.replies + StoredReply(scopeHex, replyExpiry(nowMs), toKey?.let(::reachHex)),
            )
            store.save(b.accountIdB64, doc)
            if (!syncLocked(b, nowMs)) throw IllegalStateException("reply key not registered")
        }
        return derive(b, ReachSlot(ReachKind.REPLY, scopeHex), 0)
    }

    /** An answer arrived from [fromKey] (their ContactAck, a Welcome from
     *  them): retire every reply key waiting on them. */
    public fun retireRepliesTo(fromKey: ByteArray) {
        val b = binding.get() ?: return
        // ADR-0074 3.5a rule 2: the person, under whichever of their keys
        // the request was addressed to.
        val keysHex = personKeys(fromKey)
        scope.launch {
            lock.withLock {
                if (binding.get() !== b) return@withLock
                val kept = doc.replies.filterNot { it.toKeyHex in keysHex }
                if (kept.size == doc.replies.size) return@withLock
                doc = doc.copy(replies = kept)
                store.save(b.accountIdB64, doc)
                syncLocked(b, System.currentTimeMillis())
            }
        }
    }

    /**
     * The `link` grant for a new invite link [linkId] (`generateLinkId()`),
     * registered BEFORE this returns (with the link's expiry, `null` = never):
     * a link shared before its key is on the server would be refused.
     */
    public suspend fun mintLink(linkId: ByteArray, expiresAtMs: Long?): FfiReachGrant {
        val b = binding.get() ?: throw IllegalStateException("no session")
        val linkHex = reachHex(linkId)
        lock.withLock {
            if (binding.get() !== b) throw IllegalStateException("no session")
            doc = doc.copy(links = doc.links.filterNot { it.linkIdHex == linkHex } + StoredLink(linkHex, expiresAtMs))
            store.save(b.accountIdB64, doc)
            if (!syncLocked(b, System.currentTimeMillis())) throw IllegalStateException("link key not registered")
        }
        return derive(b, ReachSlot(ReachKind.LINK, linkHex), versionOf(doc, ReachSlot(ReachKind.LINK, linkHex)))
    }

    /** Remember the ledger id [linkId] was uploaded under, so a revoke by
     *  ledger id finds its key. */
    public fun noteLinkBlob(linkId: ByteArray, blobId: ByteArray) {
        val b = binding.get() ?: return
        val linkHex = reachHex(linkId)
        scope.launch {
            lock.withLock {
                if (binding.get() !== b) return@withLock
                doc = doc.copy(
                    links = doc.links.map { if (it.linkIdHex == linkHex) it.copy(blobIdHex = reachHex(blobId)) else it },
                )
                store.save(b.accountIdB64, doc)
            }
        }
    }

    /** A link uploaded under [blobId] was revoked: its key comes off. */
    public suspend fun retireLinkByBlob(blobId: ByteArray) {
        val blobHex = reachHex(blobId)
        val link = doc.links.firstOrNull { it.blobIdHex == blobHex } ?: return
        reachUnhex(link.linkIdHex)?.let { retireLink(it) }
    }

    /** A link was revoked or expired: its key comes off the mailbox. */
    public suspend fun retireLink(linkId: ByteArray) {
        val b = binding.get() ?: return
        val linkHex = reachHex(linkId)
        lock.withLock {
            if (binding.get() !== b) return@withLock
            doc = doc.copy(links = doc.links.filterNot { it.linkIdHex == linkHex })
            store.save(b.accountIdB64, doc)
            syncLocked(b, System.currentTimeMillis())
        }
    }

    /** The conversation groups this account is in: while it is in any, the
     *  one `member` key is registered, and the setting decides the `group`
     *  key. A group LEFT since the last list rotates both (ADR-0078 section
     *  9) and republishes the profile in every remaining group. */
    public fun setJoinedGroups(groups: Set<String>) {
        val b = binding.get() ?: return
        val normalised = groups.map { it.lowercase() }.sorted()
        if (normalised == doc.joinedGroups) return
        scope.launch {
            val envelope = lock.withLock {
                if (binding.get() !== b) return@launch
                val left = reachDeparted(doc.joinedGroups, normalised)
                // An empty list is "not loaded yet": the last one stays.
                if (normalised.isNotEmpty() || doc.joinedGroups.isEmpty()) {
                    doc = doc.copy(joinedGroups = normalised)
                }
                val envelope = if (left) bumpLocked(listOf(ReachKind.GROUP, ReachKind.MEMBER)) else null
                store.save(b.accountIdB64, doc)
                syncLocked(b, System.currentTimeMillis())
                envelope
            }
            afterRotation(b, envelope)
        }
    }

    /** The channels this account is in: the setting decides the one
     *  `channel` key. A channel LEFT since the last list rotates it (ADR-0078
     *  section 9). */
    public fun setChannels(channels: Set<String>) {
        val b = binding.get() ?: return
        val normalised = channels.map { it.lowercase() }.sorted()
        if (normalised == doc.channels) return
        scope.launch {
            val envelope = lock.withLock {
                if (binding.get() !== b) return@launch
                val left = reachDeparted(doc.channels, normalised)
                if (normalised.isNotEmpty() || doc.channels.isEmpty()) {
                    doc = doc.copy(channels = normalised)
                }
                val envelope = if (left) bumpLocked(listOf(ReachKind.CHANNEL)) else null
                store.save(b.accountIdB64, doc)
                syncLocked(b, System.currentTimeMillis())
                envelope
            }
            afterRotation(b, envelope)
        }
    }

    /**
     * Rotate a class (ADR-0060 §1): bump its version, so the old key comes
     * off the mailbox and a new one goes on. `contact` and `fof` when a
     * contact is removed. [scopeHex] matters only for `link` / `reply`.
     */
    public suspend fun rotate(kind: ReachKind, scopeHex: String = "") {
        val b = binding.get() ?: return
        val slot = ReachSlot(kind, if (kind.scopeless) "" else scopeHex.lowercase())
        val envelope = lock.withLock {
            if (binding.get() !== b) return
            val next = versionOf(doc, slot) + 1
            doc = doc.copy(
                versions = doc.versions.filterNot { it.kind == kind.name && it.scopeHex == slot.scopeHex } +
                    StoredVersion(kind.name, slot.scopeHex, next),
            )
            val envelope = stampReachStateLocked()
            store.save(b.accountIdB64, doc)
            syncLocked(b, System.currentTimeMillis())
            envelope
        }
        // An envelope composed for an account that is gone is not sent
        // through the next account's devices.
        if (binding.get() !== b) return
        envelope?.let(::publishToDevices)
    }

    /**
     * Rotate several account-wide classes at once (ADR-0078 section 9): one
     * stamp, one registration, one hand-over to the other devices, then the
     * profiles are republished in every group with the new grants. A group
     * that shrank rotates `group` and `member` this way, strictly after the
     * remove commit (C-61 shape).
     */
    public suspend fun rotateClasses(kinds: List<ReachKind>) {
        val b = binding.get() ?: return
        val envelope = lock.withLock {
            if (binding.get() !== b) return
            val envelope = bumpLocked(kinds)
            store.save(b.accountIdB64, doc)
            syncLocked(b, System.currentTimeMillis())
            envelope
        }
        afterRotation(b, envelope)
    }

    /** Bump each account-wide class in [kinds] once and stamp the state.
     *  Under [lock]; the caller saves and syncs. */
    private fun bumpLocked(kinds: List<ReachKind>): ByteArray? {
        for (kind in kinds.distinct()) {
            if (!kind.scopeless) continue
            val slot = ReachSlot(kind, "")
            val next = versionOf(doc, slot) + 1
            doc = doc.copy(
                versions = doc.versions.filterNot { it.kind == kind.name && it.scopeHex == "" } +
                    StoredVersion(kind.name, "", next),
            )
        }
        return stampReachStateLocked()
    }

    /** After a class rotation: the state goes to the other devices and the
     *  profiles (with the new grants) to every group. */
    private suspend fun afterRotation(b: Binding, envelope: ByteArray?) {
        if (envelope == null || binding.get() !== b) return
        publishToDevices(envelope)
        currentVisibility()?.let { rerunAfterAdopt(it) }
    }

    /** Compose the counters now held as a ReachState stamped strictly after
     *  the last one, and keep it as the state in force. Under [lock]. */
    private fun stampReachStateLocked(): ByteArray? {
        val previous = storedReachStateStamp()
        val state = FfiReachState(
            entries = reachStateEntries(versionsOf(doc)),
            updatedAtMs = nextSyncStamp(System.currentTimeMillis(), previous),
        )
        val envelope = try {
            uniffi.lakko_ffi_uniffi.composeReachStateEnvelope(state)
        } catch (cause: Throwable) {
            Log.w(LOG_TAG, "reach state not composed: ${cause.javaClass.simpleName}")
            return null
        }
        doc = doc.copy(reachStateHex = reachHex(envelope))
        return envelope
    }

    private fun storedReachStateStamp(): ULong? = doc.reachStateHex?.let(::reachUnhex)?.let { wire ->
        runCatching { uniffi.lakko_ffi_uniffi.decodeReachStateEnvelope(wire).updatedAtMs }.getOrNull()
    }

    private fun versionsOf(doc: ReachDoc): Map<ReachSlot, Int> = doc.versions.mapNotNull { v ->
        val kind = runCatching { ReachKind.valueOf(v.kind) }.getOrNull() ?: return@mapNotNull null
        ReachSlot(kind, v.scopeHex) to v.version
    }.toMap()

    private fun publishToDevices(envelope: ByteArray) {
        val publisher = deviceSyncPublisher
        if (publisher == null) {
            Log.i(LOG_TAG, "device sync not running: change kept on this device")
            return
        }
        try {
            publisher(envelope)
        } catch (cause: Throwable) {
            Log.w(LOG_TAG, "device sync send failed: ${cause.javaClass.simpleName}")
        }
    }

    /**
     * A device was just admitted to the DeviceGroup: send it the envelopes in
     * force here through [deviceSyncPublisher], the sync runtime's own
     * DeviceGroup handle. Never through a second handle of the same
     * DeviceGroup: two handles sealing at one sender generation reuse a key
     * and nonce, and the receiver refuses the second as a reuse.
     */
    public fun resendToDevices() {
        deviceSyncEnvelopes().forEach(::publishToDevices)
    }

    /** The Visibility and ReachState envelopes in force here, verbatim with
     *  their original stamps, and the own profile envelope with its name's
     *  stored stamp (ADR-0048 section 4a): what this device sends a device it
     *  admits to the DeviceGroup (ADR-0060 §7). Empty for what was never set. */
    public fun deviceSyncEnvelopes(): List<ByteArray> {
        val b = binding.get() ?: return emptyList()
        return deviceSyncEnvelopesToSend(doc.visibilityHex, doc.reachStateHex, ownProfileEnvelope(b))
    }

    /**
     * ADR-0048 section 4a — the own name was saved or cleared on this device:
     * send the own profile envelope to the account's other devices, with the
     * name's stored stamp. Nothing is sent when no name was ever saved, or
     * before a session binds (the hand-over to a device admitted later
     * carries it through [deviceSyncEnvelopes]).
     */
    public fun publishOwnProfileToDevices() {
        val b = binding.get() ?: return
        ownProfileEnvelope(b)?.let(::publishToDevices)
    }

    private fun ownProfileEnvelope(b: Binding): ByteArray? {
        val profiles = ownProfiles ?: return null
        val accountId = rawAccountId(b) ?: return null
        return me.lakko.app.profile.ownProfileDeviceEnvelope(profiles.load(accountId), accountId) {
            uniffi.lakko_ffi_uniffi.composeProfileEnvelope(it)
        }
    }

    private fun rawAccountId(b: Binding): ByteArray? =
        runCatching { me.lakko.app.auth.base64Decode(b.accountIdB64) }.getOrNull()

    /**
     * A DeviceGroup service envelope from another of the account's devices
     * (ADR-0060 §7). Kept only when it is a Visibility or a ReachState
     * STRICTLY newer than the one held; anything else is dropped. On
     * acceptance the mailbox is reconciled and [onVisibilityChanged] re-runs
     * what a local change would have (profiles, Everyone, introductions).
     * Before a session binds, it waits for [bind].
     */
    public fun adoptDeviceSync(envelope: ByteArray) {
        val b = binding.get()
        if (b == null) {
            pendingDeviceSync.add(envelope.copyOf())
            return
        }
        when (val received = classifyDeviceSync(envelope) { uniffi.lakko_ffi_uniffi.dispatchIncomingEnvelope(it) }) {
            is ReceivedDeviceSync.Visibility -> scope.launch { adoptVisibility(b, received) }
            is ReceivedDeviceSync.ReachState -> scope.launch { adoptReachState(b, received) }
            is ReceivedDeviceSync.Profile -> scope.launch { adoptOwnProfile(b, received) }
            ReceivedDeviceSync.Foreign -> Log.i(LOG_TAG, "device sync: foreign envelope dropped")
        }
    }

    /**
     * The visibility envelope a restored backup carried: the path an incoming
     * DeviceGroup envelope takes, so the mailbox and the Everyone publication
     * run. On a phrase route no session is bound yet and it waits for [bind];
     * a session of another account never takes it.
     */
    public fun adoptFromBackup(accountIdB64: String, envelope: ByteArray) {
        val bound = binding.get()
        if (bound != null && bound.accountIdB64 != accountIdB64) return
        if (bound == null) restoredVisibility[accountIdB64] = envelope.copyOf()
        adoptDeviceSync(envelope)
    }

    /**
     * A backup of [accountIdB64] was restored here: what this device noted
     * about the Everyone publication describes an earlier state of the
     * account, so the next refresh tells the server what the setting says.
     */
    public fun noteRestoredFromBackup(accountIdB64: String) {
        val b = binding.get()
        if (b == null || b.accountIdB64 != accountIdB64) {
            restoredBeforeBind.add(accountIdB64)
            return
        }
        scope.launch {
            lock.withLock {
                if (binding.get() !== b) return@withLock
                doc = doc.withUnknownEveryoneNote()
                store.save(b.accountIdB64, doc)
            }
        }
    }

    /** The visibility envelope in force for [accountIdB64] with its original
     *  stamp, for a backup; `null` when none was ever chosen. */
    public fun visibilityEnvelopeFor(accountIdB64: String): ByteArray? {
        val bound = binding.get()
        val held = if (bound?.accountIdB64 == accountIdB64) doc else store.load(accountIdB64)
        return held.visibilityHex?.let(::reachUnhex) ?: restoredVisibility[accountIdB64]?.copyOf()
    }

    private suspend fun adoptVisibility(b: Binding, received: ReceivedDeviceSync.Visibility) {
        val accepted = lock.withLock {
            if (binding.get() !== b) return
            val held = currentVisibility()?.updatedAtMs
            if (!keepsIncoming(received.setting.updatedAtMs, held) { i, s ->
                    uniffi.lakko_ffi_uniffi.visibilityIsNewer(i, s)
                }
            ) {
                return
            }
            // Kept verbatim: a later re-send carries the original stamp.
            doc = doc.copy(visibilityHex = reachHex(received.wire))
            mutableVisibility.value = received.setting
            store.save(b.accountIdB64, doc)
            syncLocked(b, System.currentTimeMillis())
            received.setting
        }
        rerunAfterAdopt(accepted)
    }

    /** ADR-0048 section 4a — keep the own name from a sibling when its stamp
     *  is strictly newer; one naming another account is dropped. Not
     *  republished to groups: the device that made the change already has. */
    private suspend fun adoptOwnProfile(b: Binding, received: ReceivedDeviceSync.Profile) {
        val profiles = ownProfiles ?: return
        lock.withLock {
            if (binding.get() !== b) return
            val accountId = rawAccountId(b) ?: return
            val adopted = profiles.update(accountId) { stored ->
                me.lakko.app.profile.adoptOwnProfileFromDevice(stored, received.profile, accountId)
            }
            if (!adopted) {
                Log.i(LOG_TAG, "device sync: own profile not newer or not this account's")
                return
            }
        }
        onOwnNameAdopted?.invoke()
    }

    private suspend fun adoptReachState(b: Binding, received: ReceivedDeviceSync.ReachState) {
        val adoption = lock.withLock {
            if (binding.get() !== b) return
            val decided = adoptReachStateMerge(
                stored = storedReachState(),
                incoming = received.state,
                nowMs = System.currentTimeMillis(),
                merge = { stored, incoming, now -> uniffi.lakko_ffi_uniffi.mergeReachState(stored, incoming, now) },
                compose = { uniffi.lakko_ffi_uniffi.composeReachStateEnvelope(it) },
            )
            val versions = decided.versions
            val wire = decided.storeWire
            if (versions != null && wire != null) {
                val fofSlot = ReachSlot(ReachKind.FOF, "")
                val fofBefore = versionOf(doc, fofSlot)
                // Persisted exactly as the core returned it, stamp included.
                doc = doc.copy(
                    versions = versions.map { (slot, v) -> StoredVersion(slot.kind.name, slot.scopeHex, v) },
                    reachStateHex = reachHex(wire),
                )
                // ADR-0060 §4 — the other device rotated `fof` (its schedule
                // or a removal): this one restarts its schedule from now.
                doc = doc.copy(
                    fofRotatedAtMs = fofRotatedAtAfterAdopt(
                        fofBefore = fofBefore,
                        fofAfter = versionOf(doc, fofSlot),
                        fofRotatedAtMs = doc.fofRotatedAtMs,
                        nowMs = System.currentTimeMillis(),
                    ),
                )
                store.save(b.accountIdB64, doc)
                syncLocked(b, System.currentTimeMillis())
            }
            decided
        }
        if (binding.get() !== b) return
        // Sent as returned, never re-stamped here.
        adoption.sendWire?.let(::publishToDevices)
        if (adoption.storeWire != null) currentVisibility()?.let { rerunAfterAdopt(it) }
    }

    /** The ReachState in force here: the stored envelope, or (never set) the
     *  counters held at stamp 0. */
    private fun storedReachState(): FfiReachState =
        doc.reachStateHex?.let(::reachUnhex)?.let { wire ->
            runCatching { uniffi.lakko_ffi_uniffi.decodeReachStateEnvelope(wire) }.getOrNull()
        } ?: FfiReachState(entries = reachStateEntries(versionsOf(doc)), updatedAtMs = 0uL)

    private suspend fun rerunAfterAdopt(setting: FfiVisibilitySetting) {
        try {
            onVisibilityChanged?.invoke(setting)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (cause: Throwable) {
            Log.w(LOG_TAG, "applying a synced change failed: ${cause.javaClass.simpleName}")
        }
    }

    /** Reconcile the mailbox with the desired key set, in the background.
     *  A due scheduled `fof` rotation ([rotateFofIfDue]) goes first. */
    public fun launchSync() {
        val b = binding.get() ?: return
        scope.launch {
            if (rotateFofIfDue(b)) return@launch
            lock.withLock { syncLocked(b, System.currentTimeMillis()) }
        }
    }

    /**
     * The app became active (MainActivity.onResume): run the scheduled `fof`
     * rotation if it is due. This and [launchSync] (every bind, so every
     * start) are the only places it fires; there is no background job of its
     * own, so a schedule that came due while the app was closed fires here.
     */
    public fun onAppActive() {
        val b = binding.get() ?: return
        scope.launch { rotateFofIfDue(b) }
    }

    /**
     * ADR-0060 §4, "Narrowing" — the `fof` key rotates every
     * [FOF_ROTATION_MS] as well as on a contact's removal, so a person who
     * stopped being a contact of a contact stops finding this account once
     * the relayed record ages out. As on web: a new `fof` version, the old
     * key unregistered at once (the reconcile inside), the ReachState to the
     * other devices, and the profiles and the introduction record (whose offer
     * key carries the `fof` version) to the contacts. The schedule is
     * persisted in [ReachDoc.fofRotatedAtMs], so it survives a restart.
     *
     * The first pass on a device (no "rotated at" held: install, restore,
     * pairing) only starts the schedule. Waits for the version recovery of a
     * fresh install, so it never establishes a version without looking at
     * the mailbox. Returns `true` when it rotated (and reconciled).
     */
    private suspend fun rotateFofIfDue(b: Binding): Boolean {
        val envelope = lock.withLock {
            if (binding.get() !== b) return false
            val now = System.currentTimeMillis()
            // The first pass on this device (install, restore, pairing) only
            // starts the schedule; it rotates nothing.
            fofScheduleStart(doc.fofRotatedAtMs, now)?.let { start ->
                doc = doc.copy(fofRotatedAtMs = start)
                store.save(b.accountIdB64, doc)
                return false
            }
            if (!fofRotationDue(doc.fofRotatedAtMs, now)) return false
            if (ensureVersionRecoveryLocked(b) != null) return false
            if (binding.get() !== b) return false
            doc = withScheduledFofRotation(doc, now)
            val envelope = stampReachStateLocked()
            store.save(b.accountIdB64, doc)
            Log.i(LOG_TAG, "fof key rotated on its schedule")
            syncLocked(b, now)
            envelope
        }
        afterRotation(b, envelope)
        return true
    }

    /** Reconcile now. `true` when the mailbox holds the desired set. */
    public suspend fun sync(): Boolean {
        val b = binding.get() ?: return false
        return lock.withLock { syncLocked(b, System.currentTimeMillis()) }
    }

    /** The version counters, as the ReachState envelope carries them, with
     *  the stamp of the state in force (0 before any rotation). */
    public fun reachState(): FfiReachState = FfiReachState(
        entries = reachStateEntries(versionsOf(doc)),
        updatedAtMs = storedReachStateStamp() ?: 0uL,
    )

    private suspend fun syncLocked(b: Binding, nowMs: Long): Boolean {
        if (binding.get() !== b) return false
        val recovery = ensureVersionRecoveryLocked(b)
        if (binding.get() !== b) return false
        val setting = mutableVisibility.value ?: uniffi.lakko_ffi_uniffi.defaultVisibilitySetting()
        val inputs = inputsFor(doc, setting)
        val wanted = desiredReachKeys(inputs, nowMs).let {
            if (recovery == null) it else registrableBeforeRecovery(it)
        }
        val desired = wanted.associateBy { key ->
            reachHex(publicKeyOf(derive(b, key.slot, key.version).seed))
        }
        val registered = doc.registered.mapNotNull { r ->
            val kind = runCatching { ReachKind.valueOf(r.kind) }.getOrNull() ?: return@mapNotNull null
            r.pkHex to RegisteredReachKey(ReachSlot(kind, r.scopeHex), r.version, r.expiresAtMs)
        }.toMap()
        val diff = diffReachKeys(desired, registered)
        val wantOpen = setting.mode == FfiVisibilityMode.EVERYONE
        // The Everyone flag waits for the recovery too: it is part of the
        // setting the versions are reconciled with.
        val openChange = if (recovery != null || doc.registeredOpen == wantOpen) null else wantOpen
        // Expired replies drop out of the document once the diff removed them.
        val liveReplies = doc.replies.filter { it.expiresAtMs > nowMs }
        if (diff.isEmpty && openChange == null) {
            if (liveReplies.size != doc.replies.size) {
                doc = doc.copy(replies = liveReplies)
                store.save(b.accountIdB64, doc)
            }
            mutableStatus.value = settledStatus(recovery)
            // While the recovery is pending this reports the restricted set
            // (`link` / `reply` keys only), so a link or reply mint proceeds;
            // the status says the rest is not applied.
            return true
        }
        mutableStatus.value = ReachApplyStatus.APPLYING
        return try {
            dsFactory().use { ds ->
                ds.putMailboxReach(
                    pseudonym = b.mailbox,
                    signer = b.mailboxSigner,
                    add = diff.add.map { (pk, key) -> DsReachKeyAdd(reachUnhex(pk)!!, key.expiresAtMs) },
                    remove = diff.remove.mapNotNull(::reachUnhex),
                    open = openChange,
                )
            }
            // The account's keys were rotated while this ran (what it
            // registered is on the OLD mailbox and is not recorded), or
            // the account signed out (or another bound) while the request was
            // out: [doc] is no longer this account's document. Writing it
            // here would save an empty or a foreign document under this
            // account, losing its setting and its version counters.
            if (binding.get() !== b) return false
            doc = doc.copy(
                registered = desired.map { (pk, key) ->
                    StoredRegistered(pk, key.slot.kind.name, key.slot.scopeHex, key.version, key.expiresAtMs)
                },
                registeredOpen = if (openChange == null) doc.registeredOpen else wantOpen,
                replies = liveReplies,
            )
            store.save(b.accountIdB64, doc)
            mutableStatus.value = settledStatus(recovery)
            // Pending: only the restricted set went out (see above).
            true
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (cause: DsNetworkError) {
            Log.i(LOG_TAG, "reach keys not registered yet: offline")
            mutableStatus.value = ReachApplyStatus.OFFLINE
            false
        } catch (cause: Throwable) {
            Log.w(LOG_TAG, "reach keys not registered: ${cause.javaClass.simpleName}")
            mutableStatus.value = ReachApplyStatus.FAILED
            false
        }
    }

    /** What the reconcile leaves on screen: the recovery's status, a stuck
     *  Everyone publication, or applied. */
    private fun settledStatus(recovery: ReachApplyStatus?): ReachApplyStatus =
        recovery ?: if (everyoneStuck) ReachApplyStatus.FAILED else ReachApplyStatus.APPLIED

    /**
     * ADR-0060 §1 ("Versions after a restore", amended 2026-10-01). Under [lock].
     *
     * A device that holds no version state ([holdsNoVersionState]: a fresh
     * install, or a restore with no other device alive) must not start every
     * class at version 0, or a key rotated away (a removed contact, an evicted
     * member) would be registered again. So before it registers anything
     * versioned it asks the DS which keys the mailbox holds (owner-only, the
     * mailbox proof) and each slot resumes at the highest version whose key is
     * listed ([recoverVersions]); a slot with none starts at 0.
     *
     * Returns `null` when versioned keys may be registered, or the status to
     * show while the read has not succeeded. Fails CLOSED meanwhile: no
     * versioned key and no Everyone flag go out (only `link` / `reply` keys,
     * which carry no history); the keys the mailbox already holds stay as they
     * are on the server, so whoever holds a current grant can still write. The
     * read is retried after [VERSION_RECOVERY_RETRY_MS] and on every reconcile.
     *
     * Slots this device learns later (a group it is re-admitted to after the
     * restore) are recovered from the same server snapshot the first time they
     * appear without a version of their own.
     */
    private suspend fun ensureVersionRecoveryLocked(b: Binding): ReachApplyStatus? {
        var state = doc.versionRecovery?.let { runCatching { VersionRecovery.valueOf(it) }.getOrNull() }
        if (state == null) {
            state = if (holdsNoVersionState(
                    versionsEmpty = doc.versions.isEmpty(),
                    registeredEmpty = doc.registered.isEmpty(),
                    reachStateKnown = doc.reachStateHex != null,
                )
            ) {
                VersionRecovery.PENDING
            } else {
                VersionRecovery.DONE
            }
            doc = doc.copy(versionRecovery = state.name)
            store.save(b.accountIdB64, doc)
        }
        if (state == VersionRecovery.PENDING) {
            val listed = try {
                dsFactory().use { ds -> ds.fetchMailboxReach(b.mailbox, b.mailboxSigner) }
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (cause: Throwable) {
                Log.i(LOG_TAG, "reach versions not recovered yet (${cause.javaClass.simpleName}); retried later")
                scheduleRecoveryRetry(b)
                return if (cause is DsNetworkError) ReachApplyStatus.OFFLINE else ReachApplyStatus.FAILED
            }
            if (binding.get() !== b) return ReachApplyStatus.APPLYING
            doc = doc.copy(
                versionRecovery = VersionRecovery.DONE.name,
                recoveryPkHex = listed.keys.map { reachHex(it.publicKey) }.distinct(),
            )
            store.save(b.accountIdB64, doc)
            Log.i(LOG_TAG, "reach versions recovered from ${listed.keys.size} registered keys")
        }
        applyRecoveredSlotsLocked(b)
        return null
    }

    /** Recover the version of every nameable slot that has none yet and was
     *  not scanned before, from [ReachDoc.recoveryPkHex]. Under [lock]. */
    private fun applyRecoveredSlotsLocked(b: Binding) {
        if (doc.recoveryPkHex.isEmpty()) return
        val scanned = doc.recoveredSlots.toHashSet()
        val versioned = doc.versions.map { it.kind + ":" + it.scopeHex }.toHashSet()
        val todo = recoverableSlots(doc.joinedGroups, doc.channels).filter {
            val id = reachSlotId(it)
            id !in scanned && id !in versioned
        }
        if (todo.isEmpty()) return
        val found = recoverVersions(
            slots = todo,
            registered = doc.recoveryPkHex.toHashSet(),
            publicKeyHexAt = { slot, version ->
                runCatching { reachHex(publicKeyOf(derive(b, slot, version).seed)) }.getOrNull()
            },
        )
        doc = doc.copy(
            versions = doc.versions + found.filterValues { it > 0 }.map { (slot, v) ->
                StoredVersion(slot.kind.name, slot.scopeHex, v)
            },
            recoveredSlots = doc.recoveredSlots + todo.map(::reachSlotId),
        )
        store.save(b.accountIdB64, doc)
    }

    private val recoveryRetryScheduled = java.util.concurrent.atomic.AtomicBoolean(false)

    private fun scheduleRecoveryRetry(b: Binding) {
        if (!recoveryRetryScheduled.compareAndSet(false, true)) return
        scope.launch {
            try {
                kotlinx.coroutines.delay(VERSION_RECOVERY_RETRY_MS)
            } finally {
                recoveryRetryScheduled.set(false)
            }
            if (binding.get() === b) lock.withLock { syncLocked(b, System.currentTimeMillis()) }
        }
    }

    private fun inputsFor(doc: ReachDoc, setting: FfiVisibilitySetting): ReachInputs {
        val versions = doc.versions.mapNotNull { v ->
            val kind = runCatching { ReachKind.valueOf(v.kind) }.getOrNull() ?: return@mapNotNull null
            ReachSlot(kind, v.scopeHex) to v.version
        }.toMap()
        val joined = doc.joinedGroups.toSet()
        val visibleGroups = joined.filter { hex ->
            val id = reachUnhex(hex) ?: return@filter false
            allows(setting, FfiScopeKind.GROUP, id)
        }.toSet()
        val visibleChannels = doc.channels.filter { hex ->
            val id = reachUnhex(hex) ?: return@filter false
            allows(setting, FfiScopeKind.CHANNEL, id)
        }.toSet()
        return ReachInputs(
            versions = versions,
            joinedGroups = joined,
            visibleGroups = visibleGroups,
            visibleChannels = visibleChannels,
            contactsOfContacts = runCatching { uniffi.lakko_ffi_uniffi.allowsContactsOfContacts(setting) }
                .getOrDefault(false),
            links = doc.links.associate { it.linkIdHex to it.expiresAtMs },
            replies = doc.replies.associate { it.scopeHex to it.expiresAtMs },
            registeredContactVersions = doc.registered
                .filter { it.kind == ReachKind.CONTACT.name && it.scopeHex == "" }
                .map { it.version }
                .toSet(),
        )
    }

    // -----------------------------------------------------------------
    // The setting
    // -----------------------------------------------------------------

    /** The setting now in force here (the default for an account that never
     *  chose), or `null` before a session binds. */
    public fun currentVisibility(): FfiVisibilitySetting? = mutableVisibility.value
        ?: binding.get()?.let { uniffi.lakko_ffi_uniffi.defaultVisibilitySetting() }

    /**
     * Apply a setting the person chose (UX section 3: takes effect at once, no
     * Save button). Stamped strictly after the one it replaces, persisted, the
     * mailbox reconciled, then [onVisibilityChanged] runs.
     *
     * ADR-0078 section 9, decision 4 — a change made HERE also rotates the
     * `group` and `channel` keys, so a group or channel the setting no longer
     * allows stops holding a working key (one key serves them all), and `fof`
     * when it narrowed which contacts get the record ([settingChangeRotation]).
     * A setting adopted from another device arrives with that device's bumped
     * counters and is not rotated again.
     */
    public suspend fun updateVisibility(chosen: FfiVisibilitySetting) {
        val b = binding.get() ?: return
        val previous = currentVisibility()
        val stamp = maxOf(System.currentTimeMillis().toULong(), (previous?.updatedAtMs ?: 0uL) + 1uL)
        val stamped = chosen.copy(updatedAtMs = stamp)
        val envelope = uniffi.lakko_ffi_uniffi.composeVisibilityEnvelope(stamped)
        // The chosen row shows at once (UX section 9, "Applying"); the server
        // catches up under the lock.
        mutableVisibility.value = stamped
        mutableStatus.value = ReachApplyStatus.APPLYING
        val stateEnvelope = lock.withLock {
            if (binding.get() !== b) return
            doc = doc.copy(visibilityHex = reachHex(envelope))
            val stateEnvelope =
                bumpLocked(settingChangeRotation(previous?.contactsReach(), stamped.contactsReach()))
            store.save(b.accountIdB64, doc)
            syncLocked(b, System.currentTimeMillis())
            stateEnvelope
        }
        // Signed out while the mailbox was reconciled: the setting is saved
        // for its account; nothing is sent as whoever is bound now.
        if (binding.get() !== b) return
        // ADR-0060 §7 — the account's other devices take the same setting,
        // and the rotated counters.
        publishToDevices(envelope)
        stateEnvelope?.let(::publishToDevices)
        try {
            onVisibilityChanged?.invoke(stamped)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (cause: Throwable) {
            Log.w(LOG_TAG, "applying the setting to profiles failed: ${cause.javaClass.simpleName}")
        }
    }

    /**
     * ADR-0060 section 1 (2026-11-06) — what to do with a mailbox row that
     * [provingKey] admitted, sent by someone who is ([senderIsContact]) or is
     * not a current contact: `PROCESS`, `PROCESS_AND_REGRANT` (a retired
     * contact key from a current contact) or `DROP` (a retired contact key
     * from anyone else). `PROCESS` with no session or a failure.
     */
    public fun mailboxRowVerdict(provingKey: ByteArray?, senderIsContact: Boolean): FfiContactKeyRowVerdict {
        val b = binding.get() ?: return FfiContactKeyRowVerdict.PROCESS
        return try {
            uniffi.lakko_ffi_uniffi.mailboxRowContactVerdict(
                b.accountSeed,
                versionOf(doc, ReachSlot(ReachKind.CONTACT, "")).toUInt(),
                provingKey,
                senderIsContact,
            )
        } catch (cause: Throwable) {
            Log.w(LOG_TAG, "mailbox row verdict failed: ${cause.javaClass.simpleName}")
            FfiContactKeyRowVerdict.PROCESS
        }
    }

    /** Whether [provingKey] is a retired `contact` key of this account (its
     *  row's sender must be known before it is kept). */
    public fun isRetiredContactKey(provingKey: ByteArray?): Boolean {
        val b = binding.get() ?: return false
        if (provingKey == null || provingKey.size != 32) return false
        return try {
            uniffi.lakko_ffi_uniffi.classifyContactKey(
                b.accountSeed,
                versionOf(doc, ReachSlot(ReachKind.CONTACT, "")).toUInt(),
                provingKey,
            ) is uniffi.lakko_ffi_uniffi.FfiContactKeyStanding.Retired
        } catch (_: Throwable) {
            false
        }
    }

    /** A removed contact leaves the chosen list (UX section 4.1). */
    public suspend fun forgetChosenContact(accountId: ByteArray) {
        val setting = currentVisibility() ?: return
        withoutChosenContact(setting, accountId)?.let { updateVisibility(it) }
    }

    /** Retry after a failure (the screen's Retry action). */
    public suspend fun retryApply() {
        sync()
        mutableVisibility.value?.let { setting ->
            try {
                onVisibilityChanged?.invoke(setting)
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (_: Throwable) {
                // Reported through the status.
            }
        }
    }

    /** Whether the setting lets the members of group [groupHex] find this
     *  person (their `group` grant and username go in the profile there). */
    public fun groupVisible(groupHex: String): Boolean {
        val setting = currentVisibility() ?: return false
        val id = reachUnhex(groupHex) ?: return false
        return allows(setting, FfiScopeKind.GROUP, id)
    }

    /** Every group and channel this account is in, as setting scopes (what
     *  "turning a group off under All" keeps on). */
    public fun knownScopes(): List<FfiScopeId> =
        doc.joinedGroups.mapNotNull { hex -> reachUnhex(hex)?.let { FfiScopeId(FfiScopeKind.GROUP, it) } } +
            doc.channels.mapNotNull { hex -> reachUnhex(hex)?.let { FfiScopeId(FfiScopeKind.CHANNEL, it) } }

    /** Whether the setting lets the members of channel [channelHex] find this person. */
    public fun channelVisible(channelHex: String): Boolean {
        val setting = currentVisibility() ?: return false
        val id = reachUnhex(channelHex) ?: return false
        return allows(setting, FfiScopeKind.CHANNEL, id)
    }

    /**
     * The grants and whether to state the username in a profile published in
     * [groupHex] (ADR-0060 §3): a one-to-one carries `contact` + `member` and
     * the username; a group carries `member` always, and `group` + the
     * username only where the setting lets its members find this person.
     */
    public fun profileGrants(groupHex: String, oneToOne: Boolean): ProfileGrants {
        val scope = reachUnhex(groupHex) ?: return ProfileGrants(emptyList(), shareUsername = oneToOne)
        val member = grant(ReachKind.MEMBER, scope)
        if (oneToOne) {
            return ProfileGrants(listOfNotNull(grant(ReachKind.CONTACT), member), shareUsername = true)
        }
        val visible = groupVisible(groupHex)
        return ProfileGrants(
            grants = listOfNotNull(if (visible) grant(ReachKind.GROUP, scope) else null, member),
            shareUsername = visible,
        )
    }

    /** The username certificate discovery signed for this account (ADR-0060
     *  §5), or `null` until one was fetched. */
    public fun usernameCertificate(): ByteArray? = doc.usernameCertHex?.let(::reachUnhex)

    /** ADR-0076 - the account was renamed: the certificate held here is the
     *  old name's. Dropped, so the current one is fetched again. */
    public fun dropUsernameCertificate() {
        val b = binding.get() ?: return
        if (doc.usernameCertHex == null) return
        doc = doc.copy(usernameCertHex = null)
        scope.launch {
            lock.withLock {
                doc = doc.copy(usernameCertHex = null)
                store.save(b.accountIdB64, doc)
            }
        }
    }

    /** Keep the certificate from `POST /discovery/register` or
     *  `GET /discovery/username-certificate`. */
    public fun keepUsernameCertificate(certificate: ByteArray): Boolean {
        val b = binding.get() ?: return false
        val hex = reachHex(certificate)
        if (doc.usernameCertHex == hex) return false
        doc = doc.copy(usernameCertHex = hex)
        scope.launch {
            lock.withLock {
                if (binding.get() !== b) return@withLock
                doc = doc.copy(usernameCertHex = hex)
                store.save(b.accountIdB64, doc)
            }
        }
        return true
    }

    /** Whether the Everyone publication was last sent as public, and for
     *  which [everyonePublishKey] (ADR-0060 §5). */
    public fun publishedEveryone(): Pair<Boolean?, String?> = doc.publishedPublic to doc.publishedKey

    /** Record what the Everyone publication now holds ([key] is `null` when
     *  not public). */
    /**
     * The OPRF round of the term [key], from what this account already holds
     * when it can be: each round costs an anonymous token, and an Everyone
     * publication that fails part way otherwise pays for every term again.
     * The lookup key is kept beside the output under `k:<key>`.
     */
    internal suspend fun keptOprf(key: String, round: suspend () -> OprfResult): OprfResult {
        val output = doc.termOutputs[key]?.let(::reachUnhex)
        val lookupKey = doc.termOutputs["k:$key"]?.let(::reachUnhex)
        if (output != null && lookupKey != null) return OprfResult(output, lookupKey)
        val result = waitingOutRateLimit(round)
        val b = binding.get() ?: return result
        lock.withLock {
            if (binding.get() === b) {
                val kept = mapOf(key to reachHex(result.output), "k:$key" to reachHex(result.lookupKey))
                doc = doc.copy(termOutputs = doc.termOutputs + kept)
                store.save(b.accountIdB64, doc)
            }
        }
        return result
    }

    /**
     * The OPRF rounds of [keys] at once: those this account holds come from
     * it, the rest are evaluated by ONE [batch] call (the own name's terms go
     * through `POST /discovery/oprf/batch`, one token for all) and kept.
     * Results in the order of [keys].
     */
    internal suspend fun keptOprfBatch(
        keys: List<String>,
        batch: suspend (missing: List<Int>) -> List<OprfResult>,
    ): List<OprfResult> {
        val held = keys.map { key ->
            val output = doc.termOutputs[key]?.let(::reachUnhex)
            val lookupKey = doc.termOutputs["k:$key"]?.let(::reachUnhex)
            if (output != null && lookupKey != null) OprfResult(output, lookupKey) else null
        }
        val missing = held.indices.filter { held[it] == null }
        if (missing.isEmpty()) return held.map { it!! }
        val fresh = waitingOutRateLimit { batch(missing) }
        val out = held.toMutableList()
        for ((i, at) in missing.withIndex()) out[at] = fresh[i]
        val b = binding.get()
        if (b != null) {
            lock.withLock {
                if (binding.get() === b) {
                    val kept = HashMap<String, String>()
                    for ((i, at) in missing.withIndex()) {
                        kept[keys[at]] = reachHex(fresh[i].output)
                        kept["k:${keys[at]}"] = reachHex(fresh[i].lookupKey)
                    }
                    doc = doc.copy(termOutputs = doc.termOutputs + kept)
                    store.save(b.accountIdB64, doc)
                }
            }
        }
        return out.map { it!! }
    }

    /** Whether the last Everyone publication did not land. */
    public fun everyonePublicationStuck(): Boolean = everyoneStuck

    /** The Everyone publication could not be made: the person sees it on the
     *  Who can find me screen, with Retry. */
    public fun noteEveryoneStuck() {
        everyoneStuck = true
        mutableStatus.value = ReachApplyStatus.FAILED
    }

    public fun notePublishedEveryone(public: Boolean, key: String?) {
        val b = binding.get() ?: return
        doc = doc.copy(publishedPublic = public, publishedKey = key)
        if (everyoneStuck) {
            everyoneStuck = false
            if (mutableStatus.value == ReachApplyStatus.FAILED) mutableStatus.value = ReachApplyStatus.APPLIED
        }
        scope.launch {
            lock.withLock {
                if (binding.get() !== b) return@withLock
                doc = doc.copy(publishedPublic = public, publishedKey = key)
                store.save(b.accountIdB64, doc)
            }
        }
    }

    // -----------------------------------------------------------------
    // Grants other people handed this device
    // -----------------------------------------------------------------

    /** Keep a grant [ownerKey] handed out, received at [scopeHex] (the group
     *  or channel id for those classes). The caller already ran
     *  [acceptReceivedGrant]. A newer version replaces an older one. */
    public fun keepHeld(ownerKey: ByteArray, grant: FfiReachGrant, scopeHex: String) {
        val b = binding.get() ?: return
        scope.launch { storeHeld(b, ownerKey, grant, scopeHex) }
    }

    /** [keepHeld], awaited: the grant is in the store when this returns
     *  (the member-card reader checks what it holds right after filing). */
    public suspend fun fileHeld(ownerKey: ByteArray, grant: FfiReachGrant, scopeHex: String) {
        val b = binding.get() ?: return
        storeHeld(b, ownerKey, grant, scopeHex)
    }

    private suspend fun storeHeld(b: Binding, ownerKey: ByteArray, grant: FfiReachGrant, scopeHex: String) {
        val kind = grant.`class`.toKind()
        // ADR-0074 3.5a rule 2: filed for the PERSON, i.e. under the current
        // key of the account, whichever key the leaf it arrived from names.
        val currentOwner = mailboxAccountKey(ownerKey)
        val ownerHex = reachHex(currentOwner)
        val scopeKey = if (kind == ReachKind.GROUP || kind == ReachKind.MEMBER || kind == ReachKind.CHANNEL) {
            scopeHex.lowercase()
        } else {
            ""
        }
        val wire = try {
            uniffi.lakko_ffi_uniffi.encodeReachGrant(grant)
        } catch (_: Throwable) {
            return
        }
        val changed = lock.withLock {
            if (binding.get() !== b) return@withLock false
            val existing = doc.held.firstOrNull {
                it.ownerKeyHex == ownerHex && it.kind == kind.name && it.scopeHex == scopeKey
            }
            if (existing != null && existing.grantHex == reachHex(wire)) return@withLock false
            if (existing != null) {
                val old = runCatching {
                    uniffi.lakko_ffi_uniffi.decodeReachGrant(reachUnhex(existing.grantHex)!!)
                }.getOrNull()
                if (old != null && old.version > grant.version) return@withLock false
            }
            // Rule 6: a grant this one replaces is kept beside it until the
            // person's address is known current.
            doc = doc.copy(held = heldWithGrant(doc.held, ownerHex, kind.name, scopeKey, reachHex(wire)))
            store.save(b.accountIdB64, doc)
            true
        }
        if (changed) notifyGrantHeld(currentOwner)
    }

    /** Drop the grant of [kind] held for [ownerKey] at [scopeHex] (a member
     *  card that no longer carries its `channel` grant, ADR-0060 §3). */
    public suspend fun withdrawHeld(ownerKey: ByteArray, kind: ReachKind, scopeHex: String) {
        val b = binding.get() ?: return
        val ownerKeys = personKeys(ownerKey)
        val scopeKey = scopeHex.lowercase()
        lock.withLock {
            if (binding.get() !== b) return@withLock
            val kept = doc.held.filterNot {
                it.ownerKeyHex in ownerKeys && it.kind == kind.name && it.scopeHex == scopeKey
            }
            if (kept.size == doc.held.size) return@withLock
            doc = doc.copy(held = kept)
            store.save(b.accountIdB64, doc)
        }
    }

    /** Drop the one grant [grantWire] of [kind] held for [ownerKey] (a join
     *  request's reply key, once the person is admitted or refused). */
    public suspend fun withdrawHeldGrant(ownerKey: ByteArray, kind: ReachKind, grantWire: ByteArray) {
        val b = binding.get() ?: return
        val ownerKeys = personKeys(ownerKey)
        lock.withLock {
            if (binding.get() !== b) return@withLock
            val kept = withoutHeldGrant(doc.held, ownerKeys, kind.name, reachHex(grantWire))
            if (kept === doc.held) return@withLock
            doc = doc.copy(held = kept)
            store.save(b.accountIdB64, doc)
        }
    }

    /** Whether a grant of [kind] is held for [ownerKey] at [scopeHex]. */
    public fun holdsHeld(ownerKey: ByteArray, kind: ReachKind, scopeHex: String): Boolean {
        val ownerKeys = personKeys(ownerKey)
        val scopeKey = scopeHex.lowercase()
        return doc.held.any { it.ownerKeyHex in ownerKeys && it.kind == kind.name && it.scopeHex == scopeKey }
    }

    /**
     * Every grant held for [ownerKey] that a contact request may use, as
     * wires, in the order they are tried ([contactRequestGrantsInOrder]). A
     * bare `member` key is never one: a co-member the person hides from holds
     * it (UX section 6.2). SECRET.
     */
    public fun heldGrantWiresInOrder(ownerKey: ByteArray): List<ByteArray> =
        contactRequestGrantsInOrder(heldFor(ownerKey)).mapNotNull { grant ->
            runCatching { uniffi.lakko_ffi_uniffi.encodeReachGrant(grant) }.getOrNull()
        }

    /** Drop every grant held for [ownerKey] (a contact removed). */
    public fun forgetHeld(ownerKey: ByteArray) {
        val b = binding.get() ?: return
        val ownerKeys = personKeys(ownerKey)
        scope.launch {
            lock.withLock {
                if (binding.get() !== b) return@withLock
                doc = doc.copy(held = doc.held.filterNot { it.ownerKeyHex in ownerKeys })
                store.save(b.accountIdB64, doc)
            }
        }
    }

    /** Forget the grants learned in the group [scopeHex] (ADR-0087 section 9).
     *  The same person's grants learned elsewhere stay: a row names its
     *  scope. */
    public fun forgetHeldScope(scopeHex: String) {
        val b = binding.get() ?: return
        val scoped = scopeHex.lowercase()
        scope.launch {
            lock.withLock {
                if (binding.get() !== b) return@withLock
                doc = doc.copy(held = doc.held.filterNot { it.scopeHex == scoped })
                store.save(b.accountIdB64, doc)
            }
        }
    }

    /**
     * The grants held for the person known by [ownerKey]. SECRET.
     *
     * ADR-0074 3.5a rule 2: found under the account's current key AND under
     * every proven predecessor of it, the current key's first, so a grant
     * filed before the key change was proven is still found and one issued
     * for the new mailbox is tried before an older one of the same class.
     */
    public fun heldFor(ownerKey: ByteArray): List<Pair<HeldGrantRef, FfiReachGrant>> {
        return heldForPerson(doc.held, personKeys(ownerKey)).flatMap { h ->
            val kind = runCatching { ReachKind.valueOf(h.kind) }.getOrNull() ?: return@flatMap emptyList()
            // Rule 6: the grant an entry replaced is still a candidate,
            // after the newer one.
            grantWiresOf(h).mapNotNull { wireHex ->
                val grant = runCatching { uniffi.lakko_ffi_uniffi.decodeReachGrant(reachUnhex(wireHex)!!) }
                    .getOrNull() ?: return@mapNotNull null
                HeldGrantRef(kind, h.scopeHex) to grant
            }
        }
    }

    /**
     * Where a write of [purpose] into [ownerKey]'s mailbox goes and what signs
     * it, or `null` when no grant held verifies (a proofless write then needs
     * [mailboxPkFor], which an open mailbox takes and a closed one drops).
     *
     * This account's own mailbox is written with its own `contact` key (the
     * account's other devices, ADR-0050). A Welcome into [groupHex] prefers a
     * committer delegation this device holds for that person there.
     */
    public fun reachAuthFor(
        ownerKey: ByteArray,
        purpose: WritePurpose,
        groupHex: String? = null,
    ): MailboxDest? {
        for (candidate in reachAuthCandidates(ownerKey, purpose, groupHex)) {
            candidate()?.let { return it }
        }
        return null
    }

    /**
     * ADR-0078 - where a write of [purpose] to [ownerKey] may go, in turn:
     * [first] (a grant or binding the caller holds for this write), each held
     * grant that verifies, and a proofless write to the mailbox key a held
     * binding names ([mailboxPkFor]; it spends one `mailbox-write` token), the
     * last only when nothing before it yielded a destination.
     */
    public fun writeDestCandidates(
        ownerKey: ByteArray,
        purpose: WritePurpose,
        first: MailboxDest? = null,
    ): List<() -> MailboxDest?> {
        val grants = reachAuthCandidates(ownerKey, purpose)
        val all = if (first == null) grants else listOf<() -> MailboxDest?>({ first }) + grants
        return withProoflessLast(all) { mailboxPkFor(ownerKey)?.let { MailboxDest(it, null) } }
    }

    /**
     * Every authorisation this device holds for a write of [purpose] into
     * [ownerKey]'s mailbox, in [reachAuthFor]'s order, each built only when
     * asked for (a signer is a native object). ADR-0050 section 2.5a item 7:
     * a Welcome writer tries them in turn and moves on after a `401`. This
     * account's own mailbox yields its own `contact` key alone. Empty before a
     * session binds or when nothing is held.
     */
    public fun reachAuthCandidates(
        ownerKey: ByteArray,
        purpose: WritePurpose,
        groupHex: String? = null,
    ): List<() -> MailboxDest?> {
        val b = binding.get() ?: return emptyList()
        // ADR-0074 3.5a rule 1: this account's own conversation key (a
        // proven predecessor after a rotation) IS this account.
        val current = mailboxAccountKey(ownerKey)
        if (current.contentEquals(b.accountKey)) {
            return listOf({
                grant(ReachKind.CONTACT)?.let { MailboxDest(b.mailboxPk.copyOf(), DsReachAuth(reachSigner(it.seed))) }
            })
        }
        val held = heldFor(ownerKey)
        val out = ArrayList<() -> MailboxDest?>()
        for (ref in orderedWriteGrants(held.map { it.first }, purpose, groupHex?.lowercase())) {
            // Rule 2: one class and scope may be held under the current key
            // and under a predecessor; each is a candidate, the current
            // key's first, and a `401` moves on to the next.
            val grants = held.filter { it.first == ref }.map { it.second }
            for ((index, grant) in grants.withIndex()) {
                if (grants.take(index).any { it.seed.contentEquals(grant.seed) }) continue
                // ADR-0078: the grant's binding is verified against the
                // person's CURRENT key before the write; a refused one
                // writes nothing (the candidate yields `null`).
                out += { verifiedGrantDest(grant, current, mailboxCrypto) { seed -> DsReachAuth(reachSigner(seed)) } }
            }
        }
        // ADR-0073 R.4 - the continuity Welcome to a person whose reset this
        // device accepted is a recorded no-op under ADR-0078: there is no
        // carrier for the reset account's new mailbox key (ADR-0078 open
        // question), and an address derived from the account id or the
        // identity key is exactly what ADR-0078 removed.
        if (purpose == WritePurpose.WELCOME) {
            out += { continuityWelcomeDest(runCatching { acceptedResetOf?.invoke(current) }.getOrNull() != null) }
        }
        if (purpose == WritePurpose.WELCOME && groupHex != null) {
            out += {
                // The write is signed with the own key the delegation
                // NAMES: the current one, or an own predecessor; it is
                // sealed to the mailbox the delegation's binding names,
                // verified against the added person's current key.
                delegationFor(groupHex, ownerKey)?.let { picked ->
                    mailboxCrypto.bindingMailboxPkOrNull(picked.addedMailboxBinding, current)?.let { pk ->
                        MailboxDest(pk, DsReachAuth(reachSigner(picked.signerSeed), delegationHeader = picked.header))
                    }
                }
            }
        }
        return out
    }

    /** ADR-0050 section 2.5a item 7 — told the owner key of a grant (or a
     *  committer delegation naming its account) this device just kept, so a
     *  Welcome owed to that person is retried at once. Installed by the chat
     *  repository; must not block. */
    @Volatile
    public var onGrantHeld: ((ownerKey: ByteArray) -> Unit)? = null

    private fun notifyGrantHeld(ownerKey: ByteArray) {
        try {
            onGrantHeld?.invoke(ownerKey.copyOf())
        } catch (cause: Throwable) {
            Log.w(LOG_TAG, "grant-held hook failed: ${cause.javaClass.simpleName}")
        }
    }

    // -----------------------------------------------------------------
    // Contacts of contacts (ADR-0060 §4)
    // -----------------------------------------------------------------

    /** Keep the introduction record [senderKey] offered in the one-to-one
     *  conversation (the caller checked it came from the other participant).
     *  The newest per contact wins. */
    public suspend fun keepIntroductionOffer(
        senderKey: ByteArray,
        record: ByteArray,
        /** The account the offer was received for; `null` = whoever is bound. */
        boundAs: BoundAccount? = null,
    ) {
        val b = binding.get() ?: return
        if (boundAs != null && boundAs.token !== b) return
        val senderHex = reachHex(senderKey)
        val recordHex = reachHex(record)
        lock.withLock {
            if (binding.get() !== b || doc.introOffers[senderHex] == recordHex) return@withLock
            doc = doc.copy(introOffers = doc.introOffers + (senderHex to recordHex))
            store.save(b.accountIdB64, doc)
        }
    }

    /** Keep the bundle [senderKey] relayed, when strictly newer than the one
     *  held (`bundleIsNewer`: a re-send carries its original stamp). */
    public suspend fun keepIntroductionsBundle(
        senderKey: ByteArray,
        wire: ByteArray,
        updatedAtMs: ULong,
        /** The account the bundle was received for; `null` = whoever is bound. */
        boundAs: BoundAccount? = null,
    ) {
        val b = binding.get() ?: return
        if (boundAs != null && boundAs.token !== b) return
        val senderHex = reachHex(senderKey)
        lock.withLock {
            if (binding.get() !== b) return@withLock
            val held = doc.introBundles[senderHex]
            if (held != null &&
                !uniffi.lakko_ffi_uniffi.bundleIsNewer(updatedAtMs, held.updatedAtMs.toULong())
            ) {
                return@withLock
            }
            doc = doc.copy(
                introBundles = doc.introBundles + (senderHex to StoredIntroBundle(reachHex(wire), updatedAtMs.toLong())),
            )
            store.save(b.accountIdB64, doc)
        }
    }

    /** Every relayed bundle wire this device keeps. */
    public fun introductionBundleWires(): List<ByteArray> =
        doc.introBundles.toSortedMap().values.mapNotNull { reachUnhex(it.bundleHex) }

    /** What the kept document holds, counts only, for the search log line. */
    public fun storedCounts(): String =
        "held=${doc.held.size} offers=${doc.introOffers.size} bundles=${doc.introBundles.size} " +
            "own_offer=${doc.ownOffer != null} offer_sent=${doc.offerSentTo.size} " +
            "bundle_sent=${doc.bundleSentTo.size} joined=${doc.joinedGroups.size}"

    /** What this device holds from the person known by [accountKey]
     *  (ADR-0060 section 4, whether it asks for introductions). */
    internal fun introductionsHeldFrom(accountKey: ByteArray): IntroductionsHeld {
        val keys = personKeys(accountKey)
        return IntroductionsHeld(
            offer = keys.any { it in doc.introOffers },
            bundle = keys.any { it in doc.introBundles },
            contactGrant = holdsHeld(accountKey, ReachKind.CONTACT, ""),
        )
    }

    /** Every record the contacts offered, hex, ordered by contact key. */
    public fun offeredIntroductionRecordsHex(): List<String> = doc.introOffers.toSortedMap().values.toList()

    /** The contacts-of-contacts publication state held here. */
    internal fun introPublicationState(): IntroPublicationState = IntroPublicationState(
        termOutputs = doc.termOutputs,
        ownOffer = doc.ownOffer,
        offerSentTo = doc.offerSentTo,
        bundleSentTo = doc.bundleSentTo,
        reopenedAtMs = doc.introReopenedAtMs,
    )

    /** Persist the publication state after a step. */
    internal suspend fun saveIntroPublicationState(state: IntroPublicationState) {
        val b = binding.get() ?: return
        lock.withLock {
            if (binding.get() !== b) return@withLock
            doc = doc.copy(
                termOutputs = state.termOutputs,
                ownOffer = state.ownOffer,
                offerSentTo = state.offerSentTo,
                bundleSentTo = state.bundleSentTo,
                introReopenedAtMs = state.reopenedAtMs,
            )
            store.save(b.accountIdB64, doc)
        }
    }

    /** The current version of the `fof` slot. */
    public fun fofVersion(): Int = versionOf(doc, ReachSlot(ReachKind.FOF, ""))

    // -----------------------------------------------------------------
    // Channels (ADR-0060 §3)
    // -----------------------------------------------------------------

    /**
     * ADR-0078 section 9: the `channel` key is the account's one key of that
     * class, the same in every channel and no longer versioned by a channel's
     * epoch; it rotates when a channel is left or the setting changes.
     */

    /**
     * Seal this account's member card (ADR-0060 §3): [profile] (a profile
     * envelope) under the epoch [seed], bound to the account identity, or
     * `null` without a session (a reader drops an unbound card).
     */
    public fun sealOwnMemberCard(
        seed: ByteArray,
        epoch: UInt,
        channelId: ByteArray,
        subscriberPk: ByteArray,
        profile: ByteArray,
    ): ByteArray? {
        val b = binding.get() ?: return null
        return uniffi.lakko_ffi_uniffi.sealMemberCard(seed, epoch, channelId, subscriberPk, b.accountSeed, profile)
    }

    // -----------------------------------------------------------------
    // Committer delegations (ADR-0060 §6)
    // -----------------------------------------------------------------

    /** Keep the newest delegation envelope per (sender, added account) in a
     *  group, the sender resolved at receipt (C-68). */
    public fun keepDelegation(groupHex: String, senderKey: ByteArray, envelope: ByteArray) {
        val b = binding.get() ?: return
        val addedAccountKey = try {
            delegationAddedKey(envelope)
        } catch (_: Throwable) {
            return
        }
        val entry = StoredDelegation(
            groupHex = groupHex.lowercase(),
            senderKeyHex = reachHex(senderKey),
            addedAccountKeyHex = reachHex(addedAccountKey),
            envelopeHex = reachHex(envelope),
        )
        scope.launch {
            lock.withLock {
                if (binding.get() !== b) return@withLock
                doc = doc.copy(
                    delegations = doc.delegations.filterNot {
                        it.groupHex == entry.groupHex && it.senderKeyHex == entry.senderKeyHex &&
                            it.addedAccountKeyHex == entry.addedAccountKeyHex
                    } + entry,
                )
                store.save(b.accountIdB64, doc)
            }
            // A re-sent envelope is what a committer that merged the commit
            // and owes the Welcome waits for: its last delegation may have
            // expired (ADR-0060 section 6).
            notifyGrantHeld(mailboxAccountKey(addedAccountKey))
        }
    }

    /** A delegation this device may use: its header, the 32-byte Ed25519
     *  seed of the own account key it names, and the added person's mailbox
     *  binding the envelope carried (ADR-0078). SECRET. */
    public class OwnDelegation(
        public val header: String,
        public val signerSeed: ByteArray,
        public val addedMailboxBinding: ByteArray,
    )

    /**
     * The delegation naming this account for a Welcome to [addedAccountKey]
     * in [groupHex], or `null`.
     *
     * ADR-0074 3.5a: a proposer names the committer by the account key IT
     * knows. One that observed this account's proven rotation names the
     * current key; one that has not names the key the role table carries,
     * a predecessor. So every own key this device holds the seed of is
     * tried ([pickOwnDelegation], the current one first) and the write is
     * signed with the key that was named.
     */
    public fun delegationFor(groupHex: String, addedAccountKey: ByteArray): OwnDelegation? {
        val b = binding.get() ?: return null
        // A delegation names the added person by whichever of their keys
        // the proposer knew; the current key's first.
        val addedKeys = personKeys(addedAccountKey)
        val envelopes = addedKeys.flatMap { key ->
            doc.delegations.filter { it.groupHex == groupHex.lowercase() && it.addedAccountKeyHex == key }
        }.mapNotNull { d ->
            runCatching {
                uniffi.lakko_ffi_uniffi.decodeCommitterDelegationEnvelope(reachUnhex(d.envelopeHex)!!)
            }.getOrNull()
        }
        if (envelopes.isEmpty()) return null
        val previous = try {
            ownPredecessorKeys(b.accountIdB64).filterNot { it.first.contentEquals(b.accountKey) }
        } catch (_: Throwable) {
            emptyList()
        }
        try {
            val keys = listOf(b.accountKey) + previous.map { it.first }
            var bindingOfPick: ByteArray? = null
            val (header, index) = pickOwnDelegation(envelopes, keys) { env, key ->
                uniffi.lakko_ffi_uniffi.committerDelegationFor(env, key)?.also { bindingOfPick = env.addedMailboxBinding }
            } ?: return null
            val seed = if (index == 0) b.accountSeed.copyOf() else previous[index - 1].second.copyOf()
            return OwnDelegation(header, seed, bindingOfPick ?: return null)
        } finally {
            previous.forEach { it.second.fill(0) }
        }
    }

    /** What [composeDelegations] made. */
    public class ComposedDelegations(
        /** The envelope plaintext. */
        public val envelope: ByteArray,
        /** Whether it carries delegations; one with none names a binding,
         *  which does not expire. */
        public val signed: Boolean,
    )

    /**
     * The committer-delegation envelope this device sends next to an Add it
     * proposed for [addedAccountKey] in [groupHex] (ADR-0060 section 6): one
     * delegation per committer candidate ([candidateKeys], owner first then
     * admins by rank, at most 16), each to that candidate's account key,
     * living [ttlMs] (at most 24 h), signed with the first grant held for the
     * person whose binding verifies against their current key
     * ([delegationGrantOrder]: contact, group, channel, contacts of
     * contacts; a grant a search found them through was kept as one).
     *
     * With no such grant but a binding this device recorded for the add of a
     * person found in Everyone ([recordOpenBinding]): the envelope with that
     * binding and NO delegation, which tells the committer it may write the
     * Welcome into their open mailbox without a proof (ADR-0060 section 6,
     * amended 2026-10-29). `null` when neither is held.
     */
    public fun composeDelegations(
        addedAccountKey: ByteArray,
        candidateKeys: List<ByteArray>,
        groupHex: String? = null,
        ttlMs: Long = DELEGATION_TTL_MS,
        nowMs: Long = System.currentTimeMillis(),
    ): ComposedDelegations? {
        // ADR-0074 3.5a rule 1: the delegation opens the added person's
        // CURRENT mailbox, and is signed to each candidate's current key
        // (the key that candidate's device signs the write with).
        val addedCurrent = mailboxAccountKey(addedAccountKey)
        val held = heldFor(addedAccountKey)
        // ADR-0078: the mailbox comes from the grant's binding, verified
        // against that current key; a grant whose binding is refused is
        // passed over.
        val signing = delegationGrantOrder(held.map { it.first }).asSequence()
            .flatMap { ref -> held.filter { it.first == ref }.map { it.second }.asSequence() }
            .mapNotNull { grant -> mailboxCrypto.grantMailboxPkOrNull(grant, addedCurrent)?.let { grant to it } }
            .firstOrNull()
        if (signing != null) {
            val (grant, mailboxPk) = signing
            val mailbox = mailboxCrypto.address(mailboxPk)
            val expires = (nowMs + ttlMs).toULong()
            val max = uniffi.lakko_ffi_uniffi.maxCommitterDelegations().toInt()
            val blobs = candidateKeys.take(max).map { candidate ->
                val nonce = ByteArray(DELEGATION_NONCE_LEN).also(random::nextBytes)
                uniffi.lakko_ffi_uniffi.signDelegationBlob(grant.seed, mailbox, mailboxAccountKey(candidate), expires, nonce)
            }
            if (blobs.isEmpty()) return null
            val envelope = uniffi.lakko_ffi_uniffi.composeCommitterDelegationEnvelope(
                uniffi.lakko_ffi_uniffi.FfiCommitterDelegationEnvelope(
                    addedAccountKey = addedAccountKey,
                    addedMailboxBinding = grant.binding,
                    delegations = blobs,
                ),
            )
            return ComposedDelegations(envelope, signed = true)
        }
        val group = groupHex ?: return null
        val binding = ownOpenBindingFor(group, addedAccountKey) ?: return null
        val envelope = uniffi.lakko_ffi_uniffi.composeCommitterDelegationEnvelope(
            uniffi.lakko_ffi_uniffi.FfiCommitterDelegationEnvelope(
                addedAccountKey = addedAccountKey,
                addedMailboxBinding = binding,
                delegations = emptyList(),
            ),
        )
        return ComposedDelegations(envelope, signed = false)
    }

    // -----------------------------------------------------------------
    // A person found in Everyone (ADR-0060 section 6, amended 2026-10-29)
    // -----------------------------------------------------------------

    /**
     * Record the [mailboxBinding] a search found the Everyone person
     * [accountKey] with, for the add this device is about to propose in
     * [groupHex]: verified under their CURRENT account key first (a binding
     * that does not verify is not recorded and `false` is returned). The
     * delegation envelope then carries it ([composeDelegations]) and this
     * device's own sweep may write their Welcome into that open mailbox
     * ([openWelcomeMailboxPk]).
     */
    public suspend fun recordOpenBinding(groupHex: String, accountKey: ByteArray, mailboxBinding: ByteArray): Boolean {
        val b = binding.get() ?: return false
        val current = mailboxAccountKey(accountKey)
        mailboxCrypto.bindingMailboxPkOrNull(mailboxBinding, current) ?: return false
        val changed = lock.withLock {
            if (binding.get() !== b) return@withLock false
            val next = withOpenBinding(doc.openBindings, groupHex, reachHex(current), reachHex(mailboxBinding))
            if (next == doc.openBindings) return@withLock true
            doc = doc.copy(openBindings = next)
            store.save(b.accountIdB64, doc)
            true
        }
        // A Welcome already owed to them is retried now.
        if (changed) notifyGrantHeld(current)
        return changed
    }

    /** The binding this device recorded for the add of [accountKey] in
     *  [groupHex], still verifying under their current key, or `null`. */
    private fun ownOpenBindingFor(groupHex: String, accountKey: ByteArray): ByteArray? {
        val current = mailboxAccountKey(accountKey)
        return openWelcomeBindings(doc.openBindings, emptyList(), groupHex, personKeys(accountKey))
            .firstOrNull { mailboxCrypto.bindingMailboxPkOrNull(it, current) != null }
    }

    /**
     * The mailbox key a sweep's Welcome to [accountKey] in [groupHex] may be
     * written to WITHOUT a proof, or `null`: only for a person this device
     * recorded as found in Everyone for that add, or one a kept
     * no-delegation envelope names ([openWelcomeBindings]), the binding
     * verified under their current account key. The caller writes there only
     * when it holds no usable grant or delegation for them at all; a refused
     * one keeps the Welcome owed ([me.lakko.app.chat.writeWelcomeWithGrants]).
     */
    public fun openWelcomeMailboxPk(groupHex: String, accountKey: ByteArray): ByteArray? {
        if (binding.get() == null) return null
        val current = mailboxAccountKey(accountKey)
        val kept = doc.delegations.filter { it.groupHex == groupHex.lowercase() }.mapNotNull { d ->
            val env = runCatching {
                uniffi.lakko_ffi_uniffi.decodeCommitterDelegationEnvelope(reachUnhex(d.envelopeHex)!!)
            }.getOrNull() ?: return@mapNotNull null
            KeptDelegationView(d.addedAccountKeyHex, env.addedMailboxBinding, env.delegations.size)
        }
        return openWelcomeBindings(doc.openBindings, kept, groupHex, personKeys(accountKey))
            .firstNotNullOfOrNull { mailboxCrypto.bindingMailboxPkOrNull(it, current) }
    }

    /**
     * The mailbox key a Welcome to [accountKey] goes to: [mailboxPkFor] (a
     * held grant's binding), else, in [groupHex], the binding of a kept
     * delegation naming this account, else the open binding of
     * [openWelcomeMailboxPk]. `null`: no mailbox known, nothing is written.
     * Only the ADDRESS is chosen here; what signs the write is
     * [reachAuthCandidates]' (or the proofless fallback's).
     */
    public fun welcomeMailboxPkFor(accountKey: ByteArray, groupHex: String?): ByteArray? {
        mailboxPkFor(accountKey)?.let { return it }
        val group = groupHex ?: return null
        val current = mailboxAccountKey(accountKey)
        delegationFor(group, accountKey)?.let { picked ->
            picked.signerSeed.fill(0)
            mailboxCrypto.bindingMailboxPkOrNull(picked.addedMailboxBinding, current)?.let { return it }
        }
        return openWelcomeMailboxPk(group, accountKey)
    }

    // -----------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------

    private fun derive(b: Binding, slot: ReachSlot, version: Int): FfiReachGrant =
        uniffi.lakko_ffi_uniffi.deriveReachGrant(
            b.accountSeed,
            slot.kind.toFfi(),
            if (slot.kind.scopeless) ByteArray(0) else reachUnhex(slot.scopeHex) ?: ByteArray(0),
            version.toUInt(),
        )

    private fun publicKeyOf(seed: ByteArray): ByteArray = uniffi.lakko_ffi_uniffi.reachVerifyingKey(seed)

    private fun versionOf(doc: ReachDoc, slot: ReachSlot): Int =
        doc.versions.firstOrNull { it.kind == slot.kind.name && it.scopeHex == slot.scopeHex }?.version ?: 0

    private fun allows(setting: FfiVisibilitySetting, kind: FfiScopeKind, id: ByteArray): Boolean =
        runCatching { uniffi.lakko_ffi_uniffi.allowsScope(setting, FfiScopeId(kind, id)) }.getOrDefault(false)

    private fun decodeVisibility(hex: String?): FfiVisibilitySetting? {
        val bytes = hex?.let(::reachUnhex) ?: return uniffi.lakko_ffi_uniffi.defaultVisibilitySetting()
        return runCatching { uniffi.lakko_ffi_uniffi.decodeVisibilityEnvelope(bytes) }
            .getOrElse { uniffi.lakko_ffi_uniffi.defaultVisibilitySetting() }
    }

    public companion object {
        private const val LOG_TAG = "Reach"

        /** How long after a failed version-recovery read it is tried again
         *  (besides every reconcile). Chosen, not measured. */
        public const val VERSION_RECOVERY_RETRY_MS: Long = 60_000L

        /** How long a committer delegation lives: the 24-hour maximum. */
        public const val DELEGATION_TTL_MS: Long = 24L * 60 * 60 * 1000

        private const val DELEGATION_NONCE_LEN = 16

        @Volatile
        private var instance: ReachManager? = null

        /** The process-wide manager. */
        public fun get(context: Context): ReachManager =
            instance ?: synchronized(this) {
                instance ?: ReachManager(
                    store = EncryptedReachDocStore(context),
                    ownProfiles = ContextOwnProfileAccess(context.applicationContext),
                    provenLinks = me.lakko.app.contacts.SharedPrefsProvenLinkStore(context.applicationContext)::load,
                    ownPredecessorKeys = { accountIdB64 -> ownPredecessorKeysOnDevice(context.applicationContext, accountIdB64) },
                ).also { instance = it }
            }

        /** The manager if one was created, else `null` (background readers). */
        public fun peek(): ReachManager? = instance
    }
}

/** [ReachManager.OwnProfileAccess] over [me.lakko.app.profile.OwnProfileStore]. */
private class ContextOwnProfileAccess(private val context: Context) : ReachManager.OwnProfileAccess {
    override fun load(accountId: ByteArray): me.lakko.app.profile.OwnProfile? =
        me.lakko.app.profile.OwnProfileStore.load(context, accountId)

    override fun update(
        accountId: ByteArray,
        transform: (me.lakko.app.profile.OwnProfile?) -> me.lakko.app.profile.OwnProfile?,
    ): Boolean = me.lakko.app.profile.OwnProfileStore.update(context, accountId, transform)
}

/** What a profile carries (ADR-0060 §3). SECRET grants. */
public data class ProfileGrants(val grants: List<FfiReachGrant>, val shareUsername: Boolean)

/** [ReachKind] as the FFI's class. */
public fun ReachKind.toFfi(): FfiReachClass = when (this) {
    ReachKind.CONTACT -> FfiReachClass.CONTACT
    ReachKind.LINK -> FfiReachClass.LINK
    ReachKind.REPLY -> FfiReachClass.REPLY
    ReachKind.GROUP -> FfiReachClass.GROUP
    ReachKind.MEMBER -> FfiReachClass.MEMBER
    ReachKind.CHANNEL -> FfiReachClass.CHANNEL
    ReachKind.FOF -> FfiReachClass.FOF
}

/** The FFI's class as [ReachKind]. */
public fun FfiReachClass.toKind(): ReachKind = when (this) {
    FfiReachClass.CONTACT -> ReachKind.CONTACT
    FfiReachClass.LINK -> ReachKind.LINK
    FfiReachClass.REPLY -> ReachKind.REPLY
    FfiReachClass.GROUP -> ReachKind.GROUP
    FfiReachClass.MEMBER -> ReachKind.MEMBER
    FfiReachClass.CHANNEL -> ReachKind.CHANNEL
    FfiReachClass.FOF -> ReachKind.FOF
}

/** This account's own previous account keys on this device, NEWEST first,
 *  each with its 32-byte Ed25519 seed (ADR-0074 3.5a). The caller wipes the
 *  seeds. */
private fun ownPredecessorKeysOnDevice(context: Context, accountIdB64: String): List<Pair<ByteArray, ByteArray>> {
    val rows = me.lakko.app.auth.OwnPredecessorStore.onDevice(context).load(accountIdB64)
    try {
        return rows.asReversed().mapNotNull { row ->
            try {
                Identity.fromSeed(row.phraseSeed).use { it.signaturePublicKey() to it.ed25519Seed() }
            } catch (_: Throwable) {
                null
            }
        }
    } finally {
        rows.forEach { it.zero() }
    }
}

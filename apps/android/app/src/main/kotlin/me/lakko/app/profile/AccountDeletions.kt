// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.profile

import android.content.Context
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch
import kotlinx.serialization.Serializable
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import java.util.Base64
import java.util.concurrent.ConcurrentHashMap

/*
 * ADR-0098 (`docs/ux/deleted-account.md`) — what this device knows about
 * whether a person's account still exists.
 *
 * Two inputs, combined by the shared rules in `lakko-contacts::account_deleted`
 * (never re-implemented here):
 *
 *  - PUSH: an account-deleted statement the person's own device posted in a
 *    conversation, verified at receipt against the MLS sender (C-68). Final.
 *  - PULL: the directory's answer for the person's account id
 *    (`GET /discovery/identity/{id}`), asked only for a person on screen.
 *    `Absent` is reversible by a later `SamePerson`.
 *
 * Keyed on the person's account key (standard base64, [MemberIdentity] in the
 * chat code), stored per signed-in account beside the profiles, written through
 * the account store gate and removed with the account
 * ([ProfileDirectory.forgetAccountOnDevice]).
 */

/** A complete directory answer as the client classified it. Errors, timeouts
 *  and `5xx` are not answers: nothing is recorded for them. */
public enum class DirectoryAnswer {
    /** `404`: no bundle for the account id. */
    ABSENT,

    /** `200`, and the bundle serves the key held for the person, or its
     *  conversation link verifies naming that key. */
    SAME_PERSON,

    /** `200` with another key and no link to the held key. Never "deleted". */
    OTHER_KEY,
}

/** What is recorded for one person. Times are this device's clock, Unix ms. */
public data class PersonDeletionRecord(
    val statementVerified: Boolean = false,
    val lastAnswer: DirectoryAnswer? = null,
    val lastAnswerAtMs: Long? = null,
    val lastSamePersonAtMs: Long? = null,
    val lastAttemptAtMs: Long? = null,
)

/**
 * The shared rules, behind a seam so the host suite (which does not load the
 * native library) can run against a fake. [FfiAccountDeletionRules] is the
 * one production implementation.
 */
public interface AccountDeletionRules {
    /** `accountDeletionStatus(...) == Deleted`. */
    public fun isDeleted(statementVerified: Boolean, lastAnswer: DirectoryAnswer?): Boolean

    /** `mayShowUsername(status, confirmedAtMs, nowMs)`. */
    public fun mayShowUsername(deleted: Boolean, confirmedAtMs: Long?, nowMs: Long): Boolean

    /** `directoryCheckDue(...)`. */
    public fun checkDue(statementVerified: Boolean, lastAnswerAtMs: Long?, lastAttemptAtMs: Long?, nowMs: Long): Boolean
}

/** The rules of `crates/lakko-contacts/src/account_deleted.rs`, over UniFFI. */
public object FfiAccountDeletionRules : AccountDeletionRules {
    private fun ffi(answer: DirectoryAnswer?): uniffi.lakko_ffi_uniffi.FfiDirectoryAnswer? = when (answer) {
        null -> null
        DirectoryAnswer.ABSENT -> uniffi.lakko_ffi_uniffi.FfiDirectoryAnswer.ABSENT
        DirectoryAnswer.SAME_PERSON -> uniffi.lakko_ffi_uniffi.FfiDirectoryAnswer.SAME_PERSON
        DirectoryAnswer.OTHER_KEY -> uniffi.lakko_ffi_uniffi.FfiDirectoryAnswer.OTHER_KEY
    }

    private fun ms(value: Long?): ULong? = value?.coerceAtLeast(0L)?.toULong()

    override fun isDeleted(statementVerified: Boolean, lastAnswer: DirectoryAnswer?): Boolean =
        uniffi.lakko_ffi_uniffi.accountDeletionStatus(statementVerified, ffi(lastAnswer)) ==
            uniffi.lakko_ffi_uniffi.FfiAccountStatus.DELETED

    override fun mayShowUsername(deleted: Boolean, confirmedAtMs: Long?, nowMs: Long): Boolean =
        uniffi.lakko_ffi_uniffi.mayShowUsername(
            if (deleted) uniffi.lakko_ffi_uniffi.FfiAccountStatus.DELETED else uniffi.lakko_ffi_uniffi.FfiAccountStatus.ACTIVE,
            ms(confirmedAtMs),
            nowMs.coerceAtLeast(0L).toULong(),
        )

    override fun checkDue(statementVerified: Boolean, lastAnswerAtMs: Long?, lastAttemptAtMs: Long?, nowMs: Long): Boolean =
        uniffi.lakko_ffi_uniffi.directoryCheckDue(
            statementVerified,
            ms(lastAnswerAtMs),
            ms(lastAttemptAtMs),
            nowMs.coerceAtLeast(0L).toULong(),
        )
}

/** Where the records live. */
public interface PersonDeletionPersistence {
    /** The signed-in account, or `null` when none is. */
    public fun currentAccount(): String?

    public fun load(account: String): Map<String, PersonDeletionRecord>

    /** Write [records] for [account]; refused (not written) for an account
     *  whose session ended. */
    public fun save(account: String, records: Map<String, PersonDeletionRecord>)
}

/**
 * The per-person records of the signed-in account, and the decisions over
 * them. Thread-safe; every mutation persists at once.
 */
public class PersonDeletionBook(
    private val persistence: PersonDeletionPersistence,
    private val rules: AccountDeletionRules,
    private val nowMs: () -> Long = System::currentTimeMillis,
    /** Told after any change that can alter how a person is drawn. */
    private val onChanged: () -> Unit = {},
) {
    private val lock = Any()
    private var loadedFor: String? = null
    private var records: MutableMap<String, PersonDeletionRecord> = mutableMapOf()
    private val inFlight: MutableSet<String> = ConcurrentHashMap.newKeySet()

    /** Told the first time a person becomes deleted on this device (statement
     *  or answer), with their key: the pending-contact queue drop hangs here. */
    @Volatile
    public var onPersonDeleted: ((String) -> Unit)? = null

    private fun loaded(): Pair<String, MutableMap<String, PersonDeletionRecord>>? {
        val account = persistence.currentAccount() ?: return null
        if (loadedFor != account) {
            records = try {
                persistence.load(account).toMutableMap()
            } catch (_: Throwable) {
                mutableMapOf()
            }
            loadedFor = account
        }
        return account to records
    }

    /** The record of [keyB64], empty when nothing is known. */
    public fun record(keyB64: String): PersonDeletionRecord =
        synchronized(lock) { loaded()?.second?.get(keyB64) } ?: PersonDeletionRecord()

    /** Whether [keyB64] is shown as Deleted account. */
    public fun isDeleted(keyB64: String): Boolean {
        val r = record(keyB64)
        if (!r.statementVerified && r.lastAnswer == null) return false
        return rules.isDeleted(r.statementVerified, r.lastAnswer)
    }

    /** Whether a verified `@username` may be shown beside [keyB64] now. */
    public fun mayShowUsername(keyB64: String): Boolean {
        val r = record(keyB64)
        val deleted = (r.statementVerified || r.lastAnswer != null) && rules.isDeleted(r.statementVerified, r.lastAnswer)
        return rules.mayShowUsername(deleted, r.lastSamePersonAtMs, nowMs())
    }

    private fun update(keyB64: String, change: (PersonDeletionRecord) -> PersonDeletionRecord): Pair<Boolean, Boolean>? {
        var becameDeleted = false
        val changed = synchronized(lock) {
            val (account, map) = loaded() ?: return null
            val before = map[keyB64] ?: PersonDeletionRecord()
            val after = change(before)
            if (after == before) return@synchronized false
            val wasDeleted = (before.statementVerified || before.lastAnswer != null) &&
                rules.isDeleted(before.statementVerified, before.lastAnswer)
            val isDeleted = (after.statementVerified || after.lastAnswer != null) &&
                rules.isDeleted(after.statementVerified, after.lastAnswer)
            becameDeleted = isDeleted && !wasDeleted
            map[keyB64] = after
            persistence.save(account, map.toMap())
            true
        }
        if (changed) onChanged()
        if (becameDeleted) onPersonDeleted?.invoke(keyB64)
        return changed to becameDeleted
    }

    /** A verified account-deleted statement from [keyB64]: final. Returns
     *  `true` when this is new. */
    public fun markStatementVerified(keyB64: String): Boolean =
        update(keyB64) { if (it.statementVerified) it else it.copy(statementVerified = true) }?.first == true

    /** A complete directory answer for [keyB64]. Ignored after a statement:
     *  nothing reverses it. */
    public fun recordAnswer(keyB64: String, answer: DirectoryAnswer) {
        val now = nowMs()
        update(keyB64) {
            if (it.statementVerified) {
                it
            } else {
                it.copy(
                    lastAnswer = answer,
                    lastAnswerAtMs = now,
                    lastSamePersonAtMs = if (answer == DirectoryAnswer.SAME_PERSON) now else it.lastSamePersonAtMs,
                )
            }
        }
    }

    /**
     * Start a directory check of [keyB64] when one is due and none is in
     * flight: the attempt time is recorded first. `false` = ask nothing.
     * Every `true` must be followed by [endCheck].
     */
    public fun beginCheck(keyB64: String): Boolean {
        val r = record(keyB64)
        val now = nowMs()
        if (!rules.checkDue(r.statementVerified, r.lastAnswerAtMs, r.lastAttemptAtMs, now)) return false
        if (!inFlight.add(keyB64)) return false
        val written = update(keyB64) { it.copy(lastAttemptAtMs = now) }
        if (written == null) {
            inFlight.remove(keyB64)
            return false
        }
        return true
    }

    /** The check of [keyB64] is over, answered or not. */
    public fun endCheck(keyB64: String) {
        inFlight.remove(keyB64)
    }

    /** Drop the in-memory copy (forget device); the rows on disk are the
     *  caller's. */
    public fun forget() {
        synchronized(lock) {
            loadedFor = null
            records = mutableMapOf()
        }
        inFlight.clear()
        onChanged()
    }
}

/**
 * Classify a complete answer of `GET /discovery/identity/{id}` for a person
 * this device holds under [heldKeysB64]. [bundleKeyB64] is `null` for a
 * `404`. [verifyLink] is `verifyConversationLink(link, servedKey)`, returning
 * the conversation key the link names, or `null` when it does not verify.
 */
public fun classifyDirectoryAnswer(
    bundleKeyB64: String?,
    conversationLinkB64: String?,
    heldKeysB64: Set<String>,
    verifyLink: (link: ByteArray, servedKey: ByteArray) -> ByteArray?,
): DirectoryAnswer {
    if (bundleKeyB64 == null) return DirectoryAnswer.ABSENT
    if (bundleKeyB64 in heldKeysB64) return DirectoryAnswer.SAME_PERSON
    val link = conversationLinkB64?.takeIf { it.isNotEmpty() } ?: return DirectoryAnswer.OTHER_KEY
    val named = try {
        verifyLink(Base64.getDecoder().decode(link), Base64.getDecoder().decode(bundleKeyB64))
    } catch (_: Throwable) {
        null
    } ?: return DirectoryAnswer.OTHER_KEY
    return if (Base64.getEncoder().encodeToString(named) in heldKeysB64) {
        DirectoryAnswer.SAME_PERSON
    } else {
        DirectoryAnswer.OTHER_KEY
    }
}

/**
 * The pull side: who to ask about, how, and where the answer goes. One per
 * process, owned by [ProfileDirectory].
 */
public class AccountDeletionChecks(
    public val book: PersonDeletionBook,
    /** The account id (16 bytes) this device holds for [keyB64], or `null`. */
    private val accountIdOf: (keyB64: String) -> ByteArray?,
    /** The keys this device holds for the account id (lower-case hex). */
    private val heldKeysOf: (accountIdHex: String) -> Set<String>,
    /** `GET /discovery/identity/{id}`: the served key and conversation link
     *  on 200, `null` on 404; throws on anything else. */
    private val fetch: suspend (accountId: ByteArray) -> Pair<String, String?>?,
    private val verifyLink: (ByteArray, ByteArray) -> ByteArray?,
    private val scope: CoroutineScope,
    /** The signed-in account's own id (hex), never asked about. */
    private val ownAccountIdHex: () -> String? = { null },
) {
    /**
     * Any identity fetch answered (200 or 404) for [accountId]: record it for
     * every key held for that account. Nothing for an account this device
     * holds no key for.
     */
    public fun onDirectoryAnswer(accountId: ByteArray, servedKeyB64: String?, conversationLinkB64: String?) {
        val hex = accountId.joinToString("") { "%02x".format(it.toInt() and 0xff) }
        if (hex == ownAccountIdHex()) return
        val held = try {
            heldKeysOf(hex)
        } catch (_: Throwable) {
            emptySet()
        }
        if (held.isEmpty()) return
        val answer = classifyDirectoryAnswer(servedKeyB64, conversationLinkB64, held, verifyLink)
        for (key in held) book.recordAnswer(key, answer)
    }

    /**
     * [keyB64] is on screen: ask the directory about them when the rule says
     * a check is due. At most one request in flight per person.
     */
    public fun onScreen(keyB64: String) {
        if (keyB64.isEmpty()) return
        val accountId = try {
            accountIdOf(keyB64)
        } catch (_: Throwable) {
            null
        } ?: return
        if (accountId.size != ACCOUNT_ID_BYTES) return
        val hex = accountId.joinToString("") { "%02x".format(it.toInt() and 0xff) }
        if (hex == ownAccountIdHex()) return
        if (!book.beginCheck(keyB64)) return
        scope.launch {
            try {
                // The answer itself is recorded by [onDirectoryAnswer], which
                // every identity fetch reports to.
                fetch(accountId)
            } catch (cancel: kotlinx.coroutines.CancellationException) {
                throw cancel
            } catch (_: Throwable) {
                // Not an answer: nothing recorded.
            } finally {
                book.endCheck(keyB64)
            }
        }
    }
}

/**
 * Every complete answer of `GET /discovery/identity/{id}` this process gets
 * is reported here ([me.lakko.app.discovery.DiscoveryClient.identityFetch]),
 * so each existing fetch records it too. `null` until the app wires it.
 */
public object DirectoryAnswerFeed {
    @Volatile
    public var sink: ((accountId: ByteArray, servedKeyB64: String?, conversationLinkB64: String?) -> Unit)? = null

    /** Report one answer; never throws. */
    public fun report(accountId: ByteArray, servedKeyB64: String?, conversationLinkB64: String?) {
        try {
            sink?.invoke(accountId, servedKeyB64, conversationLinkB64)
        } catch (_: Throwable) {
            // Recording is best effort; the caller's fetch goes on.
        }
    }
}

/** [PersonDeletionPersistence] beside the profiles, one row per account. */
internal class SharedPrefsPersonDeletionPersistence(private val context: Context) : PersonDeletionPersistence {
    private val json = Json { ignoreUnknownKeys = true }

    @Serializable
    private data class Stored(
        val s: Boolean = false,
        val a: String? = null,
        val at: Long? = null,
        val same: Long? = null,
        val tried: Long? = null,
    )

    private fun prefs() = context.getSharedPreferences(SharedPrefsProfilePersistence.PREFS, Context.MODE_PRIVATE)

    override fun currentAccount(): String? = me.lakko.app.contacts.AcceptedContactsStore.boundAccountB64

    override fun load(account: String): Map<String, PersonDeletionRecord> {
        val raw = prefs().getString(key(account), null) ?: return emptyMap()
        return try {
            json.decodeFromString<Map<String, Stored>>(raw).mapValues { (_, s) ->
                PersonDeletionRecord(
                    statementVerified = s.s,
                    lastAnswer = s.a?.let { name -> DirectoryAnswer.entries.firstOrNull { it.name == name } },
                    lastAnswerAtMs = s.at,
                    lastSamePersonAtMs = s.same,
                    lastAttemptAtMs = s.tried,
                )
            }
        } catch (_: Exception) {
            emptyMap()
        }
    }

    override fun save(account: String, records: Map<String, PersonDeletionRecord>) {
        val stored = records.mapValues { (_, r) ->
            Stored(r.statementVerified, r.lastAnswer?.name, r.lastAnswerAtMs, r.lastSamePersonAtMs, r.lastAttemptAtMs)
        }
        me.lakko.app.account.AccountStoreGate.write(account) {
            prefs().edit().putString(key(account), json.encodeToString(stored)).apply()
        }
    }

    companion object {
        fun key(account: String): String =
            "person-status.v1:" + me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(account)

        /** Remove [account]'s row (forget device). */
        fun delete(context: Context, account: String) {
            context.getSharedPreferences(SharedPrefsProfilePersistence.PREFS, Context.MODE_PRIVATE).edit()
                .remove(key(account))
                .apply()
        }
    }
}

/** Hex of [bytes], lower case. */
private fun hexOf(bytes: ByteArray): String = bytes.joinToString("") { "%02x".format(it.toInt() and 0xff) }

private fun bytesOfHex(hex: String): ByteArray? {
    if (hex.length % 2 != 0) return null
    return try {
        ByteArray(hex.length / 2) { i -> hex.substring(i * 2, i * 2 + 2).toInt(16).toByte() }
    } catch (_: NumberFormatException) {
        null
    }
}

/**
 * Build and attach the ADR-0098 state to the process-wide [directory]: the
 * book over the per-account store, the account-id and held-key lookups over
 * the profiles, the accepted contacts and the pending contacts, the identity
 * fetch, and the feed every identity fetch reports to.
 */
internal fun attachAccountDeletions(context: Context, directory: ProfileDirectory) {
    val app = context.applicationContext
    val book = PersonDeletionBook(
        persistence = SharedPrefsPersonDeletionPersistence(app),
        rules = FfiAccountDeletionRules,
        onChanged = directory::namesChanged,
    )
    val account = { me.lakko.app.contacts.AcceptedContactsStore.boundAccountB64 }
    val contacts = {
        try {
            me.lakko.app.contacts.AcceptedContactsStore.load(app)
        } catch (_: Throwable) {
            emptyList()
        }
    }
    val pending = {
        account()?.let { a ->
            try {
                me.lakko.app.contacts.PendingContactsFiles.onDevice(app).load(a)
            } catch (_: Throwable) {
                emptyList()
            }
        } ?: emptyList()
    }
    val ownHex = {
        account()?.let { a ->
            try {
                hexOf(me.lakko.app.auth.base64Decode(me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(a)))
            } catch (_: Throwable) {
                null
            }
        }
    }
    val checks = AccountDeletionChecks(
        book = book,
        accountIdOf = { keyB64 ->
            directory.profiles.get(keyB64)?.accountIdHex?.let(::bytesOfHex)
                ?: contacts().firstOrNull { Base64.getEncoder().encodeToString(it.request.fromPubkey) == keyB64 }
                    ?.request?.fromAccountId
                ?: pending().firstOrNull { it.signaturePubkeyB64 == keyB64 }?.accountIdHex?.let(::bytesOfHex)
        },
        heldKeysOf = { hex ->
            val keys = mutableSetOf<String>()
            directory.accountIdsByKey().forEach { (key, id) -> if (id.equals(hex, ignoreCase = true)) keys += key }
            contacts().forEach { c ->
                val id = c.request.fromAccountId
                if (id != null && hexOf(id) == hex) keys += Base64.getEncoder().encodeToString(c.request.fromPubkey)
            }
            pending().forEach { p -> if (p.accountIdHex.equals(hex, ignoreCase = true)) keys += p.signaturePubkeyB64 }
            keys.filter { it.isNotEmpty() }.toSet()
        },
        fetch = { id ->
            me.lakko.app.discovery.DiscoveryClient().use { it.identityFetch(id) }
                ?.let { it.signature_pubkey to it.conversation_link }
        },
        verifyLink = { link, served ->
            try {
                uniffi.lakko_ffi_uniffi.verifyConversationLink(link, served)
            } catch (_: Throwable) {
                null
            }
        },
        scope = CoroutineScope(kotlinx.coroutines.SupervisorJob() + kotlinx.coroutines.Dispatchers.IO),
        ownAccountIdHex = ownHex,
    )
    directory.attachDeletions(checks, ownHex)
    DirectoryAnswerFeed.sink = checks::onDirectoryAnswer
}

// ---------------------------------------------------------------------------
// ADR-0098 section 5 — removing a deleted member from a group
// ---------------------------------------------------------------------------

/**
 * Whether this device proposes the removal of a member whose account-deleted
 * statement it just verified in a group: only in a group (a one-to-one
 * cannot remove a person, ADR-0062), only when this device's account is the
 * owner or an admin there, never for the owner (C-63: unremovable), and once
 * per (account, group).
 */
public fun deletedMemberRemovalDue(
    isGroup: Boolean,
    ownRoleIsOwnerOrAdmin: Boolean,
    deletedIsOwner: Boolean,
    alreadyProposed: Boolean,
): Boolean = isGroup && ownRoleIsOwnerOrAdmin && !deletedIsOwner && !alreadyProposed

/**
 * The (group, account key) pairs whose removal this device owes or has
 * proposed after a verified statement, per signed-in account. Only a signed
 * statement ever enters here, never a directory answer.
 */
public class DeletedMemberRemovals(private val storage: Storage) {
    /** Where the ledger lives. */
    public interface Storage {
        public fun load(): Map<String, Boolean>

        public fun save(rows: Map<String, Boolean>)
    }

    private val lock = Any()

    private fun rowKey(groupIdHex: String, keyB64: String): String = groupIdHex.lowercase() + "|" + keyB64

    /** Whether the removal of [keyB64] from [groupIdHex] was proposed. */
    public fun proposed(groupIdHex: String, keyB64: String): Boolean =
        synchronized(lock) { storage.load()[rowKey(groupIdHex, keyB64)] == true }

    /** Record that the removal is owed (before the proposal is sent).
     *  `false` when it was already recorded, owed or proposed. */
    public fun markOwed(groupIdHex: String, keyB64: String): Boolean = synchronized(lock) {
        val rows = storage.load()
        val key = rowKey(groupIdHex, keyB64)
        if (key in rows) return@synchronized false
        storage.save(rows + (key to false))
        true
    }

    /** The proposal reached the group. */
    public fun markProposed(groupIdHex: String, keyB64: String) {
        synchronized(lock) {
            storage.save(storage.load() + (rowKey(groupIdHex, keyB64) to true))
        }
    }

    /** The account keys whose removal from [groupIdHex] is still owed. */
    public fun owed(groupIdHex: String): List<String> = synchronized(lock) {
        val prefix = groupIdHex.lowercase() + "|"
        storage.load().filter { (k, done) -> !done && k.startsWith(prefix) }.keys.map { it.removePrefix(prefix) }
    }

    public companion object {
        private const val PREFIX = "deleted-removals.v1:"

        private fun key(account: String): String =
            PREFIX + me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(account)

        /** The ledger of [account] on this device, beside the profiles. */
        public fun onDevice(context: Context, account: String): DeletedMemberRemovals {
            val prefs = context.applicationContext
                .getSharedPreferences(SharedPrefsProfilePersistence.PREFS, Context.MODE_PRIVATE)
            val json = Json { ignoreUnknownKeys = true }
            return DeletedMemberRemovals(object : Storage {
                override fun load(): Map<String, Boolean> {
                    val raw = prefs.getString(key(account), null) ?: return emptyMap()
                    return try {
                        json.decodeFromString<Map<String, Boolean>>(raw)
                    } catch (_: Exception) {
                        emptyMap()
                    }
                }

                override fun save(rows: Map<String, Boolean>) {
                    me.lakko.app.account.AccountStoreGate.write(account) {
                        prefs.edit().putString(key(account), json.encodeToString(rows)).apply()
                    }
                }
            })
        }

        /** Remove [account]'s ledger (forget device). */
        public fun deleteOnDevice(context: Context, account: String) {
            context.applicationContext.getSharedPreferences(SharedPrefsProfilePersistence.PREFS, Context.MODE_PRIVATE)
                .edit().remove(key(account)).apply()
        }
    }
}

// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.content.Context
import android.content.SharedPreferences
import kotlin.coroutines.cancellation.CancellationException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import me.lakko.app.l10n.UiText
import uniffi.lakko_ffi_uniffi.FfiException

/**
 * ADR-0087 section 9, `docs/ux/group-and-channel-privacy.md` section 9 - the end
 * of a membership.
 *
 * A person removed from a group, or who left it, loses it: the row goes, the
 * history is wiped, on every device of the account. The rules live here, where
 * the host suite can run them; `ChatRepository` feeds them live group state and
 * supplies the ports. [MembershipEnded.kt] keeps what stays for the one case
 * that does not end: a device whose own leaf was retired while the account
 * stays in the group keeps its history and its ended presentation.
 *
 * ## The trigger is a state, not an event
 *
 * [membershipFullyEnded]: the group is not active AND its frozen roster holds no
 * leaf of this account. `isActive` alone is wrong: a retired leaf reads inactive
 * too. The check runs after every applied commit, at launch over the stored
 * groups, and a device that never applied the commit hears of the end through
 * the `conversation_ended` register of the synced state.
 *
 * ## One durable owed record
 *
 * [OwedEnd] is written first, and [runMembershipEnd] walks it through its stages
 * so a crash resumes where it stopped: record, publish the register, stop the
 * poll / beacon / rotation and retire the push subscription, wipe the stores,
 * delete the MLS group, persist the provider snapshot durably, and only then
 * drop the known-groups row and the in-memory record. Every step may run twice.
 *
 * What may never start this: a refused read of the Delivery Service, and the
 * background paths (C-59), which cannot write at all.
 */

// ---------------------------------------------------------------------------
// The register, as this client reads it
// ---------------------------------------------------------------------------

/** One `conversation_ended` register that says ended, as plain values. */
public data class HeardConversationEnd(
    /** Lower-case hex of the group id, or of the channel id. */
    val idHex: String,
    val channel: Boolean,
    /** The MLS epoch of the end; 0 for a channel. */
    val epoch: ULong,
)

/** The ended registers of one account's synced state. */
public data class HeardConversationEnds(
    val accountIdB64: String,
    val ends: List<HeardConversationEnd>,
)

/**
 * The ended registers of the signed-in account, published by the sync runtime
 * after every change to the folded state. `null` while no synced state is open,
 * so a value here is also the signal that the register can be written: the chat
 * layer and the sync runtime start independently, and an end that had to wait
 * for one resumes on it.
 */
public object ConversationEndsMirror {
    private val mutable = MutableStateFlow<HeardConversationEnds?>(null)
    public val registers: StateFlow<HeardConversationEnds?> = mutable.asStateFlow()

    public fun publish(value: HeardConversationEnds?) {
        mutable.value = value
    }
}

/** What writing a register came to. */
public enum class EndPublish {
    /** No synced state is open yet: nothing was written. */
    NOT_OPEN,

    /** Folded here and handed to the DeviceGroup. */
    PUBLISHED,

    /** There is nothing to tell the other devices. It never stops a wipe. */
    NOTHING_TO_TELL,
}

// ---------------------------------------------------------------------------
// The trigger
// ---------------------------------------------------------------------------

/**
 * Is this account out of the group?
 *
 * [isActive] is `null` on a torn read, [rosterAccountKeys] on an unreadable
 * roster: neither is evidence of anything, and both answer `false`, since a
 * wrong `true` deletes a person's history. [isGroupKind] keeps a one-to-one out
 * of this for the same reason: no commit can take a person out of one
 * (ADR-0062), so a one-to-one that reads this way is a key this device failed
 * to recognise as its own, not an end.
 */
internal fun membershipFullyEnded(
    isActive: Boolean?,
    isGroupKind: Boolean,
    rosterAccountKeys: List<String>?,
    ownAccountKeys: Set<String>,
): Boolean {
    if (isActive != false) return false
    if (!isGroupKind) return false
    val roster = rosterAccountKeys ?: return false
    if (ownAccountKeys.isEmpty()) return false
    return roster.none { it in ownAccountKeys }
}

/** What a device does with a register that says ended. */
internal enum class HeardEndVerdict {
    /** Nothing held, or nothing can be read. */
    NOTHING,

    /** Stop and wipe, on the word of this account's other device. */
    ACT,

    /** This device came back after that end: it answers with `joined`. */
    ANSWER_JOINED,
}

/**
 * Compare epochs BEFORE anything is stopped. A group keeps its id across a
 * removal and a re-add and its epoch only rises, so a group held above the
 * register's epoch is one this device came back to, and a device offline across
 * both must not undo the re-add. `deleteAfterEndedElsewhere` refuses that too,
 * but only at the last step, after the poll stopped and the push went.
 */
internal fun heardEndVerdict(
    held: Boolean,
    isActive: Boolean?,
    groupEpoch: ULong?,
    registerEpoch: ULong,
): HeardEndVerdict {
    if (!held || isActive == null) return HeardEndVerdict.NOTHING
    val epoch = groupEpoch ?: return HeardEndVerdict.NOTHING
    if (epoch <= registerEpoch) return HeardEndVerdict.ACT
    return if (isActive) HeardEndVerdict.ANSWER_JOINED else HeardEndVerdict.NOTHING
}

// ---------------------------------------------------------------------------
// The owed record
// ---------------------------------------------------------------------------

/** How far an end has come. Each stage says what is done. */
public enum class EndStage {
    RECORDED,
    PUBLISHED,
    STOPPED,
    WIPED,

    /** Everything is gone; only the notice is still owed. */
    NOTICE_ONLY,
}

/** How a device came to end a group. */
public enum class EndSource {
    /** It applied the commit itself: `deleteLocally`. */
    APPLIED,

    /** It heard the register: `deleteAfterEndedElsewhere`. */
    HEARD,

    /** The person pressed `Delete this chat` on what stays ended here. No
     *  register, since the account may still be in the group. */
    CHOSEN,
}

/** One owed end. */
@Serializable
public data class OwedEnd(
    val idHex: String,
    /** Held only for the notice. */
    val name: String,
    val left: Boolean,
    /** Decimal: a full `u64` survives. */
    val epoch: String,
    val source: EndSource,
    val stage: EndStage,
    /** The person closed the notice while the steps were still running. */
    val noticeClosed: Boolean = false,
    /** When this device recorded the end (unix ms): orders the notices, since
     *  the store hands rows back in no order. `0` on a row stored before. */
    val recordedAtMs: Long = 0L,
) {
    /** A person who asked to go gets no notice; everyone else, once. */
    val noticeOwed: Boolean
        get() = !noticeClosed && source != EndSource.CHOSEN && !(source == EndSource.APPLIED && left)

    val epochValue: ULong get() = epoch.toULongOrNull() ?: 0uL
}

/** The first row of an end this device applied itself. */
internal fun appliedEnd(
    idHex: String,
    name: String,
    left: Boolean,
    epoch: ULong,
    nowMs: Long = System.currentTimeMillis(),
): OwedEnd =
    OwedEnd(idHex.lowercase(), name, left, epoch.toString(), EndSource.APPLIED, EndStage.RECORDED, recordedAtMs = nowMs)

/** The first row of an end this device heard of: steps 3 and 4 only. */
internal fun heardEnd(idHex: String, name: String, epoch: ULong, nowMs: Long = System.currentTimeMillis()): OwedEnd =
    OwedEnd(idHex.lowercase(), name, false, epoch.toString(), EndSource.HEARD, EndStage.PUBLISHED, recordedAtMs = nowMs)

/** The first row of a `Delete this chat`: the wipe alone. */
internal fun chosenEnd(idHex: String, epoch: ULong, nowMs: Long = System.currentTimeMillis()): OwedEnd =
    OwedEnd(idHex.lowercase(), "", true, epoch.toString(), EndSource.CHOSEN, EndStage.STOPPED, recordedAtMs = nowMs)

/** Where owed ends live. */
internal interface MembershipEndStore {
    fun load(accountIdB64: String): List<OwedEnd>

    /** `false` when the row did not reach the disk. */
    fun save(accountIdB64: String, row: OwedEnd): Boolean

    fun remove(accountIdB64: String, idHex: String)

    fun clearForAccount(accountIdB64: String)
}

/** The production store: one plain `SharedPreferences` file, written with
 *  `commit`, so a row returned as saved is on disk. The name in it is one the
 *  person's device already held in the same posture ([GroupNameStore]). */
internal class SharedPrefsMembershipEndStore(context: Context) : MembershipEndStore {

    private val appContext: Context = context.applicationContext
    private val json = Json { ignoreUnknownKeys = true }

    private fun prefs(): SharedPreferences =
        appContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    private fun prefix(accountIdB64: String) = "v1:$accountIdB64:"

    override fun load(accountIdB64: String): List<OwedEnd> = try {
        val prefix = prefix(accountIdB64)
        prefs().all.entries.filter { it.key.startsWith(prefix) }.mapNotNull { (_, value) ->
            runCatching { json.decodeFromString(OwedEnd.serializer(), value as String) }.getOrNull()
        }
    } catch (_: Throwable) {
        emptyList()
    }

    override fun save(accountIdB64: String, row: OwedEnd): Boolean =
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            prefs().edit()
                .putString(prefix(accountIdB64) + row.idHex, json.encodeToString(OwedEnd.serializer(), row))
                .commit()
        } ?: false

    override fun remove(accountIdB64: String, idHex: String) {
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            prefs().edit().remove(prefix(accountIdB64) + idHex.lowercase()).commit()
        }
    }

    override fun clearForAccount(accountIdB64: String) {
        val prefix = prefix(accountIdB64)
        val p = prefs()
        val editor = p.edit()
        for (key in p.all.keys) if (key.startsWith(prefix)) editor.remove(key)
        editor.commit()
    }

    internal companion object {
        const val PREFS_NAME = "lakko-membership-ends"
    }
}

// ---------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------

/** What the last MLS step came to. */
internal enum class MlsDelete {
    DELETED,

    /** The core refused: the stored group is active above the register's
     *  epoch, a Welcome got here first. Nothing changed. */
    STILL_ACTIVE,
    FAILED,
}

/** The live half of [runMembershipEnd]. Each may run more than once. */
internal interface MembershipEndPorts {
    /** Step 2: write the register. */
    suspend fun publishEnded(row: OwedEnd): EndPublish

    /** Step 3: stop the poll, the beacon and the rotation, retire the push
     *  subscription. A failed retire is this port's to keep owed: it does not
     *  hold the wipe back. */
    suspend fun stop(row: OwedEnd)

    /** Step 4: every store of the conversation, the group record's timeline
     *  with them. Throws when a store refused. */
    suspend fun wipeStores(row: OwedEnd)

    /** `deleteLocally`, or `deleteAfterEndedElsewhere` with the row's epoch. A
     *  group this device no longer holds is [MlsDelete.DELETED]. */
    suspend fun deleteMls(row: OwedEnd): MlsDelete

    /** The provider snapshot, durably. `false` when it did not land. */
    suspend fun persistDurably(): Boolean

    /** LAST: the known-groups row and the in-memory record. */
    suspend fun dropRegistry(row: OwedEnd)

    /** The core refused at the last step: the end is off, and this device
     *  answers with `joined`. */
    suspend fun cameBack(row: OwedEnd)
}

/** How a run ended. */
internal enum class EndRun {
    /** Nothing is owed but the notice, if any. */
    DONE,

    /** The synced state is not open: the next run resumes here. */
    WAITING,

    /** A step failed: the next run resumes here. */
    RETRY,

    /** The group came back: the record is gone, nothing was deleted. */
    ABANDONED,
}

/**
 * Walk [row] to its end. The stage is written only after what it names is done,
 * so a crash re-runs one step, and every step can be re-run. Never throws but
 * for a cancellation, which leaves the row where it is.
 */
internal suspend fun runMembershipEnd(
    accountIdB64: String,
    row: OwedEnd,
    store: MembershipEndStore,
    ports: MembershipEndPorts,
): EndRun {
    var current = row
    suspend fun advance(stage: EndStage): Boolean {
        val next = current.copy(stage = stage)
        if (!store.save(accountIdB64, next)) return false
        current = next
        return true
    }
    try {
        if (current.stage == EndStage.RECORDED) {
            when (ports.publishEnded(current)) {
                EndPublish.NOT_OPEN -> return EndRun.WAITING
                EndPublish.PUBLISHED, EndPublish.NOTHING_TO_TELL -> Unit
            }
            if (!advance(EndStage.PUBLISHED)) return EndRun.RETRY
        }
        if (current.stage == EndStage.PUBLISHED) {
            ports.stop(current)
            if (!advance(EndStage.STOPPED)) return EndRun.RETRY
        }
        if (current.stage == EndStage.STOPPED) {
            ports.wipeStores(current)
            when (ports.deleteMls(current)) {
                MlsDelete.DELETED -> Unit
                MlsDelete.FAILED -> return EndRun.RETRY
                MlsDelete.STILL_ACTIVE -> {
                    store.remove(accountIdB64, current.idHex)
                    ports.cameBack(current)
                    return EndRun.ABANDONED
                }
            }
            if (!ports.persistDurably()) return EndRun.RETRY
            if (!advance(EndStage.WIPED)) return EndRun.RETRY
        }
        if (current.stage == EndStage.WIPED) {
            ports.dropRegistry(current)
            if (current.noticeOwed) {
                if (!advance(EndStage.NOTICE_ONLY)) return EndRun.RETRY
            } else {
                store.remove(accountIdB64, current.idHex)
            }
        }
        return EndRun.DONE
    } catch (cancellation: CancellationException) {
        throw cancellation
    } catch (_: Throwable) {
        return EndRun.RETRY
    }
}

// ---------------------------------------------------------------------------
// The notice
// ---------------------------------------------------------------------------

/** "You are no longer in {name}.", owed once on this device. */
public data class MembershipEndNotice(
    val idHex: String,
    val name: String,
    /** When the end was recorded, unix ms (`0` when not known). */
    val recordedAtMs: Long = 0L,
)

/** The notices [rows] owe, oldest first. */
internal fun endNoticesOf(rows: List<OwedEnd>): List<MembershipEndNotice> =
    rows.filter { it.noticeOwed }
        .sortedWith(compareBy<OwedEnd> { it.recordedAtMs }.thenBy { it.idHex })
        .map { MembershipEndNotice(it.idHex, it.name, it.recordedAtMs) }

/** The conversations that are on their way out: gone from the list at once. */
internal fun endingIdsOf(rows: List<OwedEnd>): Set<String> =
    rows.filter { it.stage != EndStage.NOTICE_ONLY }.map { it.idHex }.toSet()

/**
 * ADR-0087 UX 9.1 — the Chats banners: one per removal still owed, the newest
 * on top, each with its own close; closing one leaves the others, and a newer
 * removal never hides an older one not yet closed (seen 2026-11-04: only the
 * older group's banner showed and the new removal said nothing).
 */
internal fun bannerNotices(notices: List<MembershipEndNotice>): List<MembershipEndNotice> =
    notices.distinctBy { it.idHex }
        .sortedWith(compareByDescending<MembershipEndNotice> { it.recordedAtMs }.thenBy { it.idHex })

/** The notice's sentence. An unnamed group reads by the catalog's own word. */
internal fun endNoticeLine(name: String, fallbackName: String): UiText =
    UiText("chat.ended.notice", texts = mapOf("name" to name.ifBlank { fallbackName }))

/** What a dialog over the open conversation shows besides the sentence. */
internal data class EndDialog(
    val name: String,
    /** What the composer held, verbatim, or `null` when nothing worth keeping. */
    val draft: String?,
)

/**
 * Whether an open conversation left the list for good. An end under way is not
 * that yet: the list drops the row a step before the notice reaches the state,
 * and leaving now would close the screen the dialog belongs on.
 */
internal fun conversationIsGone(sawConversation: Boolean, held: Boolean, endOwed: Boolean): Boolean =
    sawConversation && !held && !endOwed

/**
 * What the screen under the dialog takes the group's `isActive` to be. The wipe
 * deletes the group, the read then answers `null`, and `null` means active
 * ([membershipState]): the composer and the call buttons came back behind the
 * dialog. An end under way is not active, whatever the read says.
 */
internal fun activeBehindEnd(isActive: Boolean?, ending: Boolean): Boolean? =
    if (ending) false else isActive

/**
 * The title the screen under the dialog shows. The wipe deletes the name and
 * the roster a title is made from, so it would turn into "Group": [held] is the
 * title from the last moment the conversation was whole.
 */
internal fun titleBehindEnd(live: String?, held: String?, ending: Boolean): String? =
    if (ending) held ?: live else live

/**
 * Whether the title on screen may become the one [titleBehindEnd] holds: only
 * while the conversation is whole, so a title read during the wipe never is.
 */
internal fun holdsTitle(threadKnown: Boolean, ending: Boolean): Boolean = threadKnown && !ending

/**
 * "This conversation is still being set up on this device." The list drops a
 * conversation the moment its end is recorded, which reads exactly like one not
 * loaded yet, and for that person the sentence would be false.
 */
internal fun showsSettingUpLine(threadKnown: Boolean, ending: Boolean): Boolean = !threadKnown && !ending

/**
 * The terminal row under the timeline. It says the messages already received
 * are still here, which is true of a retired leaf ([MembershipEnded.kt]) and
 * false of an end, whose dialog says they are gone: an end shows none.
 */
internal fun terminalRowBehindEnd(state: MembershipState, ending: Boolean): TerminalRowCopy? =
    if (ending) null else terminalRow(state)

/** What the group's info screen shows under the dialog. */
internal data class InfoBehindEnd(
    /** The exit block: Leave, Transfer, or `Delete this chat`. */
    val exitBlock: Boolean,
    /** "This list stopped updating when your membership ended." */
    val frozenRosterLine: Boolean,
    /** This group's `Members can find me` switch. */
    val membersCanFindMe: Boolean,
)

/**
 * The info screen under the dialog keeps the name and the member list and
 * drops what offers an action or describes a state the dialog contradicts: an
 * exit (the group is already gone), the frozen-roster line (which belongs to a
 * retired leaf that keeps its history), and the find-me switch (there is no
 * membership left to be found through).
 */
internal fun infoBehindEnd(ending: Boolean, membershipEnded: Boolean): InfoBehindEnd =
    InfoBehindEnd(
        exitBlock = !ending,
        frozenRosterLine = membershipEnded && !ending,
        membersCanFindMe = !ending,
    )

/** The dialog for [notice] over a composer holding [draft]. */
internal fun endDialogFor(notice: MembershipEndNotice, draft: String): EndDialog =
    EndDialog(notice.name, draft.takeIf { it.isNotBlank() })

// ---------------------------------------------------------------------------
// Added again
// ---------------------------------------------------------------------------

/** What becomes of the record a Welcome replaces. */
internal enum class JoinReplacement {
    /** No record held: an ordinary join. */
    NONE,

    /** A retired leaf came back: the record keeps its history, with the new
     *  handle in it. */
    KEEP_HISTORY,

    /** The person was out: an ordinary new conversation, nothing from before. */
    FRESH,
}

/**
 * Decide it. [accountStillInRoster] is `null` when the old handle could not be
 * read, which keeps the history: only a read that says the account was out
 * throws anything away.
 */
internal fun joinReplacement(oldHeld: Boolean, endOwed: Boolean, accountStillInRoster: Boolean?): JoinReplacement {
    if (!oldHeld) return JoinReplacement.NONE
    if (endOwed) return JoinReplacement.FRESH
    return if (accountStillInRoster == false) JoinReplacement.FRESH else JoinReplacement.KEEP_HISTORY
}

// ---------------------------------------------------------------------------
// A Welcome for a group this device still holds as active
// ---------------------------------------------------------------------------

/**
 * The group id a refused Welcome names, or `null` when [error] is not that
 * refusal. The FFI message of `MembershipGroupStillActive` is exactly the
 * lower-case hex id, and `ChatError` wraps it, so the causes are walked.
 */
internal fun groupStillActiveHex(error: Throwable?): String? {
    var current = error
    var depth = 0
    while (current != null && depth < MAX_CAUSES) {
        if (current is FfiException.MembershipGroupStillActive) {
            return current.message?.takeIf { it.isNotEmpty() && it.all { c -> c in '0'..'9' || c in 'a'..'f' } }
        }
        current = current.cause
        depth += 1
    }
    return null
}

private const val MAX_CAUSES = 6

/** A Welcome kept for a group this device still holds as active. */
@Serializable
internal data class HeldWelcome(
    val groupIdHex: String,
    val welcomeB64: String,
)

/** What to do with a held Welcome once the group's journal was read. */
internal enum class HeldWelcomeVerdict {
    /** The journal was not read to its end: a refused read decides nothing. */
    KEEP,

    /** The group ended here, or is gone: join. */
    RETRY,

    /** The journal is drained and the group is still active: the core cannot
     *  report the Welcome's epoch, so this is a duplicate or a forgery. */
    DROP,
}

/** [groupActive] is `null` when this device no longer holds the group. */
internal fun heldWelcomeVerdict(journalDrained: Boolean, groupActive: Boolean?): HeldWelcomeVerdict = when {
    !journalDrained -> HeldWelcomeVerdict.KEEP
    groupActive == true -> HeldWelcomeVerdict.DROP
    else -> HeldWelcomeVerdict.RETRY
}

/** Held Welcomes per group, a few at most: one commit answers all of them. */
internal const val MAX_HELD_WELCOMES_PER_GROUP: Int = 4

/** Where held Welcomes live. */
internal interface HeldWelcomeStore {
    fun load(accountIdB64: String): List<HeldWelcome>

    /** `false` when the row did not reach the disk; the caller then keeps the
     *  mailbox cursor, or the Welcome is lost. */
    fun add(accountIdB64: String, welcome: HeldWelcome): Boolean

    fun remove(accountIdB64: String, welcome: HeldWelcome)

    fun clearForAccount(accountIdB64: String)
}

/** [rows] with [welcome] in it, identical bytes once, the oldest of a group
 *  gone past [MAX_HELD_WELCOMES_PER_GROUP]. */
internal fun withHeldWelcome(rows: List<HeldWelcome>, welcome: HeldWelcome): List<HeldWelcome> {
    if (welcome in rows) return rows
    val next = rows + welcome
    val ofGroup = next.count { it.groupIdHex == welcome.groupIdHex }
    if (ofGroup <= MAX_HELD_WELCOMES_PER_GROUP) return next
    val oldest = next.first { it.groupIdHex == welcome.groupIdHex }
    return next - oldest
}

/** The production store. A Welcome is ciphertext sealed to a KeyPackage, so a
 *  plain file holds nothing readable; written with `commit`. */
internal class SharedPrefsHeldWelcomeStore(context: Context) : HeldWelcomeStore {

    private val appContext: Context = context.applicationContext
    private val json = Json { ignoreUnknownKeys = true }
    private val serializer = kotlinx.serialization.builtins.ListSerializer(HeldWelcome.serializer())

    private fun prefs(): SharedPreferences =
        appContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    private fun key(accountIdB64: String) = "v1:$accountIdB64"

    override fun load(accountIdB64: String): List<HeldWelcome> = try {
        prefs().getString(key(accountIdB64), null)
            ?.let { runCatching { json.decodeFromString(serializer, it) }.getOrNull() }
            .orEmpty()
    } catch (_: Throwable) {
        emptyList()
    }

    private fun write(accountIdB64: String, rows: List<HeldWelcome>): Boolean =
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            prefs().edit().putString(key(accountIdB64), json.encodeToString(serializer, rows)).commit()
        } ?: false

    override fun add(accountIdB64: String, welcome: HeldWelcome): Boolean = synchronized(LOCK) {
        write(accountIdB64, withHeldWelcome(load(accountIdB64), welcome))
    }

    override fun remove(accountIdB64: String, welcome: HeldWelcome) {
        synchronized(LOCK) { write(accountIdB64, load(accountIdB64) - welcome) }
    }

    override fun clearForAccount(accountIdB64: String) {
        prefs().edit().remove(key(accountIdB64)).commit()
    }

    internal companion object {
        const val PREFS_NAME = "lakko-held-welcomes"
        private val LOCK = Any()
    }
}

// ---------------------------------------------------------------------------
// A group of one
// ---------------------------------------------------------------------------

/**
 * `docs/ux/group-and-channel-privacy.md` section 10 — "Call controls: absent.
 * There is nobody to call. They appear with a second person."
 *
 * [people] counts people, not leaves (ADR-0050): a person alone on two phones
 * is still alone. `null` is a roster not read yet; a group keeps them off until
 * it is, so a group of one never flashes them. A one-to-one always keeps them.
 */
internal fun showsCallControls(isGroup: Boolean, people: Int?): Boolean =
    !isGroup || (people ?: 0) >= 2

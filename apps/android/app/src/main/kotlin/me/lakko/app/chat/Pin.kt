// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.content.Context
import android.content.SharedPreferences
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.serialization.Serializable
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import me.lakko.app.l10n.Localization
import me.lakko.app.l10n.Strings
import me.lakko.app.l10n.UiText
import me.lakko.app.push.GroupPushRouting

/**
 * ADR-0052 decision 3 + `docs/ux/message-actions.md` section 4 — pins on the
 * Android client: the two stores, the receive-side verdict, the wire peek, the
 * pinned bar's ordering and the action sheet's options.
 *
 * ## Two features that share a word
 *
 * A **personal** pin is a flag on this device, keyed `(conversation,
 * messageId)`. Nothing is sent. A **shared** pin is group state carried as a
 * bounded envelope on an MLS application message (`lakko/pin/v1`), one
 * envelope per action on ONE message, stamped with the sender's clock. The
 * sender's authority is the RECEIVER's check: the MLS sender leaf is resolved
 * to the account key at receipt (C-68), and the live role table decides —
 * owner or admin, any member when the group has no table, either participant
 * in a one-to-one. Then the stamp decides FOR THAT MESSAGE: strictly newer
 * wins, equal is not newer (`pinIsNewer`), so a growth re-send of a stored
 * pin is not an update and a late pin can never undo a later unpin. An unpin
 * is STORED, never deleted: its stamp is what keeps a stale pin of the same
 * message from coming back.
 *
 * ## Why the rules are pure functions
 *
 * The group name ([groupNameVerdict]) is the template, for the same reason:
 * every rule must be readable and testable on the JVM without the UniFFI
 * `.so`, and the receive verdict must run against CURRENT state, under the
 * per-group lock, inside the poll's own critical section (C-62 sub-invariant
 * 5). The repository reads the role table and the stored row, calls the
 * verdict, and writes back — nothing in between.
 */

/** One shared-pin record: the message's current pinned state and the stamp
 *  that set it. An unpin is a record with [pinned] `false`. */
public data class StoredPin(
    val pinned: Boolean,
    val updatedAtMs: ULong,
)

/** The receive-side decision for an incoming shared pin or unpin. */
public enum class PinVerdict(public val accepts: Boolean) {
    /** A newer action from the ADR-0022 role-table owner. */
    ACCEPT_OWNER(true),

    /** A newer action from an ADR-0022 admin. */
    ACCEPT_ADMIN(true),

    /** A newer action in a group with no role table (legacy / open group). */
    ACCEPT_NO_ROLE_TABLE(true),

    /** A newer action from the other participant of a one-to-one. */
    ACCEPT_ONE_TO_ONE(true),

    /** A plain member of a role-table group, or a key not in the roster. */
    REJECT_NOT_AUTHORIZED(false),

    /** The role table could not be read, or the sender could not be resolved
     *  where its identity decides. Fail closed. */
    REJECT_UNVERIFIABLE(false),

    /** Authorised, but not strictly newer than what is stored for that
     *  message — a re-send, a relay, or a pin racing a later unpin. */
    REJECT_STALE(false),
}

/**
 * Decide whether an incoming action stamped [incomingUpdatedAtMs] replaces
 * [stored] for one message.
 *
 * @param senderSignatureKey the sender's account key resolved from the MLS
 *   leaf at receipt, raw bytes — `null` when unresolvable.
 * @param owner the live role table (owner + admins), no table, or unreadable.
 * @param isOneToOne two people in the conversation: either may pin or unpin,
 *   whatever the role table says (ADR-0052 decision 3 — inventing an asymmetry
 *   between two people would let one pin what the other cannot remove).
 * @param isNewer the canonical order — the FFI `pinIsNewer`, threaded in so
 *   the comparison is defined once in Rust.
 */
public fun pinVerdict(
    senderSignatureKey: ByteArray?,
    owner: GroupPushRouting.OwnerLookup,
    isOneToOne: Boolean,
    incomingUpdatedAtMs: ULong,
    stored: StoredPin?,
    isNewer: (incomingMs: ULong, storedMs: ULong) -> Boolean,
): PinVerdict {
    // (1) Authority — who may pin for everyone. A one-to-one is decided
    // before the role table is consulted: `Group::create` seeds an owner
    // table even for two people, and the spec gives both participants the
    // same standing.
    val authorised = when {
        isOneToOne -> PinVerdict.ACCEPT_ONE_TO_ONE
        else -> when (owner) {
            is GroupPushRouting.OwnerLookup.Unavailable -> PinVerdict.REJECT_UNVERIFIABLE
            is GroupPushRouting.OwnerLookup.NoRoleTable -> PinVerdict.ACCEPT_NO_ROLE_TABLE
            is GroupPushRouting.OwnerLookup.Known -> when {
                senderSignatureKey == null -> PinVerdict.REJECT_UNVERIFIABLE
                senderSignatureKey.contentEquals(owner.owner) -> PinVerdict.ACCEPT_OWNER
                owner.admins.any { it.contentEquals(senderSignatureKey) } -> PinVerdict.ACCEPT_ADMIN
                else -> PinVerdict.REJECT_NOT_AUTHORIZED
            }
        }
    }
    if (!authorised.accepts) return authorised
    // (2) Order — which authorised action survives, per message. Nothing
    // stored: the first action. Equal is NOT newer.
    if (stored == null) return authorised
    return if (isNewer(incomingUpdatedAtMs, stored.updatedAtMs)) authorised else PinVerdict.REJECT_STALE
}

/**
 * Whether THIS device may set or clear a shared pin — the local half of the
 * same rule the receiver applies: either participant of a one-to-one, any
 * member of a group with no role table, else the owner or an admin.
 * [ownRole] is `null` when the role could not be read; that fails closed
 * unless the group is known to carry no table.
 */
public fun mayPinForEveryone(ownRole: uniffi.lakko_ffi_uniffi.Role?, hasRoleTable: Boolean, isOneToOne: Boolean): Boolean =
    when {
        isOneToOne -> true
        !hasRoleTable -> true
        else -> ownRole == uniffi.lakko_ffi_uniffi.Role.OWNER || ownRole == uniffi.lakko_ffi_uniffi.Role.ADMIN
    }

// -----------------------------------------------------------------------------
// The wire peek
// -----------------------------------------------------------------------------

/**
 * ADR-0052 decision 3 — the frozen pin envelope domain separator,
 * `lakko/pin/v1` followed by one NUL byte (13 bytes). Built programmatically:
 * a raw NUL in a source file makes git treat it as binary.
 */
internal val PIN_ENVELOPE_DOMAIN: ByteArray =
    "lakko/pin/v1".toByteArray(Charsets.US_ASCII) + byteArrayOf(0)

/**
 * Does this application plaintext open with the pin domain separator?
 *
 * As with the profile and group-name peeks this is a complete
 * CLASSIFICATION: the separator ends in a NUL byte, so no chat envelope (`{`)
 * and no legacy UTF-8 message a person typed can open with it. A plaintext
 * that does is a pin envelope whether or not it decodes; one that does not
 * decode is DROPPED, not handed to the chat parser whose lossy-UTF-8 fallback
 * would render the domain string as a bubble. `internal` so the background
 * push-preview filter reuses this exact test. Pure — host-testable.
 */
internal fun looksLikePinEnvelope(bytes: ByteArray): Boolean {
    if (bytes.size < PIN_ENVELOPE_DOMAIN.size) return false
    for (i in PIN_ENVELOPE_DOMAIN.indices) {
        if (bytes[i] != PIN_ENVELOPE_DOMAIN[i]) return false
    }
    return true
}

// -----------------------------------------------------------------------------
// The stores
// -----------------------------------------------------------------------------

private val pinJson = Json { ignoreUnknownKeys = true }

/** One persisted shared-pin row; the stamp as text so a full `u64` survives. */
@Serializable
private data class StoredPinRow(
    val messageId: String = "",
    val pinned: Boolean = false,
    val updatedAtMs: String = "",
)

/** One persisted personal-pin row: the message and when this device pinned it. */
@Serializable
private data class PersonalPinRow(
    val messageId: String = "",
    val pinnedAtMs: String = "",
)

/** The shared map as a JSON array of rows. */
internal fun encodeSharedPins(value: Map<Long, StoredPin>): String =
    pinJson.encodeToString(
        value.entries
            .sortedBy { it.key }
            .map { (id, pin) -> StoredPinRow(id.toString(), pin.pinned, pin.updatedAtMs.toString()) },
    )

/** The inverse of [encodeSharedPins]; garbage reads as empty, a malformed row is skipped. */
internal fun decodeSharedPins(raw: String?): Map<Long, StoredPin> {
    if (raw.isNullOrEmpty()) return emptyMap()
    return try {
        val rows = pinJson.decodeFromString<List<StoredPinRow>>(raw)
        val out = LinkedHashMap<Long, StoredPin>()
        for (row in rows) {
            val id = row.messageId.toLongOrNull() ?: continue
            val stamp = row.updatedAtMs.toULongOrNull() ?: continue
            out[id] = StoredPin(pinned = row.pinned, updatedAtMs = stamp)
        }
        out
    } catch (_: Exception) {
        emptyMap()
    }
}

/** The personal map (`messageId -> pinnedAtMs`) as a JSON array of rows. */
internal fun encodePersonalPins(value: Map<Long, ULong>): String =
    pinJson.encodeToString(
        value.entries.sortedBy { it.key }.map { (id, at) -> PersonalPinRow(id.toString(), at.toString()) },
    )

/** The inverse of [encodePersonalPins]; garbage reads as empty. */
internal fun decodePersonalPins(raw: String?): Map<Long, ULong> {
    if (raw.isNullOrEmpty()) return emptyMap()
    return try {
        val rows = pinJson.decodeFromString<List<PersonalPinRow>>(raw)
        val out = LinkedHashMap<Long, ULong>()
        for (row in rows) {
            val id = row.messageId.toLongOrNull() ?: continue
            val at = row.pinnedAtMs.toULongOrNull() ?: continue
            out[id] = at
        }
        out
    } catch (_: Exception) {
        emptyMap()
    }
}

/**
 * The per-`(account, group)` pin stores: plain `SharedPreferences` (a pin
 * names a message the conversation already holds, not key material). One
 * row per conversation and scope. [version] bumps on every write so a bar
 * computed off the stores recomposes.
 *
 * The shared map is wiped with the conversation ([deleteGroup]); both maps
 * go with the device ([deleteAccount]).
 */
public object PinStore {
    private const val PREFS = "lakko-pins"
    private const val SHARED_PREFIX = "lakko.pin.shared.v1:"
    private const val PERSONAL_PREFIX = "lakko.pin.personal.v1:"

    private val mutableVersion = MutableStateFlow(0L)

    /** Bumped after every write; observe it to re-read the bar. */
    public val version: StateFlow<Long> = mutableVersion.asStateFlow()

    private fun prefs(context: Context): SharedPreferences =
        context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    private fun sharedKey(accountIdB64: String, groupIdHex: String): String =
        SHARED_PREFIX + accountIdB64 + ":" + groupIdHex.lowercase()

    private fun personalKey(accountIdB64: String, groupIdHex: String): String =
        PERSONAL_PREFIX + accountIdB64 + ":" + groupIdHex.lowercase()

    /** The conversation's shared pin map, unpins included; empty when none. */
    public fun loadShared(context: Context, accountIdB64: String?, groupIdHex: String): Map<Long, StoredPin> {
        if (accountIdB64 == null) return emptyMap()
        return decodeSharedPins(prefs(context).getString(sharedKey(accountIdB64, groupIdHex), null))
    }

    public fun saveShared(context: Context, accountIdB64: String, groupIdHex: String, value: Map<Long, StoredPin>) {
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            prefs(context).edit().putString(sharedKey(accountIdB64, groupIdHex), encodeSharedPins(value)).apply()
            mutableVersion.value += 1
    
        }
    }

    /** This device's personal pins in the conversation, `messageId -> pinnedAtMs`. */
    public fun loadPersonal(context: Context, accountIdB64: String?, groupIdHex: String): Map<Long, ULong> {
        if (accountIdB64 == null) return emptyMap()
        return decodePersonalPins(prefs(context).getString(personalKey(accountIdB64, groupIdHex), null))
    }

    public fun savePersonal(context: Context, accountIdB64: String, groupIdHex: String, value: Map<Long, ULong>) {
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            prefs(context).edit().putString(personalKey(accountIdB64, groupIdHex), encodePersonalPins(value)).apply()
            mutableVersion.value += 1
    
        }
    }

    /** Forget every pin of one conversation (delete this chat). */
    public fun deleteGroup(context: Context, accountIdB64: String, groupIdHex: String) {
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            prefs(context).edit()
                .remove(sharedKey(accountIdB64, groupIdHex))
                .remove(personalKey(accountIdB64, groupIdHex))
                .apply()
            mutableVersion.value += 1
    
        }
    }

    /** Forget every pin held for [accountIdB64] (forget device). */
    public fun deleteAccount(context: Context, accountIdB64: String) {
        val p = prefs(context)
        val e = p.edit()
        val sharedPrefix = SHARED_PREFIX + accountIdB64 + ":"
        val personalPrefix = PERSONAL_PREFIX + accountIdB64 + ":"
        for (k in p.all.keys) if (k.startsWith(sharedPrefix) || k.startsWith(personalPrefix)) e.remove(k)
        e.apply()
        mutableVersion.value += 1
    }
}

// -----------------------------------------------------------------------------
// The pinned bar
// -----------------------------------------------------------------------------

/** Which scope a bar entry comes from. A message pinned in both is shown as
 *  [SHARED]: a shared pin outranks a personal one. */
public enum class PinScope { SHARED, PERSONAL }

/** One row of the pinned bar / list: the message and the scope it is shown for. */
public data class PinBarEntry(
    val messageId: Long,
    val scope: PinScope,
)

/**
 * The bar's entries, in the order the bar cycles them (section 4.1): shared
 * pins before personal ones, each scope newest pin first (by its stamp, then
 * by message id). A pin whose message this device does not hold, or holds
 * deleted, is skipped — kept in the store, not shown. A message pinned in
 * both scopes appears once, as shared.
 */
public fun pinBarEntries(
    shared: Map<Long, StoredPin>,
    personal: Map<Long, ULong>,
    messages: List<ChatMessage>,
): List<PinBarEntry> {
    val held = HashMap<Long, ChatMessage>(messages.size)
    for (m in messages) if (m.messageId != 0L) held[m.messageId] = m
    fun shown(id: Long): Boolean {
        val m = held[id] ?: return false
        return !m.deleted
    }
    val sharedRows = shared.entries
        .filter { it.value.pinned && shown(it.key) }
        .sortedWith(compareByDescending<Map.Entry<Long, StoredPin>> { it.value.updatedAtMs }.thenByDescending { it.key })
        .map { PinBarEntry(it.key, PinScope.SHARED) }
    val sharedIds = sharedRows.mapTo(HashSet()) { it.messageId }
    val personalRows = personal.entries
        .filter { it.key !in sharedIds && shown(it.key) }
        .sortedWith(compareByDescending<Map.Entry<Long, ULong>> { it.value }.thenByDescending { it.key })
        .map { PinBarEntry(it.key, PinScope.PERSONAL) }
    return sharedRows + personalRows
}

/** The index the bar shows after a tap on [current] of [count]: the next
 *  older pin, wrapping past the oldest to the newest. */
public fun advancePinIndex(current: Int, count: Int): Int {
    if (count <= 0) return 0
    return (current.coerceIn(0, count - 1) + 1) % count
}

/** Clamp a remembered position into a list that may have shrunk. */
public fun clampPinIndex(current: Int, count: Int): Int =
    if (count <= 0) 0 else current.coerceIn(0, count - 1)

/** The bar's excerpt for [message]: its text on one line, else the same
 *  one-word stand-in the reply quote uses (`Photo`, `Voice message`, ...). */
public fun pinExcerpt(message: ChatMessage, strings: Strings = Localization.strings.value): String =
    replySnapshotFor(message, "").excerpt ?: strings[REPLY_FALLBACK_EXCERPT_KEY]

/** The label a personal pin carries in place of the author (section 4.1). */
public val PIN_FOR_YOU_LABEL: UiText = UiText("chat.pinned_bar.for_you")

// -----------------------------------------------------------------------------
// The action row and the sheet
// -----------------------------------------------------------------------------

/** One choice on the pin sheet, or the one act a single-scope row performs. */
public enum class PinAction { PIN_ME, PIN_EVERYONE, UNPIN_ME, UNPIN_EVERYONE }

/**
 * What the card's row says and what tapping it offers. One action means the
 * row acts at once; two mean the sheet asks which.
 */
public data class PinMenu(
    val label: UiText,
    val actions: List<PinAction>,
)

/**
 * Section 4.1 — the row and the sheet for one message as this device sees it.
 *
 * The row reads `Unpin` when this device can undo a pin in at least one
 * scope: a personal pin of its own, or a shared pin when it has the standing
 * to clear one. Otherwise it reads `Pin`, and offers `Pin for me` always plus
 * `Pin for everyone` when the standing is there and no shared pin is set.
 * A plain member holding a message an admin pinned therefore sees `Pin` with
 * `Pin for me` alone, or `Unpin` for me only once they also pinned it.
 */
public fun pinMenuFor(
    sharedPinned: Boolean,
    personalPinned: Boolean,
    mayShared: Boolean,
): PinMenu {
    val unpins = buildList {
        if (personalPinned) add(PinAction.UNPIN_ME)
        if (sharedPinned && mayShared) add(PinAction.UNPIN_EVERYONE)
    }
    if (unpins.isNotEmpty()) return PinMenu(label = UiText("chat.message.unpin"), actions = unpins)
    val pins = buildList {
        add(PinAction.PIN_ME)
        if (mayShared && !sharedPinned) add(PinAction.PIN_EVERYONE)
    }
    return PinMenu(label = UiText("chat.message.pin"), actions = pins)
}

/** The sheet row's title. In a one-to-one the shared scope reads `both`. */
public fun pinActionLabel(action: PinAction, oneToOne: Boolean): UiText = UiText(
    when (action) {
        PinAction.PIN_ME -> "chat.pin.for_me"
        PinAction.UNPIN_ME -> "chat.unpin.for_me"
        PinAction.PIN_EVERYONE -> if (oneToOne) "chat.pin.for_both" else "chat.pin.for_everyone"
        PinAction.UNPIN_EVERYONE -> if (oneToOne) "chat.unpin.for_both" else "chat.unpin.for_everyone"
    },
)

/** The one line under each sheet row saying what it does. */
public fun pinActionSubline(action: PinAction): UiText = UiText(
    when (action) {
        PinAction.PIN_ME, PinAction.UNPIN_ME -> "chat.pin.detail_personal"
        PinAction.PIN_EVERYONE, PinAction.UNPIN_EVERYONE -> "chat.pin.detail_shared"
    },
)

/** UX copy. */
public val PIN_LIST_LABEL: UiText = UiText("chat.pinned_bar.list_title")
public val PIN_SEND_FAILED: UiText = UiText("chat.error.pin")

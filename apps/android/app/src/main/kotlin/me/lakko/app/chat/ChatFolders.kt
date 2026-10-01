// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.l10n.UiText
import uniffi.lakko_ffi_uniffi.ConversationRow as SharedConversationRow
import uniffi.lakko_ffi_uniffi.FolderChangeSet
import uniffi.lakko_ffi_uniffi.FolderConversationKind
import uniffi.lakko_ffi_uniffi.FolderConversationRef
import uniffi.lakko_ffi_uniffi.FolderMemberRegister
import uniffi.lakko_ffi_uniffi.FolderMemberState
import uniffi.lakko_ffi_uniffi.FolderMetaRegister
import uniffi.lakko_ffi_uniffi.FolderOrderRegister
import uniffi.lakko_ffi_uniffi.FolderPinsRegister
import uniffi.lakko_ffi_uniffi.FolderRemovedRegister
import uniffi.lakko_ffi_uniffi.FolderRule
import uniffi.lakko_ffi_uniffi.FolderView

/**
 * Chat folders (`docs/ux/chat-folders.md`, ADR-0058): the client half.
 *
 * **What is not here.** Which chats a folder holds is `folder_matches`, and
 * the order inside it is `order_conversations_in_folder`, both in
 * `crates/lakko-ui-rules`; how two devices' edits combine is the fold in
 * `lakko-sync`. Three clients each writing those would disagree about which
 * chats are in "Work". What IS here is the mapping between the rows Compose
 * draws and the references a folder stores, and turning a person's edit into
 * the change set that carries ONLY the registers it changed - an edit that
 * rewrote a register it did not touch could delete, on every device, a chat
 * this device simply does not hold yet (ADR-0058 section 5a).
 *
 * Pure of Compose and Android, and the two shared rules arrive as parameters
 * whose defaults are the real bindings, so a host JVM test can drive it.
 */
public object FolderLimits {
    /** Folders per account (UX spec 3.4). */
    public const val MAX_FOLDERS: Int = 20

    /** Characters in a name. */
    public const val MAX_NAME: Int = 24

    /** Chats added by hand, and separately chats excluded by hand. */
    public const val MAX_HAND: Int = 200

    /** Pins inside one folder. */
    public const val MAX_PINS: Int = 10
}

/** `include_kinds` bits (ADR-0058 section 2). */
public object IncludeKind {
    public const val ONE_TO_ONE: UInt = 1u
    public const val GROUPS: UInt = 2u
    public const val CHANNELS: UInt = 4u
}

/** `exclude_flags` bits. */
public object ExcludeFlag {
    public const val MUTED: UInt = 1u
    public const val READ: UInt = 2u
    public const val ARCHIVED: UInt = 4u
}

/** One folder, as this client reads it off the fold. */
public data class ChatFolder(
    /** 16 bytes, minted once. */
    val id: ByteArray,
    val name: String,
    val includeKinds: UInt,
    val excludeFlags: UInt,
    /** Chats added by hand. Held whether or not this device can resolve them. */
    val included: List<FolderConversationRef>,
    /** Chats excluded by hand. */
    val excluded: List<FolderConversationRef>,
    /** The folder's own pins, in their stored order. */
    val pinned: List<FolderConversationRef>,
) {
    /** A stable string form of [id], for keys and for the saved tab. */
    val idHex: String get() = hexOf(id)

    /** The rule in row terms, for `folder_matches`. */
    val rule: FolderRule
        get() = FolderRule(
            includeKinds = includeKinds,
            excludeFlags = excludeFlags,
            included = included.map(::rowIdOf),
            excluded = excluded.map(::rowIdOf),
        )

    /** The row ids of the pins, in order. */
    val pinnedRowIds: List<String> get() = pinned.map(::rowIdOf)

    override fun equals(other: Any?): Boolean =
        other is ChatFolder &&
            id.contentEquals(other.id) &&
            name == other.name &&
            includeKinds == other.includeKinds &&
            excludeFlags == other.excludeFlags &&
            included.map(::refKey) == other.included.map(::refKey) &&
            excluded.map(::refKey) == other.excluded.map(::refKey) &&
            pinned.map(::refKey) == other.pinned.map(::refKey)

    override fun hashCode(): Int = id.contentHashCode()

    public companion object {
        /** From the fold's view of one folder. */
        public fun of(view: FolderView): ChatFolder = ChatFolder(
            id = view.folderId,
            name = view.meta.name,
            includeKinds = view.meta.includeKinds,
            excludeFlags = view.meta.excludeFlags,
            included = view.included,
            excluded = view.excluded,
            pinned = view.pinned,
        )
    }
}

// ---------------------------------------------------------------------------
// Rows <-> references
// ---------------------------------------------------------------------------

/**
 * The row id a reference names. A chat or group row is keyed on its group id
 * hex and a channel row on its channel id, which is already 32-char lowercase
 * hex of the 16-byte id - so every kind is just the id's hex. The kinds cannot
 * collide: a group id is 32 bytes and a channel id is 16.
 */
public fun rowIdOf(ref: FolderConversationRef): String = hexOf(ref.id)

/** The reference that names [row], or `null` when its id is not hex. */
public fun refOf(row: ConversationListRow): FolderConversationRef? {
    val bytes = bytesOfHex(row.id) ?: return null
    val kind = when (row.kind) {
        ConversationRowKind.CHAT -> FolderConversationKind.ONE_TO_ONE
        ConversationRowKind.GROUP -> FolderConversationKind.GROUP
        ConversationRowKind.CHANNEL -> FolderConversationKind.CHANNEL
    }
    return FolderConversationRef(kind = kind, id = bytes)
}

/**
 * The identity of a reference for comparing and de-duplicating. The kind is
 * left out on purpose: a one-to-one that became a group is the same
 * conversation under the same id, and the rule matches on the id alone.
 */
internal fun refKey(ref: FolderConversationRef): String = rowIdOf(ref)

/** Whether [ref] names [row]. */
internal fun refNames(ref: FolderConversationRef, row: ConversationListRow): Boolean =
    rowIdOf(ref) == row.id.lowercase()

/** What the badge means: something unread, or a manual "mark as unread". */
public val ConversationListRow.hasUnread: Boolean get() = unread > 0 || unreadOverride

/** The fields the folder rule reads: the six of the order plus muted and
 *  has-unread (ADR-0058 section 6). */
internal fun ConversationListRow.toFolderRow(): SharedConversationRow = SharedConversationRow(
    id = id,
    kind = kind,
    title = title,
    lastActivityMs = lastActivityMs,
    pinned = pinned,
    archived = archived,
    muted = muted,
    hasUnread = hasUnread,
)

// ---------------------------------------------------------------------------
// What a folder shows
// ---------------------------------------------------------------------------

/** `folder_matches`, as a parameter so a host test can stand in for it. */
public typealias FolderMatcher = (FolderRule, SharedConversationRow) -> Boolean

/** `order_conversations_in_folder`, likewise. */
public typealias FolderOrderer = (List<SharedConversationRow>, List<String>) -> List<UInt>

internal val realFolderMatcher: FolderMatcher = { rule, row ->
    uniffi.lakko_ffi_uniffi.folderMatches(rule, row)
}

internal val realFolderOrderer: FolderOrderer = { rows, pins ->
    uniffi.lakko_ffi_uniffi.orderConversationsInFolder(rows, pins)
}

/** The rows of [rows] a rule matches, in list order. */
public fun rowsMatching(
    rule: FolderRule,
    rows: List<ConversationListRow>,
    matches: FolderMatcher = realFolderMatcher,
): List<ConversationListRow> = rows.filter { !it.membershipEnded && matches(rule, it.toFolderRow()) }

/**
 * The rows inside [folder], in the folder's order, with [ConversationListRow.pinned]
 * replaced by the folder's OWN pin (UX spec 2.3: the All-chats pin does not
 * carry into a folder).
 */
public fun folderRows(
    folder: ChatFolder,
    rows: List<ConversationListRow>,
    matches: FolderMatcher = realFolderMatcher,
    order: FolderOrderer = realFolderOrderer,
): List<ConversationListRow> {
    val inFolder = rowsMatching(folder.rule, rows, matches)
    if (inFolder.isEmpty()) return emptyList()
    val pins = folder.pinnedRowIds.toSet()
    val shown = inFolder.map { it.copy(pinned = it.id.lowercase() in pins) }
    return order(inFolder.map { it.toFolderRow() }, folder.pinnedRowIds).mapNotNull { index ->
        shown.getOrNull(index.toInt())
    }
}

/**
 * The tab's count: conversations in the folder with anything unread, not
 * counting muted ones (UX spec 2.1). Conversations, not messages.
 */
public fun folderUnreadCount(
    folder: ChatFolder,
    rows: List<ConversationListRow>,
    matches: FolderMatcher = realFolderMatcher,
): Int = rowsMatching(folder.rule, rows, matches).count { it.hasUnread && !it.muted }

/** The tab a screen opens on: 0 is All chats, `i + 1` is folder `i`. A saved
 *  folder that has since been deleted opens All chats (UX spec 2.5). */
public fun selectedTabIndex(savedFolderIdHex: String?, folders: List<ChatFolder>): Int {
    if (savedFolderIdHex.isNullOrEmpty()) return 0
    val i = folders.indexOfFirst { it.idHex == savedFolderIdHex }
    return if (i < 0) 0 else i + 1
}

// ---------------------------------------------------------------------------
// Edits -> change sets (only the registers that changed)
// ---------------------------------------------------------------------------

/** A stamp source: every register in a change set gets its own fresh stamp. */
public typealias StampSource = () -> ULong

private fun changeSet(
    metas: List<FolderMetaRegister> = emptyList(),
    members: List<FolderMemberRegister> = emptyList(),
    pins: List<FolderPinsRegister> = emptyList(),
    removed: List<FolderRemovedRegister> = emptyList(),
    order: FolderOrderRegister? = null,
): FolderChangeSet = FolderChangeSet(
    metas = metas,
    members = members,
    pins = pins,
    removed = removed,
    order = order,
)

/** Whether a change set carries nothing. */
public fun FolderChangeSet.isEmpty(): Boolean =
    metas.isEmpty() && members.isEmpty() && pins.isEmpty() && removed.isEmpty() && order == null

/** What the editor holds while a person edits: the whole folder as they see it. */
public data class FolderDraft(
    val name: String,
    val includeKinds: UInt,
    val excludeFlags: UInt,
    val included: List<FolderConversationRef>,
    val excluded: List<FolderConversationRef>,
) {
    /** UX spec 3.1: Save needs a name and something included. */
    val canSave: Boolean
        get() = name.trim().isNotEmpty() && (includeKinds != 0u || included.isNotEmpty())

    /** The rule this draft would save, for the live count. */
    val rule: FolderRule
        get() = FolderRule(
            includeKinds = includeKinds,
            excludeFlags = excludeFlags,
            included = included.map(::rowIdOf),
            excluded = excluded.map(::rowIdOf),
        )

    public companion object {
        /** A new folder: Archived excluded by default, nothing else. An
         *  optional first chat comes from "New folder..." on a row. */
        public fun empty(first: FolderConversationRef? = null): FolderDraft = FolderDraft(
            name = "",
            includeKinds = 0u,
            excludeFlags = ExcludeFlag.ARCHIVED,
            included = listOfNotNull(first),
            excluded = emptyList(),
        )

        /** An existing folder, with every reference it holds - including the
         *  ones this device cannot show, so saving never drops them. */
        public fun of(folder: ChatFolder): FolderDraft = FolderDraft(
            name = folder.name,
            includeKinds = folder.includeKinds,
            excludeFlags = folder.excludeFlags,
            included = folder.included,
            excluded = folder.excluded,
        )
    }

    /** Whether this draft says the same as [other]: the name as it would be
     *  saved, the toggles, and the same chats in each hand list. Drives
     *  "Discard changes?". */
    public fun sameAs(other: FolderDraft): Boolean =
        name.trim() == other.name.trim() &&
            includeKinds == other.includeKinds &&
            excludeFlags == other.excludeFlags &&
            included.map(::refKey).toSet() == other.included.map(::refKey).toSet() &&
            excluded.map(::refKey).toSet() == other.excluded.map(::refKey).toSet()
}

/**
 * Save an editor draft: create the folder when [original] is `null`, else
 * write ONLY what changed - the meta register when the name or a toggle
 * moved, one member register per chat whose place moved. A chat the draft
 * still holds keeps its register untouched.
 *
 * A created folder ALSO writes the order register: [listedFolderIds] (the
 * folders as the fold lists them now) with the new id appended - see
 * [appendToOrder]. Ignored when editing an existing folder.
 */
public fun saveChanges(
    folderId: ByteArray,
    original: ChatFolder?,
    draft: FolderDraft,
    listedFolderIds: List<ByteArray>,
    stamp: StampSource,
): FolderChangeSet {
    val name = draft.name.trim()
    val metas = if (
        original == null ||
        name != original.name ||
        draft.includeKinds != original.includeKinds ||
        draft.excludeFlags != original.excludeFlags
    ) {
        listOf(
            FolderMetaRegister(
                hlc = stamp(),
                folderId = folderId,
                name = name,
                includeKinds = draft.includeKinds,
                excludeFlags = draft.excludeFlags,
            ),
        )
    } else {
        emptyList()
    }

    val before = HashMap<String, Pair<FolderConversationRef, FolderMemberState>>()
    original?.included?.forEach { before[refKey(it)] = it to FolderMemberState.INCLUDED }
    original?.excluded?.forEach { before[refKey(it)] = it to FolderMemberState.EXCLUDED }
    val after = LinkedHashMap<String, Pair<FolderConversationRef, FolderMemberState>>()
    draft.included.forEach { after[refKey(it)] = it to FolderMemberState.INCLUDED }
    // Excluded by hand beats included by hand (UX spec 3.2).
    draft.excluded.forEach { after[refKey(it)] = it to FolderMemberState.EXCLUDED }

    val members = ArrayList<FolderMemberRegister>()
    for ((key, value) in after) {
        val (ref, state) = value
        if (before[key]?.second != state) {
            members += FolderMemberRegister(stamp(), folderId, ref, state)
        }
    }
    for ((key, value) in before) {
        if (key !in after) {
            // NONE is the element tombstone: a device that has not seen the
            // removal cannot bring the chat back by re-sending its older state.
            members += FolderMemberRegister(stamp(), folderId, value.first, FolderMemberState.NONE)
        }
    }
    val order = if (original == null) appendToOrder(listedFolderIds, folderId, stamp) else null
    return changeSet(metas = metas, members = members, order = order)
}

/**
 * The order register a folder CREATION carries in the same change set: the
 * folders as currently listed ([listedFolderIds], the fold's `folders()`
 * order) with [newFolderId] appended at the end, never listed twice.
 *
 * Without it the fold places a folder the order does not name by the stamp
 * of its CURRENT meta register, so every later rename would move the folder
 * to the end of the folder bar. Writing the listed order is safe here: it is
 * exactly what this device shows, so it rewrites nothing the person sees.
 */
public fun appendToOrder(
    listedFolderIds: List<ByteArray>,
    newFolderId: ByteArray,
    stamp: StampSource,
): FolderOrderRegister {
    val ids = listedFolderIds.filterNot { it.contentEquals(newFolderId) } + newFolderId.copyOf()
    return FolderOrderRegister(stamp(), ids)
}

/** Delete a folder. Final on every device (ADR-0058 section 3). */
public fun deleteChanges(folderId: ByteArray, stamp: StampSource): FolderChangeSet =
    changeSet(removed = listOf(FolderRemovedRegister(stamp(), folderId)))

/** The person's folder order, as one value. */
public fun orderChanges(folderIds: List<ByteArray>, stamp: StampSource): FolderChangeSet =
    changeSet(order = FolderOrderRegister(stamp(), folderIds))

/** One chat's place in one folder. */
public fun memberChange(
    folderId: ByteArray,
    ref: FolderConversationRef,
    state: FolderMemberState,
    stamp: StampSource,
): FolderChangeSet = changeSet(members = listOf(FolderMemberRegister(stamp(), folderId, ref, state)))

/**
 * What tapping a folder in "Add to folder" does to one chat (UX spec 5), or
 * `null` when nothing would change.
 *
 *  - Not in the folder: added by hand; if it was excluded by hand the
 *    exclusion is removed instead (and when the folder's kind toggles do not
 *    reach it, the removal is an addition by hand, so the tap always puts it
 *    in).
 *  - In by hand: removed.
 *  - In by a kind toggle: excluded by hand.
 */
public fun membershipToggle(
    folder: ChatFolder,
    row: ConversationListRow,
    inFolder: Boolean,
): FolderMemberState? {
    val handIn = folder.included.any { refNames(it, row) }
    val handOut = folder.excluded.any { refNames(it, row) }
    return if (inFolder) {
        if (handIn) FolderMemberState.NONE else FolderMemberState.EXCLUDED
    } else {
        when {
            handOut && kindIncluded(folder.includeKinds, row.kind) -> FolderMemberState.NONE
            handIn -> null
            else -> FolderMemberState.INCLUDED
        }
    }
}

/**
 * Whether a toggle that ADDS a hand entry is refused by the 200 limit. A
 * removal (`NONE`) is never refused, and neither is turning one kind of hand
 * entry into the other when the other list has room.
 */
public fun membershipToggleBlocked(folder: ChatFolder, row: ConversationListRow, state: FolderMemberState): Boolean {
    val handIn = folder.included.any { refNames(it, row) }
    val handOut = folder.excluded.any { refNames(it, row) }
    return when (state) {
        FolderMemberState.NONE -> false
        FolderMemberState.INCLUDED -> !handIn && folder.included.size >= FolderLimits.MAX_HAND
        FolderMemberState.EXCLUDED -> !handOut && folder.excluded.size >= FolderLimits.MAX_HAND
    }
}

/** Whether the folder's kind toggles reach a row of [kind]. */
public fun kindIncluded(includeKinds: UInt, kind: ConversationRowKind): Boolean = when (kind) {
    ConversationRowKind.CHAT -> includeKinds and IncludeKind.ONE_TO_ONE != 0u
    ConversationRowKind.GROUP -> includeKinds and IncludeKind.GROUPS != 0u
    ConversationRowKind.CHANNEL -> includeKinds and IncludeKind.CHANNELS != 0u
}

/**
 * Pin or unpin [ref] inside [folder]: the folder's pin list is one value, so
 * the whole new list is the register. A new pin goes to the top. `null` when
 * pinning would pass the limit or nothing changes.
 */
public fun pinChanges(
    folder: ChatFolder,
    ref: FolderConversationRef,
    pin: Boolean,
    stamp: StampSource,
): FolderChangeSet? {
    val key = refKey(ref)
    val without = folder.pinned.filter { refKey(it) != key }
    val next = if (pin) {
        if (without.size != folder.pinned.size) return null
        if (without.size >= FolderLimits.MAX_PINS) return null
        listOf(ref) + without
    } else {
        if (without.size == folder.pinned.size) return null
        without
    }
    return changeSet(pins = listOf(FolderPinsRegister(stamp(), folder.id, next)))
}

// ---------------------------------------------------------------------------
// Suggested folders (UX spec 4)
// ---------------------------------------------------------------------------

/**
 * One suggestion. Its name and description render in the interface language
 * (ADR-0061); adding one stores the name in the language of that moment as
 * the folder's own name, which from then on is the person's text.
 */
public data class FolderSuggestion(
    val name: UiText,
    val description: UiText,
    val includeKinds: UInt,
    val excludeFlags: UInt,
)

/**
 * The four suggestions, exactly as UX spec section 4 lists them: Unread is
 * every kind excluding Read only; the other three exclude nothing.
 */
public val FOLDER_SUGGESTIONS: List<FolderSuggestion> = listOf(
    FolderSuggestion(
        UiText("folders.suggested.unread.name"),
        UiText("folders.suggested.unread.description"),
        IncludeKind.ONE_TO_ONE or IncludeKind.GROUPS or IncludeKind.CHANNELS,
        ExcludeFlag.READ,
    ),
    FolderSuggestion(
        UiText("folders.suggested.personal.name"),
        UiText("folders.suggested.personal.description"),
        IncludeKind.ONE_TO_ONE,
        0u,
    ),
    FolderSuggestion(
        UiText("folders.suggested.groups.name"),
        UiText("folders.suggested.groups.description"),
        IncludeKind.GROUPS,
        0u,
    ),
    FolderSuggestion(
        UiText("folders.suggested.channels.name"),
        UiText("folders.suggested.channels.description"),
        IncludeKind.CHANNELS,
        0u,
    ),
)

/** Suggestions the person does not already have, compared by rule: same
 *  toggles and nothing added or excluded by hand. */
public fun suggestionsNotHeld(folders: List<ChatFolder>): List<FolderSuggestion> =
    FOLDER_SUGGESTIONS.filter { s ->
        folders.none { f ->
            f.includeKinds == s.includeKinds &&
                f.excludeFlags == s.excludeFlags &&
                f.included.isEmpty() &&
                f.excluded.isEmpty()
        }
    }

/** Create a suggestion as is, appended to the folder order ([appendToOrder]). */
public fun suggestionChanges(
    folderId: ByteArray,
    s: FolderSuggestion,
    listedFolderIds: List<ByteArray>,
    stamp: StampSource,
): FolderChangeSet =
    changeSet(
        metas = listOf(FolderMetaRegister(stamp(), folderId, s.name.render(), s.includeKinds, s.excludeFlags)),
        order = appendToOrder(listedFolderIds, folderId, stamp),
    )

// ---------------------------------------------------------------------------
// Hex
// ---------------------------------------------------------------------------

internal fun hexOf(bytes: ByteArray): String {
    val sb = StringBuilder(bytes.size * 2)
    for (b in bytes) {
        val v = b.toInt() and 0xFF
        sb.append(HEX[v ushr 4])
        sb.append(HEX[v and 0x0F])
    }
    return sb.toString()
}

internal fun bytesOfHex(hex: String): ByteArray? {
    if (hex.isEmpty() || hex.length % 2 != 0) return null
    val out = ByteArray(hex.length / 2)
    var i = 0
    while (i < hex.length) {
        val hi = Character.digit(hex[i], 16)
        val lo = Character.digit(hex[i + 1], 16)
        if (hi < 0 || lo < 0) return null
        out[i / 2] = ((hi shl 4) or lo).toByte()
        i += 2
    }
    return out
}

private const val HEX = "0123456789abcdef"

// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.ConversationKind as SharedConversationKind
import uniffi.lakko_ffi_uniffi.ConversationRow as SharedConversationRow
import uniffi.lakko_ffi_uniffi.FolderConversationKind
import uniffi.lakko_ffi_uniffi.FolderConversationRef
import uniffi.lakko_ffi_uniffi.FolderMemberState
import uniffi.lakko_ffi_uniffi.FolderRule

/**
 * docs/ux/chat-folders.md / ADR-0058 — the client half of chat folders.
 *
 * Which chats a folder holds and their order are `folder_matches` and
 * `order_conversations_in_folder` in `lakko-ui-rules`, tested there once for
 * three clients; the host JVM has no native library, so a stand-in rule is
 * passed where the real binding would be. What is tested here is what this
 * side owns: rows mapped to references and back, the rule's inputs, and each
 * edit turned into a change set carrying ONLY the registers it changed.
 */
class ChatFoldersTest {

    private val groupA = "aa".repeat(32)
    private val groupB = "bb".repeat(32)
    private val chat1 = "11".repeat(32)
    private val channel1 = "cc".repeat(16)

    private fun row(
        id: String,
        kind: ConversationRowKind,
        unread: Int = 0,
        muted: Boolean = false,
        archived: Boolean = false,
        override: Boolean = false,
        pinned: Boolean = false,
        ended: Boolean = false,
    ) = ConversationListRow(
        id = id,
        kind = kind,
        title = id.take(4),
        preview = "",
        time = null,
        lastActivityMs = null,
        unread = unread,
        pinned = pinned,
        muted = muted,
        archived = archived,
        unreadOverride = override,
        membershipEnded = ended,
    )

    private fun ref(id: String, kind: FolderConversationKind) =
        FolderConversationRef(kind = kind, id = bytesOfHex(id)!!)

    private fun folder(
        include: UInt = 0u,
        exclude: UInt = 0u,
        included: List<FolderConversationRef> = emptyList(),
        excluded: List<FolderConversationRef> = emptyList(),
        pinned: List<FolderConversationRef> = emptyList(),
        name: String = "Work",
    ) = ChatFolder(
        id = ByteArray(16) { 7 },
        name = name,
        includeKinds = include,
        excludeFlags = exclude,
        included = included,
        excluded = excluded,
        pinned = pinned,
    )

    /** UX spec 3.2, written out for the host JVM in place of `folder_matches`. */
    private val specMatcher: FolderMatcher = { rule, r ->
        val kindBit = when (r.kind) {
            SharedConversationKind.CHAT -> 1u
            SharedConversationKind.GROUP -> 2u
            SharedConversationKind.CHANNEL -> 4u
        }
        val included = (rule.includeKinds and kindBit) != 0u || r.id in rule.included
        val excluded = r.id in rule.excluded ||
            (rule.excludeFlags and 1u != 0u && r.muted) ||
            (rule.excludeFlags and 2u != 0u && !r.hasUnread) ||
            (rule.excludeFlags and 4u != 0u && r.archived)
        included && !excluded
    }

    private fun counter(): StampSource {
        var n = 100uL
        return { n++ }
    }

    // ---- rows <-> references ---------------------------------------------

    @Test
    fun every_kind_maps_to_a_reference_and_back_to_its_row_id() {
        val cases = listOf(
            row(chat1, ConversationRowKind.CHAT) to FolderConversationKind.ONE_TO_ONE,
            row(groupA, ConversationRowKind.GROUP) to FolderConversationKind.GROUP,
            row(channel1, ConversationRowKind.CHANNEL) to FolderConversationKind.CHANNEL,
        )
        for ((r, kind) in cases) {
            val ref = refOf(r)!!
            assertEquals(kind, ref.kind)
            assertEquals(r.id, rowIdOf(ref))
        }
        // A channel id is 16 bytes and a group id 32: the row ids of the two
        // kinds cannot collide.
        assertEquals(16, refOf(row(channel1, ConversationRowKind.CHANNEL))!!.id.size)
        assertEquals(32, refOf(row(groupA, ConversationRowKind.GROUP))!!.id.size)
    }

    @Test
    fun a_row_whose_id_is_not_hex_has_no_reference() {
        assertNull(refOf(row("not-hex", ConversationRowKind.GROUP)))
    }

    @Test
    fun the_rule_reads_muted_and_has_unread_from_the_real_flags() {
        val shared: SharedConversationRow = row(groupA, ConversationRowKind.GROUP, muted = true, override = true)
            .toFolderRow()
        assertTrue(shared.muted)
        assertTrue("a manual mark-as-unread counts as unread", shared.hasUnread)
        assertTrue(row(groupA, ConversationRowKind.GROUP, unread = 2).toFolderRow().hasUnread)
        assertFalse(row(groupA, ConversationRowKind.GROUP).toFolderRow().hasUnread)
    }

    @Test
    fun the_folder_rule_names_hand_entries_by_row_id() {
        val f = folder(
            include = IncludeKind.CHANNELS,
            exclude = ExcludeFlag.READ,
            included = listOf(ref(groupA, FolderConversationKind.GROUP)),
            excluded = listOf(ref(channel1, FolderConversationKind.CHANNEL)),
        )
        val rule: FolderRule = f.rule
        assertEquals(IncludeKind.CHANNELS, rule.includeKinds)
        assertEquals(ExcludeFlag.READ, rule.excludeFlags)
        assertEquals(listOf(groupA), rule.included)
        assertEquals(listOf(channel1), rule.excluded)
    }

    // ---- what a folder shows ---------------------------------------------

    @Test
    fun a_folder_uses_its_own_pins_not_the_global_pin() {
        val f = folder(
            include = IncludeKind.GROUPS,
            pinned = listOf(ref(groupB, FolderConversationKind.GROUP)),
        )
        val rows = listOf(
            row(groupA, ConversationRowKind.GROUP, pinned = true),
            row(groupB, ConversationRowKind.GROUP),
        )
        var pinsSeen: List<String>? = null
        val shown = folderRows(
            f,
            rows,
            matches = specMatcher,
            order = { list, pins ->
                pinsSeen = pins
                assertEquals(listOf(groupA, groupB), list.map { it.id })
                listOf(1u, 0u)
            },
        )
        assertEquals(listOf(groupB), pinsSeen)
        assertEquals(listOf(groupB, groupA), shown.map { it.id })
        assertTrue("the folder pin is drawn", shown[0].pinned)
        assertFalse("the All-chats pin does not carry into a folder", shown[1].pinned)
    }

    @Test
    fun a_conversation_the_person_left_is_in_no_folder() {
        val f = folder(include = IncludeKind.GROUPS)
        val rows = listOf(row(groupA, ConversationRowKind.GROUP, ended = true))
        assertTrue(rowsMatching(f.rule, rows, specMatcher).isEmpty())
    }

    @Test
    fun the_tab_count_is_unread_conversations_without_the_muted_ones() {
        val f = folder(include = IncludeKind.GROUPS or IncludeKind.CHANNELS)
        val rows = listOf(
            row(groupA, ConversationRowKind.GROUP, unread = 5),
            row(groupB, ConversationRowKind.GROUP, unread = 1, muted = true),
            row(chat1, ConversationRowKind.CHAT, unread = 3),
            row(channel1, ConversationRowKind.CHANNEL),
        )
        assertEquals(1, folderUnreadCount(f, rows, specMatcher))
    }

    @Test
    fun the_saved_tab_falls_back_to_all_chats_when_its_folder_is_gone() {
        val f = folder()
        assertEquals(0, selectedTabIndex(null, listOf(f)))
        assertEquals(1, selectedTabIndex(f.idHex, listOf(f)))
        assertEquals(0, selectedTabIndex("ff".repeat(16), listOf(f)))
        assertEquals(0, selectedTabIndex(f.idHex, emptyList()))
    }

    // ---- edits -> change sets ---------------------------------------------

    @Test
    fun creating_a_folder_writes_its_meta_its_hand_entries_and_the_order_with_it_appended() {
        val existing = listOf(ByteArray(16) { 1 }, ByteArray(16) { 2 })
        val newId = ByteArray(16) { 3 }
        val draft = FolderDraft.empty(ref(groupA, FolderConversationKind.GROUP)).copy(name = " Work ")
        val cs = saveChanges(newId, null, draft, existing, counter())
        assertEquals(1, cs.metas.size)
        assertEquals("Work", cs.metas[0].name)
        assertEquals(ExcludeFlag.ARCHIVED, cs.metas[0].excludeFlags)
        assertEquals(1, cs.members.size)
        assertEquals(FolderMemberState.INCLUDED, cs.members[0].state)
        assertEquals(listOf("01".repeat(16), "02".repeat(16), "03".repeat(16)), cs.order!!.folderIds.map(::hexOf))
        assertTrue(cs.pins.isEmpty() && cs.removed.isEmpty())
    }

    @Test
    fun editing_an_existing_folder_never_writes_the_order() {
        val f = folder()
        val cs = saveChanges(f.id, f, FolderDraft.of(f).copy(name = "Job"), listOf(ByteArray(16) { 1 }, f.id), counter())
        assertNull(cs.order)
    }

    @Test
    fun append_to_order_appends_the_new_folder_at_the_end() {
        val listed = listOf(ByteArray(16) { 1 }, ByteArray(16) { 2 })
        val reg = appendToOrder(listed, ByteArray(16) { 3 }, counter())
        assertEquals(listOf("01".repeat(16), "02".repeat(16), "03".repeat(16)), reg.folderIds.map(::hexOf))
    }

    @Test
    fun append_to_order_on_an_empty_list_is_just_the_new_folder() {
        val reg = appendToOrder(emptyList(), ByteArray(16) { 3 }, counter())
        assertEquals(listOf("03".repeat(16)), reg.folderIds.map(::hexOf))
    }

    @Test
    fun append_to_order_never_lists_the_new_folder_twice() {
        val listed = listOf(ByteArray(16) { 1 }, ByteArray(16) { 2 })
        val reg = appendToOrder(listed, ByteArray(16) { 2 }, counter())
        assertEquals(listOf("01".repeat(16), "02".repeat(16)), reg.folderIds.map(::hexOf))
    }

    @Test
    fun adding_a_suggestion_writes_its_meta_and_the_order_with_it_appended() {
        val newId = ByteArray(16) { 9 }
        val cs = suggestionChanges(newId, FOLDER_SUGGESTIONS[1], listOf(ByteArray(16) { 1 }), counter())
        assertEquals(1, cs.metas.size)
        assertEquals("Personal", cs.metas[0].name)
        assertEquals(listOf("01".repeat(16), "09".repeat(16)), cs.order!!.folderIds.map(::hexOf))
        assertTrue(cs.metas[0].hlc != cs.order!!.hlc)
    }

    @Test
    fun saving_an_unchanged_folder_writes_nothing() {
        val f = folder(include = IncludeKind.GROUPS, included = listOf(ref(chat1, FolderConversationKind.ONE_TO_ONE)))
        assertTrue(saveChanges(f.id, f, FolderDraft.of(f), listOf(f.id), counter()).isEmpty())
    }

    @Test
    fun a_rename_writes_the_meta_register_only() {
        val f = folder(included = listOf(ref(chat1, FolderConversationKind.ONE_TO_ONE)))
        val cs = saveChanges(f.id, f, FolderDraft.of(f).copy(name = "Job"), listOf(f.id), counter())
        assertEquals(1, cs.metas.size)
        assertTrue(cs.members.isEmpty())
    }

    @Test
    fun a_membership_edit_writes_one_register_per_moved_chat_and_keeps_the_rest() {
        val unresolved = ref("dd".repeat(32), FolderConversationKind.GROUP)
        val f = folder(
            include = IncludeKind.GROUPS,
            included = listOf(ref(chat1, FolderConversationKind.ONE_TO_ONE), unresolved),
            excluded = listOf(ref(groupB, FolderConversationKind.GROUP)),
        )
        // Remove chat1, add groupA by hand, move groupB from excluded to
        // included. The chat this device cannot resolve stays untouched.
        val draft = FolderDraft.of(f).copy(
            included = listOf(unresolved, ref(groupA, FolderConversationKind.GROUP), ref(groupB, FolderConversationKind.GROUP)),
            excluded = emptyList(),
        )
        val cs = saveChanges(f.id, f, draft, listOf(f.id), counter())
        assertTrue(cs.metas.isEmpty())
        val byId = cs.members.associate { rowIdOf(it.conversation) to it.state }
        assertEquals(
            mapOf(
                chat1 to FolderMemberState.NONE,
                groupA to FolderMemberState.INCLUDED,
                groupB to FolderMemberState.INCLUDED,
            ),
            byId,
        )
        assertEquals(
            "every register carries its own stamp",
            cs.members.size,
            cs.members.map { it.hlc }.toSet().size,
        )
    }

    @Test
    fun excluded_by_hand_beats_included_by_hand_in_one_draft() {
        val a = ref(groupA, FolderConversationKind.GROUP)
        val draft = FolderDraft.empty().copy(name = "X", includeKinds = IncludeKind.GROUPS, included = listOf(a), excluded = listOf(a))
        val cs = saveChanges(ByteArray(16), null, draft, emptyList(), counter())
        assertEquals(listOf(FolderMemberState.EXCLUDED), cs.members.map { it.state })
    }

    @Test
    fun delete_and_reorder_write_one_register_each() {
        val del = deleteChanges(ByteArray(16), counter())
        assertEquals(1, del.removed.size)
        assertTrue(del.metas.isEmpty() && del.members.isEmpty() && del.order == null)
        val ids = listOf(ByteArray(16) { 1 }, ByteArray(16) { 2 })
        val ord = orderChanges(ids, counter())
        assertEquals(2, ord.order!!.folderIds.size)
        assertTrue(ord.metas.isEmpty() && ord.removed.isEmpty())
    }

    // ---- Add to folder (UX spec 5) ----------------------------------------

    @Test
    fun add_to_folder_follows_the_four_rules() {
        val a = row(groupA, ConversationRowKind.GROUP)
        // Not in, not excluded: added by hand.
        assertEquals(FolderMemberState.INCLUDED, membershipToggle(folder(), a, inFolder = false))
        // In by hand: removed.
        val hand = folder(included = listOf(ref(groupA, FolderConversationKind.GROUP)))
        assertEquals(FolderMemberState.NONE, membershipToggle(hand, a, inFolder = true))
        // In by a kind toggle: excluded by hand.
        assertEquals(
            FolderMemberState.EXCLUDED,
            membershipToggle(folder(include = IncludeKind.GROUPS), a, inFolder = true),
        )
        // Excluded by hand, the kind reaches it: the exclusion is removed.
        val out = folder(include = IncludeKind.GROUPS, excluded = listOf(ref(groupA, FolderConversationKind.GROUP)))
        assertEquals(FolderMemberState.NONE, membershipToggle(out, a, inFolder = false))
        // Excluded by hand, no kind reaches it: added by hand (replacing the
        // exclusion in the same register).
        val outNoKind = folder(excluded = listOf(ref(groupA, FolderConversationKind.GROUP)))
        assertEquals(FolderMemberState.INCLUDED, membershipToggle(outNoKind, a, inFolder = false))
    }

    @Test
    fun the_hand_list_limit_blocks_additions_only() {
        // First byte 0x01 keeps every entry distinct from groupA (0xaa...).
        val many = (0 until FolderLimits.MAX_HAND).map { i ->
            FolderConversationRef(FolderConversationKind.GROUP, ByteArray(32) { j -> if (j == 0) 1 else i.toByte() })
        }
        val full = folder(included = many)
        val a = row(groupA, ConversationRowKind.GROUP)
        assertTrue(membershipToggleBlocked(full, a, FolderMemberState.INCLUDED))
        assertFalse(membershipToggleBlocked(full, a, FolderMemberState.NONE))
        assertFalse(membershipToggleBlocked(full, a, FolderMemberState.EXCLUDED))
    }

    // ---- pins inside a folder ---------------------------------------------

    @Test
    fun a_new_folder_pin_goes_first_and_the_tenth_is_the_last() {
        val a = ref(groupA, FolderConversationKind.GROUP)
        val b = ref(groupB, FolderConversationKind.GROUP)
        val one = pinChanges(folder(pinned = listOf(a)), b, pin = true, stamp = counter())!!
        assertEquals(listOf(groupB, groupA), one.pins[0].pinned.map(::rowIdOf))
        val unpin = pinChanges(folder(pinned = listOf(a, b)), a, pin = false, stamp = counter())!!
        assertEquals(listOf(groupB), unpin.pins[0].pinned.map(::rowIdOf))
        val ten = (0 until FolderLimits.MAX_PINS).map { i ->
            FolderConversationRef(FolderConversationKind.GROUP, ByteArray(32) { i.toByte() })
        }
        assertNull("an eleventh pin is refused", pinChanges(folder(pinned = ten), a, pin = true, stamp = counter()))
        assertNull("pinning what is pinned changes nothing", pinChanges(folder(pinned = listOf(a)), a, pin = true, stamp = counter()))
    }

    // ---- suggestions -------------------------------------------------------

    @Test
    fun the_suggestions_are_exactly_the_ux_spec_rules() {
        val byName = FOLDER_SUGGESTIONS.associateBy { it.name }
        assertEquals(listOf("Unread", "Personal", "Groups", "Channels"), FOLDER_SUGGESTIONS.map { it.name })
        assertEquals(IncludeKind.ONE_TO_ONE or IncludeKind.GROUPS or IncludeKind.CHANNELS, byName["Unread"]!!.includeKinds)
        assertEquals("Unread excludes Read only", ExcludeFlag.READ, byName["Unread"]!!.excludeFlags)
        assertEquals(IncludeKind.ONE_TO_ONE, byName["Personal"]!!.includeKinds)
        assertEquals(IncludeKind.GROUPS, byName["Groups"]!!.includeKinds)
        assertEquals(IncludeKind.CHANNELS, byName["Channels"]!!.includeKinds)
        for (name in listOf("Personal", "Groups", "Channels")) {
            assertEquals("$name excludes nothing", 0u, byName[name]!!.excludeFlags)
        }
    }

    @Test
    fun a_folder_with_an_extra_exclude_toggle_is_a_different_rule() {
        // An editor-made one-to-one folder keeps Archived excluded by default,
        // which is not the Personal suggestion's rule.
        val editorMade = folder(include = IncludeKind.ONE_TO_ONE, exclude = ExcludeFlag.ARCHIVED)
        assertTrue("Personal" in suggestionsNotHeld(listOf(editorMade)).map { it.name })
        val unreadPlusArchived = folder(
            include = IncludeKind.ONE_TO_ONE or IncludeKind.GROUPS or IncludeKind.CHANNELS,
            exclude = ExcludeFlag.READ or ExcludeFlag.ARCHIVED,
        )
        assertTrue("Unread" in suggestionsNotHeld(listOf(unreadPlusArchived)).map { it.name })
    }

    @Test
    fun a_suggestion_disappears_once_a_folder_has_its_rule() {
        assertEquals(4, suggestionsNotHeld(emptyList()).size)
        val personal = FOLDER_SUGGESTIONS.first { it.name == "Personal" }
        // Compared by rule, not by name.
        val held = folder(include = personal.includeKinds, exclude = personal.excludeFlags, name = "Mine")
        val left = suggestionsNotHeld(listOf(held)).map { it.name }
        assertFalse("Personal" in left)
        assertEquals(3, left.size)
        // The same toggles plus a hand entry is a different rule.
        val edited = held.copy(included = listOf(ref(groupA, FolderConversationKind.GROUP)))
        assertTrue("Personal" in suggestionsNotHeld(listOf(edited)).map { it.name })
    }

    @Test
    fun the_draft_needs_a_name_and_something_included() {
        assertFalse(FolderDraft.empty().canSave)
        assertFalse(FolderDraft.empty().copy(name = "X").canSave)
        assertTrue(FolderDraft.empty().copy(name = "X", includeKinds = IncludeKind.GROUPS).canSave)
        assertTrue(FolderDraft.empty(ref(groupA, FolderConversationKind.GROUP)).copy(name = "X").canSave)
        assertFalse(FolderDraft.empty().copy(name = "  ", includeKinds = IncludeKind.GROUPS).canSave)
    }
}

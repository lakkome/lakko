// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.OutgoingOwner
import uniffi.lakko_ffi_uniffi.ProposalKind
import uniffi.lakko_ffi_uniffi.ProposalRecord

/**
 * ADR-0045 — ownership transfer, the owner's way out
 * (`docs/ux/proposal-based-membership.md` §6.1).
 *
 * Mirrors `apps/web/src/chat/ownership-transfer.test.ts`. Every rule §6.1 states
 * lives in `OwnershipTransfer.kt` rather than in the composable precisely so it
 * can be run here — the host suite renders nothing and loads no JNI library.
 *
 * ## Which of these discriminate (MEASURED — each probe was RUN and reverted)
 *
 * The failures are quoted verbatim in the report accompanying this change. The
 * shape of each probe is: break the one rule the test names, run, observe red,
 * revert. The tests marked BOUNDARY GUARD are the exception — their job is to
 * fail when the §6.1 copy drifts rather than when a rule changes, so the probe
 * for those is a wording edit.
 */
class OwnershipTransferTest {

    private companion object {
        const val ACCOUNT = "acct-a"
        const val GROUP = "grp-1"

        const val OWNER_KEY = "b3duZXItLS0tLS0tLS0tLS0tLS0tLS0tLS0="
        const val ALICE_KEY = "YWxpY2UtLS0tLS0tLS0tLS0tLS0tLS0tLS0="
        const val BOB_KEY = "Ym9iLS0tLS0tLS0tLS0tLS0tLS0tLS0tLS0="
    }

    private fun roster(): List<RosterEntryView> = listOf(
        RosterEntryView(leafIndex = 0u, signatureKeyB64 = OWNER_KEY, label = "Owner"),
        RosterEntryView(leafIndex = 1u, signatureKeyB64 = ALICE_KEY, label = "Alice"),
        RosterEntryView(leafIndex = 2u, signatureKeyB64 = BOB_KEY, label = "b0b1c2d3"),
    )

    private fun exitInput(
        usesProposalPath: Boolean = true,
        isOwner: Boolean = false,
        ownLeafKnown: Boolean = true,
        candidateCount: Int = 2,
        waitingRoleChange: WaitingRoleChange? = null,
        ownRemovalPending: Boolean = false,
    ) = ExitBlockInput(
        usesProposalPath = usesProposalPath,
        isOwner = isOwner,
        ownLeafKnown = ownLeafKnown,
        candidateCount = candidateCount,
        waitingRoleChange = waitingRoleChange,
        ownRemovalPending = ownRemovalPending,
    )

    // -----------------------------------------------------------------------
    // §6.1.1 — the exit block, two role-disjoint controls
    // -----------------------------------------------------------------------

    @Test
    fun `the owner sees Transfer live and Leave disabled with its reason`() {
        val block = exitBlock(exitInput(isOwner = true))
        assertTrue("expected the owner arm, got $block", block is ExitBlock.Owner)
        block as ExitBlock.Owner
        assertTrue("the owner's Transfer ownership must be live", block.transfer.enabled)
        assertNull(block.transfer.disabledReason)
        // The reversal §6.1.1 argues for: present and disabled, never hidden. An
        // owner looking for the exit must not find nothing at all about leaving.
        assertFalse("the owner's Leave group must be present and DISABLED", block.leave.enabled)
        assertEquals(LEAVE_BLOCKED_BY_OWNERSHIP, block.leave.disabledReason)
        assertEquals(
            "Transfer ownership first — a group cannot be left without an owner.",
            block.leave.disabledReason,
        )
    }

    @Test
    fun `a non-owner sees Leave alone, live, and NO transfer control`() {
        val block = exitBlock(exitInput(isOwner = false))
        // `ExitBlock.Member` carries no `transfer` field AT ALL — a non-owner's
        // transfer is refused by the core and by every peer, so the control
        // would never work and nothing a member could do would make it work.
        // That is the case where "a control that always refuses is worse than no
        // control" still holds. The type is the assertion: there is no field to
        // read, so no composable can render one.
        assertTrue("expected the member arm, got $block", block is ExitBlock.Member)
        block as ExitBlock.Member
        assertTrue("a non-owner's Leave group must be live", block.leave.enabled)
        assertNull(block.leave.disabledReason)
    }

    @Test
    fun `a group with no role table shows no exit block at all`() {
        // No role table means no owner, so there is nobody to transfer to and no
        // self-remove route in the core either.
        assertEquals(ExitBlock.Hidden, exitBlock(exitInput(usesProposalPath = false)))
        assertEquals(
            ExitBlock.Hidden,
            exitBlock(exitInput(usesProposalPath = false, isOwner = true)),
        )
    }

    @Test
    fun `an unresolvable own leaf shows no exit block`() {
        assertEquals(ExitBlock.Hidden, exitBlock(exitInput(ownLeafKnown = false)))
    }

    @Test
    fun `Leave group does not turn live one instant early`() {
        // §6.1.7: it stays disabled for the transferor until the role table
        // actually names someone else — read off the role table, NEVER off the
        // local record of having asked. An owner is still the owner while their
        // transfer is pending, and a `Leave group` that went live on publication
        // would offer an action the core refuses and the cryptography does not
        // support. So the gate is `isOwner`, and a pending own transfer changes
        // nothing about it.
        val pending = exitBlock(
            exitInput(
                isOwner = true,
                waitingRoleChange = WaitingRoleChange(ownTransfer = true, successorLabel = "Alice"),
            ),
        )
        assertTrue("a pending transfer does not stop us being the owner", pending is ExitBlock.Owner)
        assertFalse(
            "Leave group must stay disabled until the ROLE TABLE names someone else",
            (pending as ExitBlock.Owner).leave.enabled,
        )

        // …and the moment the table names someone else, the same control in the
        // same slot is simply live. Nothing has to announce the transition.
        val landed = exitBlock(
            exitInput(
                isOwner = false,
                waitingRoleChange = WaitingRoleChange(ownTransfer = true, successorLabel = "Alice"),
            ),
        )
        assertTrue("once the table names someone else we are an ordinary member", landed is ExitBlock.Member)
        assertTrue(
            "and then the same control in the same slot is simply live",
            (landed as ExitBlock.Member).leave.enabled,
        )
    }

    @Test
    fun `a member with their own leave pending keeps Leave visible but disabled`() {
        val block = exitBlock(exitInput(ownRemovalPending = true))
        assertTrue(block is ExitBlock.Member)
        block as ExitBlock.Member
        assertFalse("our own pending leave disables the control", block.leave.enabled)
        // §5.2's existing annotation, reused — not new copy.
        assertEquals("Waiting to leave.", block.leave.disabledReason)
    }

    // -----------------------------------------------------------------------
    // §6.1.3 / §6.1.6 / §6.1.8 — why Transfer ownership is disabled
    // -----------------------------------------------------------------------

    @Test
    fun `a group with no other member disables transfer with its own terminal reason`() {
        val block = exitBlock(exitInput(isOwner = true, candidateCount = 0))
        block as ExitBlock.Owner
        assertFalse("an empty candidate set disables transfer", block.transfer.enabled)
        assertEquals(NO_TRANSFER_CANDIDATES, block.transfer.disabledReason)
        assertEquals(
            "There is no one else in this group to transfer it to.",
            block.transfer.disabledReason,
        )
    }

    @Test
    fun `a waiting role change disables transfer with the SEVENTH string`() {
        val block = exitBlock(
            exitInput(
                isOwner = true,
                waitingRoleChange = WaitingRoleChange(ownTransfer = false, successorLabel = null),
            ),
        )
        block as ExitBlock.Owner
        assertFalse("a waiting role change disables transfer", block.transfer.enabled)
        // §6.1.8: the table stays at seven strings. This is the seventh,
        // verbatim, and it is the ONE availability refusal in the table — the
        // only one that ends by telling the user to try again.
        assertEquals(ROLE_CHANGE_ALREADY_PENDING, block.transfer.disabledReason)
        assertEquals(
            "A role change is already waiting to apply. Wait for it to land, then try again.",
            block.transfer.disabledReason,
        )
    }

    @Test
    fun `our OWN waiting transfer names the successor instead`() {
        val block = exitBlock(
            exitInput(
                isOwner = true,
                waitingRoleChange = WaitingRoleChange(ownTransfer = true, successorLabel = "Alice"),
            ),
        )
        block as ExitBlock.Owner
        assertFalse("our own waiting transfer disables transfer", block.transfer.enabled)
        assertEquals("Waiting to make Alice the owner.", block.transfer.disabledReason)
    }

    @Test
    fun `our own waiting transfer whose successor no longer resolves falls back`() {
        val block = exitBlock(
            exitInput(
                isOwner = true,
                waitingRoleChange = WaitingRoleChange(ownTransfer = true, successorLabel = null),
            ),
        )
        block as ExitBlock.Owner
        assertEquals("Ownership transfer waiting to apply.", block.transfer.disabledReason)
    }

    @Test
    fun `our own PROMOTE is not our own transfer, and gets the seventh string`() {
        // `ProposalKind::RoleChange` carries no payload, so the successor key on
        // our own observation record is the only thing that tells a transfer
        // from a promote. A promote reporting itself as `Ownership transfer
        // waiting to apply.` would be a plain falsehood; the seventh string is
        // true of it. The predicate is `own && transferTo != null`, never `own`.
        val block = exitBlock(
            exitInput(
                isOwner = true,
                waitingRoleChange = WaitingRoleChange(ownTransfer = false, successorLabel = null),
            ),
        )
        block as ExitBlock.Owner
        assertEquals(ROLE_CHANGE_ALREADY_PENDING, block.transfer.disabledReason)
    }

    @Test
    fun `a PERMANENT refusal outranks the retry-inviting one`() {
        // §6 draws the distinction and it decides the precedence: an
        // authorisation refusal is a statement of fact with no invitation to
        // retry, an availability refusal ends by telling the user to try again.
        // An empty roster is permanent, so it must not be masked by a string
        // that promises a retry which can never succeed.
        val block = exitBlock(
            exitInput(
                isOwner = true,
                candidateCount = 0,
                waitingRoleChange = WaitingRoleChange(ownTransfer = false, successorLabel = null),
            ),
        )
        block as ExitBlock.Owner
        assertEquals(NO_TRANSFER_CANDIDATES, block.transfer.disabledReason)
    }

    @Test
    fun `our own request outranks even the permanent one`() {
        // UX §10's non-negotiable own-request exception: a member must always
        // see the status of a request they made.
        val block = exitBlock(
            exitInput(
                isOwner = true,
                candidateCount = 0,
                waitingRoleChange = WaitingRoleChange(ownTransfer = true, successorLabel = "Alice"),
            ),
        )
        block as ExitBlock.Owner
        assertEquals("Waiting to make Alice the owner.", block.transfer.disabledReason)
    }

    @Test
    fun `a role change INSIDE the quiet window leaves the control enabled`() {
        // §6.1.6: below the quiet window the control stays enabled and a second
        // tap is refused. `waitingRoleChange` is null for a request that has not
        // matured, so this is the caller's WAITING filter arriving here as an
        // absence. A request not yet rendered must not silently disable a
        // control with nothing to explain it.
        val block = exitBlock(exitInput(isOwner = true, waitingRoleChange = null))
        block as ExitBlock.Owner
        assertTrue(
            "a request not yet rendered must not silently disable the control",
            block.transfer.enabled,
        )
    }

    // -----------------------------------------------------------------------
    // §6.1.3 — the picker offers every member, marked not filtered
    // -----------------------------------------------------------------------

    @Test
    fun `the picker excludes the current owner and includes a non-admin member`() {
        val rows = transferCandidates(roster(), OWNER_KEY, setOf(ALICE_KEY))
        assertEquals(listOf(ALICE_KEY, BOB_KEY), rows.map { it.signatureKeyB64 })
        // The reason this is not an admins-only picker is a dead end rather than
        // a preference: in the commonest small group — owner plus one member, no
        // admins ever appointed — an admins-only picker is EMPTY and the owner
        // is stuck again, which is the exact dead end this feature exists to
        // close.
        assertFalse(
            "a non-admin member is a candidate, unmarked",
            rows.first { it.signatureKeyB64 == BOB_KEY }.isAdmin,
        )
    }

    @Test
    fun `the picker marks a current admin rather than sorting or filtering them`() {
        val rows = transferCandidates(roster(), OWNER_KEY, setOf(BOB_KEY))
        assertEquals(listOf(false, true), rows.map { it.isAdmin })
        // Roster order, unchanged. Sorting admins to the top would be a soft
        // version of the filter §6.1.3 rejects.
        assertEquals(listOf("Alice", "b0b1c2d3"), rows.map { it.label })
    }

    @Test
    fun `the picker is keyed on the signature key, never the leaf index`() {
        val rows = transferCandidates(roster(), OWNER_KEY, emptySet())
        // OpenMLS hands a vacated leaf to the next Add, so a leaf-keyed
        // selection can silently come to name someone else. The selection VALUE
        // is the key; the leaf rides along only for the row's identity in the
        // list, and the two must not be confused.
        assertEquals(listOf(ALICE_KEY, BOB_KEY), rows.map { it.signatureKeyB64 })
        assertEquals(listOf(1u, 2u), rows.map { it.leafIndex })
    }

    @Test
    fun `a one-person group yields no candidates`() {
        val rows = transferCandidates(
            listOf(RosterEntryView(leafIndex = 0u, signatureKeyB64 = OWNER_KEY, label = "Owner")),
            OWNER_KEY,
            emptySet(),
        )
        assertTrue("the owner is the only member, so there is nobody to name", rows.isEmpty())
    }

    @Test
    fun `the picker reuses the roster's own label verbatim`() {
        // The label the user reads in the picker and the label they read on the
        // roster must be produced by the same function, because whom they are
        // naming is the one thing they will cross-check.
        val rows = transferCandidates(roster(), OWNER_KEY, emptySet())
        assertEquals("b0b1c2d3", rows[1].label)
    }

    // -----------------------------------------------------------------------
    // §6.1.4 — the outgoing-owner choice
    // -----------------------------------------------------------------------

    @Test
    fun `Stay an admin is pre-selected`() {
        assertEquals(OutgoingOwner.ADMIN, PRESELECTED_OUTGOING_OWNER)
        val preselected = OUTGOING_OWNER_OPTIONS.filter { it.preSelected }
        assertEquals(1, preselected.size)
        assertEquals(OutgoingOwner.ADMIN, preselected[0].value)
        assertEquals("Stay an admin", preselected[0].label)
    }

    @Test
    fun `each outgoing-owner option carries its own consequence string`() {
        // BOUNDARY GUARD — the copy, verbatim.
        assertEquals(
            listOf(OutgoingOwner.ADMIN, OutgoingOwner.MEMBER),
            OUTGOING_OWNER_OPTIONS.map { it.value },
        )
        assertEquals(
            "You can still add and remove members. You can give this up later by asking the new owner.",
            OUTGOING_OWNER_OPTIONS[0].consequence,
        )
        assertEquals("Become a regular member", OUTGOING_OWNER_OPTIONS[1].label)
        assertEquals(
            "You give up every group power immediately. Only the new owner can give any of it back.",
            OUTGOING_OWNER_OPTIONS[1].consequence,
        )
    }

    // -----------------------------------------------------------------------
    // §6.1.5 — the step IS the confirmation
    // -----------------------------------------------------------------------

    @Test
    fun `the action is disabled until a successor is picked`() {
        val idle = transferActionLabel(null)
        assertFalse("no default successor, and no path that skips the picker", idle.enabled)
        // There is no default successor and no path to the action that skips the
        // picker.
        assertEquals("Transfer ownership", idle.label)
    }

    @Test
    fun `the action names the picked person, using the row's label verbatim`() {
        val picked = transferActionLabel(
            TransferCandidate(
                leafIndex = 2u,
                signatureKeyB64 = BOB_KEY,
                label = "b0b1c2d3",
                isAdmin = false,
            ),
        )
        assertTrue("a picked successor makes the action live", picked.enabled)
        // Not a re-formatted or re-shortened variant: that is the string the
        // user cross-checks against the row they tapped.
        assertEquals("Make b0b1c2d3 the owner", picked.label)
    }

    @Test
    fun `the irreversibility copy states three specific losses, not an adjective`() {
        // BOUNDARY GUARD — this fails when the wording drifts, which is the
        // point: all three clauses are literally true and none is rhetorical.
        assertEquals("This cannot be undone.", TRANSFER_IRREVERSIBLE_HEADLINE)
        assertEquals(
            "Only an owner can change roles. Once you transfer this group you cannot " +
                "take it back, you cannot restore your own owner role, and you cannot " +
                "change your own role again unless the new owner does it for you.",
            TRANSFER_IRREVERSIBLE_BODY,
        )
    }

    @Test
    fun `the trailing line says APPLIED, never NOW`() {
        assertEquals(
            "Once the change is applied, you can leave the group like any other member.",
            TRANSFER_UNLOCKS_LEAVE,
        )
        // Asserting the exit is open before the commit lands would be the
        // optimistic update §3 forbids.
        assertFalse(
            "\"now\" would assert the exit is open before the commit lands",
            TRANSFER_UNLOCKS_LEAVE.contains("now"),
        )
    }

    // -----------------------------------------------------------------------
    // §6.1.7 — what the transferor sees afterwards
    // -----------------------------------------------------------------------

    @Test
    fun `a swept transfer names the transferor's OWN new role`() {
        // Half the panel changes under them in one tap: an owner who chose
        // `Become a regular member` loses the add form, the invite block, every
        // `Remove` and every admin toggle at once.
        assertEquals(
            "Alice is now the group owner. You are still an admin.",
            transferOutcomeNotice(TransferOutcome.COMMITTED, "Alice", OutgoingOwner.ADMIN),
        )
        assertEquals(
            "Alice is now the group owner. You are now a regular member.",
            transferOutcomeNotice(TransferOutcome.COMMITTED, "Alice", OutgoingOwner.MEMBER),
        )
    }

    @Test
    fun `a published-but-unswept transfer names NO role`() {
        // Nothing has changed yet; appending "you are now an admin" to a request
        // that has not landed would be the optimistic-update lie in its purest
        // form.
        val notice = transferOutcomeNotice(TransferOutcome.PROPOSED, "Alice", OutgoingOwner.ADMIN)
        assertEquals(
            "Ownership transfer requested. It applies when an owner or an admin applies the change.",
            notice,
        )
        assertFalse("an unswept request must claim no role at all", notice.contains("admin."))
        assertEquals(
            notice,
            transferOutcomeNotice(TransferOutcome.PROPOSED, "Alice", OutgoingOwner.MEMBER),
        )
    }

    // -----------------------------------------------------------------------
    // §6.1.6 — the pending label resolves through the live roster
    // -----------------------------------------------------------------------

    @Test
    fun `the pending label names the successor when they still resolve`() {
        assertEquals("Waiting to make Alice the owner.", transferPendingLabel("Alice"))
    }

    @Test
    fun `the pending label falls back when the successor no longer resolves`() {
        assertEquals("Ownership transfer waiting to apply.", transferPendingLabel(null))
    }

    // -----------------------------------------------------------------------
    // §6.1.8 — the C-65 pre-check, NEW on Android
    // -----------------------------------------------------------------------

    private fun roleChange(sender: UInt? = 1u, stored: Boolean = true) =
        ProposalRecord(senderLeaf = sender, kind = ProposalKind.RoleChange, stored = stored)

    private fun addProposal(key: ByteArray = byteArrayOf(1, 2, 3)) =
        ProposalRecord(senderLeaf = 1u, kind = ProposalKind.Add(key), stored = true)

    private fun removeProposal(leaf: UInt = 4u) =
        ProposalRecord(senderLeaf = 1u, kind = ProposalKind.Remove(leaf), stored = true)

    @Test
    fun `the C-65 pre-check fires on a pending role change`() {
        assertTrue("a lone role change occupies the C-65 slot", hasPendingRoleChange(listOf(roleChange())))
        assertTrue(
            "and so does one among unrelated proposals",
            hasPendingRoleChange(listOf(addProposal(), roleChange(), removeProposal())),
        )
    }

    @Test
    fun `the C-65 pre-check does NOT fire on a pending add or remove`() {
        // NARROW on purpose. C-65 bounds one `GroupContextExtensions` proposal
        // per commit, so only another ROLE CHANGE collides. Asking the broader
        // "is anything pending" question would refuse a legitimate transfer
        // behind an unrelated add or remove — which is exactly the fragility
        // §6.1.2 gives as its reason 2 for not taking the direct path, rebuilt
        // by hand on the proposal path.
        assertFalse("nothing pending", hasPendingRoleChange(emptyList()))
        assertFalse("a pending ADD must not block a transfer", hasPendingRoleChange(listOf(addProposal())))
        assertFalse(
            "a pending REMOVE must not block a transfer",
            hasPendingRoleChange(listOf(removeProposal())),
        )
        assertFalse(
            "nor both together",
            hasPendingRoleChange(listOf(addProposal(), removeProposal())),
        )
        assertFalse(
            "nor a leave or an update",
            hasPendingRoleChange(
                listOf(
                    ProposalRecord(senderLeaf = 3u, kind = ProposalKind.Leave, stored = true),
                    ProposalRecord(senderLeaf = 3u, kind = ProposalKind.Update, stored = true),
                ),
            ),
        )
    }

    // -----------------------------------------------------------------------
    // §6.1.9 — telling the new owner, fail-open
    // -----------------------------------------------------------------------

    @Test
    fun `the new-owner notice says nothing when no previous owner was recorded`() {
        // Structurally unable to fire on a device that first observes the table
        // already naming it owner — a fresh device, a restore, a first sync. So
        // the rule is fail-open: no remembered previous owner means say nothing.
        assertNull(newOwnerNotice(null, ALICE_KEY, ALICE_KEY, "Owner"))
    }

    @Test
    fun `the new-owner notice does not fire on a FIRST observation of an unchanged table`() {
        assertNull(newOwnerNotice(ALICE_KEY, ALICE_KEY, ALICE_KEY, "Owner"))
    }

    @Test
    fun `the new-owner notice fires, naming the previous owner, when the table changed to name us`() {
        val notice = newOwnerNotice(OWNER_KEY, ALICE_KEY, ALICE_KEY, "Owner")
        assertNotNull(notice)
        // Naming them is sound rather than a guess: a role change is owner-only
        // and `owner_survives` (C-63) forbids naming a non-member, so the
        // previous owner did author it.
        assertEquals("Owner made you the owner of this group.", notice?.headline)
        assertEquals(NEW_OWNER_BODY, notice?.body)
        assertEquals(
            "You can add and remove members, change admins, and transfer ownership to someone else.",
            notice?.body,
        )
        assertEquals(NEW_OWNER_DISMISS, notice?.dismiss)
    }

    @Test
    fun `the new-owner notice omits the name when the previous owner no longer resolves`() {
        val notice = newOwnerNotice(OWNER_KEY, ALICE_KEY, ALICE_KEY, null)
        assertEquals("You are now the owner of this group.", notice?.headline)
    }

    @Test
    fun `the new-owner notice says nothing to someone who is not the owner`() {
        // Including the TRANSFEROR: their last observed owner was themselves and
        // the table now names someone else, which IS a change — but not one that
        // makes them the owner.
        assertNull(newOwnerNotice(OWNER_KEY, ALICE_KEY, BOB_KEY, "Owner"))
    }

    @Test
    fun `the new-owner notice says nothing with no role table or an unknown own key`() {
        assertNull(newOwnerNotice(OWNER_KEY, null, ALICE_KEY, "Owner"))
        assertNull(newOwnerNotice(OWNER_KEY, ALICE_KEY, null, "Owner"))
    }

    // -----------------------------------------------------------------------
    // §6.1.10 — the last-observed-owner store
    // -----------------------------------------------------------------------

    private class MemOwnerStore : LastObservedOwnerStore {
        val rows = mutableMapOf<String, String>()

        override fun load(accountIdB64: String, groupIdHex: String): String? =
            rows[SharedPrefsLastObservedOwnerStore.rowKey(accountIdB64, groupIdHex)]

        override fun save(accountIdB64: String, groupIdHex: String, ownerKeyB64: String) {
            rows[SharedPrefsLastObservedOwnerStore.rowKey(accountIdB64, groupIdHex)] = ownerKeyB64
        }

        override fun clearForAccount(accountIdB64: String) {
            val prefix = SharedPrefsLastObservedOwnerStore.accountPrefix(accountIdB64)
            rows.keys.filter { it.startsWith(prefix) }.forEach { rows.remove(it) }
        }
    }

    @Test
    fun `the owner store round-trips per (account, group)`() {
        val store = MemOwnerStore()
        assertNull(store.load(ACCOUNT, GROUP))
        store.save(ACCOUNT, GROUP, OWNER_KEY)
        assertEquals(OWNER_KEY, store.load(ACCOUNT, GROUP))
        assertNull(store.load(ACCOUNT, "grp-2"))
        assertNull(store.load("acct-b", GROUP))
    }

    @Test
    fun `the owner store records the CHANGE, so a second read sees the new owner`() {
        val store = MemOwnerStore()
        store.save(ACCOUNT, GROUP, OWNER_KEY)
        store.save(ACCOUNT, GROUP, ALICE_KEY)
        assertEquals(ALICE_KEY, store.load(ACCOUNT, GROUP))
    }

    @Test
    fun `the owner store is wiped for ONE account on recover or forgetDevice`() {
        val store = MemOwnerStore()
        store.save(ACCOUNT, GROUP, OWNER_KEY)
        store.save("acct-b", GROUP, OWNER_KEY)
        store.clearForAccount(ACCOUNT)
        assertNull(store.load(ACCOUNT, GROUP))
        assertEquals(OWNER_KEY, store.load("acct-b", GROUP))
    }

    @Test
    fun `the account wipe prefix cannot match a LONGER account id`() {
        // The whole reason the NUL separator is part of the prefix. Without it
        // `startsWith("…:acct-a")` also matches every row of `acct-a2`, so one
        // account's forgetDevice would silently wipe another's — and the two
        // ids differ by a character a base64 account id can legitimately carry.
        val prefix = SharedPrefsLastObservedOwnerStore.accountPrefix(ACCOUNT)
        assertTrue(
            "the account's own rows must match its wipe prefix",
            SharedPrefsLastObservedOwnerStore.rowKey(ACCOUNT, GROUP).startsWith(prefix),
        )
        assertFalse(
            "a LONGER account id must not be caught by it",
            SharedPrefsLastObservedOwnerStore.rowKey(ACCOUNT + "2", GROUP).startsWith(prefix),
        )
    }

    // -----------------------------------------------------------------------
    // §6.1.10 — the successor key on the observation record
    // -----------------------------------------------------------------------

    private class MemStore : PendingRequestStore {
        val rows = mutableMapOf<String, MutableMap<String, RequestObservation>>()

        override fun load(accountIdB64: String, groupIdHex: String) =
            rows["$accountIdB64/$groupIdHex"]?.toMap() ?: emptyMap()

        override fun save(
            accountIdB64: String,
            groupIdHex: String,
            observations: Map<String, RequestObservation>,
        ) {
            rows["$accountIdB64/$groupIdHex"] = observations.toMutableMap()
        }

        override fun clearForAccount(accountIdB64: String) {
            rows.keys.filter { it.startsWith("$accountIdB64/") }.forEach { rows.remove(it) }
        }
    }

    @Test
    fun `the successor key rides the observation record and reaches the fold`() {
        val store = MemStore()
        recordPendingRequestObservation(
            store, ACCOUNT, GROUP, "role-change", 100, own = true, transferToB64 = ALICE_KEY,
        )
        val folded = foldPendingRequests(
            records = listOf(roleChange()),
            observations = store.load(ACCOUNT, GROUP),
            nowMs = 10_000,
        )
        assertEquals(ALICE_KEY, folded.requests.single().transferToB64)
        assertTrue("and it is marked as ours", folded.requests.single().own)
    }

    @Test
    fun `a later observation without a successor must not erase ours`() {
        // The receive loop observes its OWN request landing back off the
        // journal, and that observation carries no successor. First-wins, or a
        // transfer would degrade to its unnamed variant the moment the poll
        // caught up with it.
        val store = MemStore()
        recordPendingRequestObservation(
            store, ACCOUNT, GROUP, "role-change", 100, own = true, transferToB64 = ALICE_KEY,
        )
        recordPendingRequestObservation(store, ACCOUNT, GROUP, "role-change", 900, own = false)
        assertEquals(ALICE_KEY, store.load(ACCOUNT, GROUP)["role-change"]?.transferToB64)
    }

    @Test
    fun `a promote records no successor, so the fold reports none`() {
        val store = MemStore()
        recordPendingRequestObservation(store, ACCOUNT, GROUP, "role-change", 100, own = true)
        val folded = foldPendingRequests(
            records = listOf(roleChange()),
            observations = store.load(ACCOUNT, GROUP),
            nowMs = 10_000,
        )
        // `own` is true and `transferToB64` is null — which is precisely the
        // pair the exit block reads as `ownTransfer = false`.
        assertTrue("a promote is still OUR request", folded.requests.single().own)
        assertNull(
            "but it names no successor, which is what makes it not a transfer",
            folded.requests.single().transferToB64,
        )
    }

    @Test
    fun `a swept transfer takes its successor key with it`() {
        // The reason the key lives on the observation record rather than in a
        // store of its own: the prune against the core's live pending set is
        // already written, so a stale successor key cannot survive to make an
        // unrelated LATER role change claim a successor nobody proposed.
        val store = MemStore()
        recordPendingRequestObservation(
            store, ACCOUNT, GROUP, "role-change", 100, own = true, transferToB64 = ALICE_KEY,
        )
        prunePendingRequestObservations(store, ACCOUNT, GROUP, liveKeys = emptyList())
        assertNull(store.load(ACCOUNT, GROUP)["role-change"])
    }

    @Test
    fun `the successor key survives the SharedPreferences row codec`() {
        val observations = mapOf(
            "role-change" to RequestObservation(atMs = 42L, own = true, transferToB64 = ALICE_KEY),
            "remove:4" to RequestObservation(atMs = 7L, own = false),
        )
        val encoded = SharedPrefsPendingRequestStore.encodeRow(observations)
        assertEquals(observations, SharedPrefsPendingRequestStore.decodeRow(encoded))
        // The empty field is the ABSENT successor, not an empty one: a
        // zero-length base64 key names nobody, and letting it through would make
        // `ownTransfer` true for a promote.
        assertNull(
            SharedPrefsPendingRequestStore.decodeRow("role-change|1|1||0|")["role-change"]
                ?.transferToB64,
        )
    }
}

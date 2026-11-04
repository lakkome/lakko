// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.l10n.Strings
import me.lakko.app.l10n.UiText
import me.lakko.app.l10n.joinList
import me.lakko.app.search.Normalize
import me.lakko.app.search.PersonReach
import me.lakko.app.search.PersonResult
import me.lakko.app.search.matchesLoosely

/*
 * The group's info as a screen, and its Add members screen
 * (`docs/ux/conversation-info.md` sections 2.4, 3 and 3.1), the pure half:
 * which screen is on top and where its back control returns, who the Add
 * members screen lists and who of them may be picked, the label of its Add
 * button, and how the picks are added one after another.
 *
 * No Android, no native calls, so the host JVM tests run all of it.
 */

/** What the conversation screen shows, from the bottom of its stack up. */
public enum class GroupInfoLayer {
    /** The conversation itself. */
    CONVERSATION,

    /** The group's info, pushed over the conversation. */
    INFO,

    /** Add members, pushed over the info. */
    ADD_MEMBERS,

    /** Transfer ownership, pushed over the info. */
    TRANSFER,
}

/**
 * Which layer is on top. The info shows only for a GROUP (a one-to-one's
 * info is the contact screen, a different route), and the two screens pushed
 * over it only while it is open.
 */
public fun groupInfoLayer(
    infoOpen: Boolean,
    isGroup: Boolean,
    addMembersOpen: Boolean,
    transferOpen: Boolean,
): GroupInfoLayer = when {
    !infoOpen || !isGroup -> GroupInfoLayer.CONVERSATION
    addMembersOpen -> GroupInfoLayer.ADD_MEMBERS
    transferOpen -> GroupInfoLayer.TRANSFER
    else -> GroupInfoLayer.INFO
}

/** Section 3.1: where the back control of [layer] returns. `null` for the
 *  conversation, whose back leaves the conversation itself. */
public fun backDestination(layer: GroupInfoLayer): GroupInfoLayer? = when (layer) {
    GroupInfoLayer.CONVERSATION -> null
    GroupInfoLayer.INFO -> GroupInfoLayer.CONVERSATION
    GroupInfoLayer.ADD_MEMBERS -> GroupInfoLayer.INFO
    GroupInfoLayer.TRANSFER -> GroupInfoLayer.INFO
}

/** One person picked on the Add members screen: what the add path takes
 *  (the account id and the person's key) and the name the failure line
 *  names them by. */
public data class AddMemberPick(
    /** The person's account key, standard base64. */
    val keyB64: String,
    /** The 16-byte account id, standard base64. */
    val accountIdB64: String,
    val name: String,
    /** For a person a search found through a grant they handed out: the
     *  grant's wire, standard base64 (secret). `null` for a contact. */
    val grantWireB64: String? = null,
    /** For a person a search found: the 97-byte mailbox binding the result
     *  carried (a person in Everyone carries one and no grant), standard
     *  base64. `null` for a contact. */
    val mailboxBindingB64: String? = null,
)

/** A contact as the Add members screen sees it. */
public data class AddCandidateContact(
    /** Account key, standard base64. */
    val keyB64: String,
    /** The captured 16-byte account id, standard base64, or `null` for a
     *  contact recorded before it was captured. */
    val accountIdB64: String?,
    val name: String,
    /** A verified username, without `@`. */
    val username: String?,
)

/** One contact row: the contact and whether it can be picked. */
public data class AddContactRow(val contact: AddCandidateContact, val pickable: Boolean)

/** One People row: the result and whether it can be picked. */
public data class AddPersonRow(val person: PersonResult, val pickable: Boolean)

/**
 * The contacts the Add members screen lists, in [contacts]' order: not this
 * account, not a member already ([memberKeysB64]), not a person an add is
 * already waiting for ([waitingKeysB64]); and, once something is typed, only
 * those whose name or username matches it loosely, from the first letter. A
 * contact without a captured account id is listed but cannot be picked
 * (`new-group.md` section 3).
 */
public fun addContactRows(
    contacts: List<AddCandidateContact>,
    memberKeysB64: Set<String>,
    waitingKeysB64: Set<String>,
    ownKeyB64: String?,
    query: String,
    normalize: Normalize,
): List<AddContactRow> {
    val typed = query.trim().removePrefix("@").trim()
    val seen = HashSet<String>()
    return contacts.mapNotNull { c ->
        if (!seen.add(c.keyB64)) return@mapNotNull null
        if (c.keyB64 == ownKeyB64 || c.keyB64 in memberKeysB64 || c.keyB64 in waitingKeysB64) {
            return@mapNotNull null
        }
        if (typed.isNotEmpty()) {
            val hit = matchesLoosely(c.name, typed, normalize) ||
                (c.username != null && matchesLoosely(c.username, typed, normalize))
            if (!hit) return@mapNotNull null
        }
        AddContactRow(c, pickable = c.accountIdB64 != null)
    }
}

/**
 * The network results the screen shows under **People**: not this account,
 * not a member, not a person an add is waiting for, not a contact already
 * listed above. A result can be picked when it carries an account id and the
 * search found a way to reach them (finding someone is the permission to
 * reach them, `who-can-find-me.md` section 12).
 */
public fun addPersonRows(
    results: List<PersonResult>,
    memberKeysB64: Set<String>,
    waitingKeysB64: Set<String>,
    listedKeysB64: Set<String>,
    ownKeyB64: String?,
): List<AddPersonRow> {
    val seen = HashSet<String>()
    return results.mapNotNull { p ->
        if (!seen.add(p.keyB64)) return@mapNotNull null
        if (p.keyB64 == ownKeyB64 || p.keyB64 in memberKeysB64 || p.keyB64 in waitingKeysB64 || p.keyB64 in listedKeysB64) {
            return@mapNotNull null
        }
        AddPersonRow(p, pickable = p.accountIdHex != null && p.reach != PersonReach.NONE)
    }
}

/** A search result as a pick, or `null` when it cannot be added. */
public fun pickOf(person: PersonResult): AddMemberPick? {
    val hex = person.accountIdHex ?: return null
    if (person.reach == PersonReach.NONE) return null
    val bytes = hexBytes(hex) ?: return null
    return AddMemberPick(
        keyB64 = person.keyB64,
        accountIdB64 = java.util.Base64.getEncoder().encodeToString(bytes),
        name = person.name,
        grantWireB64 = if (person.reach == PersonReach.GRANT) person.grantWireB64 else null,
        mailboxBindingB64 = person.mailboxBindingB64,
    )
}

/**
 * What the add of [pick] files first (ADR-0060 section 6, amended
 * 2026-10-29): the grant the search found the person through, kept as one
 * this device holds for them; else, for a person found in Everyone, the
 * mailbox binding their public record carried, recorded for this add.
 * Nothing for a contact.
 */
public sealed class FoundReach {
    /** Keep this grant wire (standard base64) as held for the person. */
    public data class Grant(val wireB64: String) : FoundReach()

    /** Record this binding (standard base64) for the add. */
    public data class OpenBinding(val bindingB64: String) : FoundReach()
}

/** [FoundReach] of [pick], or `null` for a contact. */
public fun foundReachOf(pick: AddMemberPick): FoundReach? =
    pick.grantWireB64?.let { FoundReach.Grant(it) }
        ?: pick.mailboxBindingB64?.let { FoundReach.OpenBinding(it) }

/** A contact row as a pick, or `null` when it cannot be added. */
public fun pickOf(contact: AddCandidateContact): AddMemberPick? {
    val accountId = contact.accountIdB64 ?: return null
    return AddMemberPick(keyB64 = contact.keyB64, accountIdB64 = accountId, name = contact.name)
}

/** [picks] with [pick] added, or removed when it is already there (by key). */
public fun togglePick(picks: List<AddMemberPick>, pick: AddMemberPick): List<AddMemberPick> =
    if (picks.any { it.keyB64 == pick.keyB64 }) picks.filterNot { it.keyB64 == pick.keyB64 } else picks + pick

/** The trailing button of the Add members screen: **Add**, or **Add {count}**
 *  from two picks on. Disabled with none (the caller's). */
public fun addSubmitLabel(count: Int): UiText =
    if (count >= 2) {
        UiText("chat.members.add.submit_count", counts = mapOf("count" to count.toULong()))
    } else {
        UiText("chat.members.add.submit")
    }

/**
 * Add [picks] one after another through [add], each attempted whatever the
 * one before it did. Returns the picks that could not be added, in order. A
 * cancellation (wrapped or not) stops the run and is passed on.
 */
public suspend fun addPicksInTurn(
    picks: List<AddMemberPick>,
    add: suspend (AddMemberPick) -> Unit,
): List<AddMemberPick> {
    val failed = ArrayList<AddMemberPick>()
    for (pick in picks) {
        try {
            add(pick)
        } catch (e: Throwable) {
            // A cancellation, also one the layer below wrapped in its own
            // error, stops the run: the screen went away, nothing failed.
            me.lakko.app.rethrowIfCancelled(e)
            failed += pick
        }
    }
    return failed
}

/** "Could not add {names}. Try again." for [failed], or `null` when every
 *  pick landed. */
public fun someFailedText(failed: List<AddMemberPick>, strings: Strings): UiText? {
    if (failed.isEmpty()) return null
    return UiText(
        "chat.members.add.error.some_failed",
        texts = mapOf("names" to joinList(failed.map { it.name }, strings)),
    )
}

private fun hexBytes(hex: String): ByteArray? {
    if (hex.length % 2 != 0) return null
    return try {
        ByteArray(hex.length / 2) { i -> hex.substring(i * 2, i * 2 + 2).toInt(16).toByte() }
    } catch (_: NumberFormatException) {
        null
    }
}

/** Section 2.2: whether tapping a member row opens the contact screen — a
 *  member who is one of this account's contacts, never this account's own
 *  row. */
public fun memberOpensContact(memberKeyB64: String, ownRow: Boolean, contactKeysB64: Set<String>): Boolean =
    !ownRow && memberKeyB64 in contactKeysB64

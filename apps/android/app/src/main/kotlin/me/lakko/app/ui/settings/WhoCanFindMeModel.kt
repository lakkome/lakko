// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.settings

import me.lakko.app.l10n.UiText
import me.lakko.app.reach.ReachApplyStatus
import me.lakko.app.ui.contacts.ContactRowModel
import me.lakko.app.ui.contacts.contactSections

/*
 * `docs/ux/who-can-find-me.md` — the pure half of the "Who can find me"
 * screen: what is shown for a setting, and the copy. The TRANSITIONS (what a
 * tap turns the setting into) are NOT here: they are one copy in Rust
 * (`lakko_ui_rules`, UX section 11), called by the screen over the FFI.
 */

/** The three positions (UX section 1). Mirror of `FfiVisibilityMode`. */
public enum class FindMeMode { NOBODY, SOME, EVERYONE }

/** Part 2.2's form. Mirror of `FfiGroupsForm`. */
public enum class FindMeForm { ALL, CHOSEN }

/** A group or a channel. */
public enum class FindMeScopeKind { GROUP, CHANNEL }

/** One group or channel the person is in, as the list shows it. */
public data class FindMeScope(val kind: FindMeScopeKind, val idHex: String, val name: String)

/** One contact, as the list of section 4.1 shows it. A contact with no known
 *  account id cannot be chosen, so it is not one of these. */
public data class FindMeContact(
    val accountIdHex: String,
    val keyB64: String,
    val name: String,
    val username: String?,
    val placeholder: Boolean,
)

/** The setting as the screen reads it. [chosen] holds `kind:idHex` keys,
 *  [chosenContacts] lower-case account id hex. */
public data class FindMeModel(
    val mode: FindMeMode,
    val contactsOfContacts: Boolean,
    val groupsForm: FindMeForm?,
    val chosen: Set<String>,
    val contactsForm: FindMeForm = FindMeForm.ALL,
    val chosenContacts: Set<String> = emptySet(),
) {
    /** Contacts of my contacts is on (UX section 5.1, under either form). */
    public val contactsPartOn: Boolean get() = mode == FindMeMode.SOME && contactsOfContacts
}

/** Whether `Their contacts can find me` shows on for a contact. */
public fun contactSwitchOn(model: FindMeModel, accountIdHex: String): Boolean =
    model.contactsForm == FindMeForm.ALL || accountIdHex.lowercase() in model.chosenContacts

/** The key [FindMeModel.chosen] holds for [scope]. */
public fun FindMeScope.chosenKey(): String = "${kind.name}:${idHex.lowercase()}"

/** What the screen draws for a model (UX section 3). */
public data class FindMeRows(
    /** The two parts, indented under Some people. */
    val showParts: Boolean,
    /** All / Only the ones I choose, under the second part. */
    val showForm: Boolean,
    /** N of "Only the ones you choose (N of M)". */
    val chosenCount: Int,
    /** M of "Only the ones you choose (N of M)". */
    val scopeCount: Int,
    /** The helper text under "Members of my groups and channels". */
    val membersHelper: UiText,
    /** All / Only the ones I choose, under the first part. */
    val showContactsForm: Boolean,
    /** The helper text under "Contacts of my contacts". */
    val contactsHelper: UiText,
)

/** Section 3's rows for [model] over the groups and channels [scopes]. */
public fun findMeRows(
    model: FindMeModel,
    scopes: List<FindMeScope>,
    contacts: List<FindMeContact> = emptyList(),
): FindMeRows {
    val some = model.mode == FindMeMode.SOME
    val chosenCount = scopes.count { it.chosenKey() in model.chosen }
    val helper = if (model.groupsForm == FindMeForm.CHOSEN) {
        UiText(
            "who_can_find_me.groups_chosen_helper",
            texts = mapOf("chosen" to chosenCount.toString(), "total" to scopes.size.toString()),
        )
    } else {
        FIND_ME_MEMBERS_ALL_HELPER
    }
    val contactsHelper = if (model.contactsForm == FindMeForm.CHOSEN) {
        UiText(
            "who_can_find_me.contacts_chosen_helper",
            texts = mapOf(
                "chosen" to contacts.count { it.accountIdHex.lowercase() in model.chosenContacts }.toString(),
                "total" to contacts.size.toString(),
            ),
        )
    } else {
        FIND_ME_FOF_HELPER
    }
    return FindMeRows(
        showParts = some,
        showForm = some && model.groupsForm != null,
        chosenCount = chosenCount,
        scopeCount = scopes.size,
        membersHelper = helper,
        showContactsForm = model.contactsPartOn,
        contactsHelper = contactsHelper,
    )
}

/** The line under the title for an apply status (UX sections 8 and 9), or
 *  `null` when the setting is in effect. */
public fun findMeStatusLine(status: ReachApplyStatus): UiText? = when (status) {
    ReachApplyStatus.APPLIED, ReachApplyStatus.APPLYING -> null
    ReachApplyStatus.OFFLINE -> FIND_ME_OFFLINE
    ReachApplyStatus.FAILED -> FIND_ME_FAILED
}

/** Groups first, then channels, each by name (UX section 4). */
public fun sortedFindMeScopes(scopes: List<FindMeScope>): List<FindMeScope> =
    scopes.sortedWith(compareBy<FindMeScope> { it.kind.ordinal }.thenBy { it.name.lowercase() })

/** UX section 4.1 — sorted as the Contacts screen sorts them. */
public fun sortedFindMeContacts(contacts: List<FindMeContact>): List<FindMeContact> {
    val byId = contacts.associateBy { it.accountIdHex }
    val rows = contacts.map { ContactRowModel(it.accountIdHex, it.name, it.username, it.placeholder) }
    return contactSections(rows).flatMap { it.rows }.mapNotNull { byId[it.key] }
}

/** UX section 3 — the screen's explanation under the title: two sentences
 *  joined through `common.sentence_pair`. */
public val FIND_ME_EXPLANATION: UiText = UiText(
    "common.sentence_pair",
    parts = mapOf(
        "first" to UiText("who_can_find_me.lede"),
        "second" to UiText("who_can_find_me.lede_link"),
    ),
)

/** UX section 3 — helper text per row. */
public val FIND_ME_NOBODY_HELPER: UiText = UiText("who_can_find_me.mode.nobody_helper")
public val FIND_ME_FOF_HELPER: UiText = UiText("who_can_find_me.contacts_of_contacts_helper")
public val FIND_ME_MEMBERS_ALL_HELPER: UiText = UiText("who_can_find_me.groups_all_helper")
public val FIND_ME_EVERYONE_HELPER: UiText = UiText("who_can_find_me.mode.everyone_helper")

/** UX section 4 — the empty list. */
public val FIND_ME_NO_SCOPES: UiText = UiText("who_can_find_me.list.empty")

/** UX section 8 — offline and failure. */
public val FIND_ME_OFFLINE: UiText = UiText("who_can_find_me.status.offline")
public val FIND_ME_FAILED: UiText = UiText("who_can_find_me.status.failed")

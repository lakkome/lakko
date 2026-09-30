// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.settings

import me.lakko.app.reach.ReachApplyStatus

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

/** The setting as the screen reads it. [chosen] holds `kind:idHex` keys. */
public data class FindMeModel(
    val mode: FindMeMode,
    val contactsOfContacts: Boolean,
    val groupsForm: FindMeForm?,
    val chosen: Set<String>,
)

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
    val membersHelper: String,
)

/** Section 3's rows for [model] over the groups and channels [scopes]. */
public fun findMeRows(model: FindMeModel, scopes: List<FindMeScope>): FindMeRows {
    val some = model.mode == FindMeMode.SOME
    val chosenCount = scopes.count { it.chosenKey() in model.chosen }
    val helper = if (model.groupsForm == FindMeForm.CHOSEN) {
        "Only the ones you choose ($chosenCount of ${scopes.size})."
    } else {
        FIND_ME_MEMBERS_ALL_HELPER
    }
    return FindMeRows(
        showParts = some,
        showForm = some && model.groupsForm != null,
        chosenCount = chosenCount,
        scopeCount = scopes.size,
        membersHelper = helper,
    )
}

/** The line under the title for an apply status (UX sections 8 and 9), or
 *  `null` when the setting is in effect. */
public fun findMeStatusLine(status: ReachApplyStatus): String? = when (status) {
    ReachApplyStatus.APPLIED, ReachApplyStatus.APPLYING -> null
    ReachApplyStatus.OFFLINE -> FIND_ME_OFFLINE
    ReachApplyStatus.FAILED -> FIND_ME_FAILED
}

/** Groups first, then channels, each by name (UX section 4). */
public fun sortedFindMeScopes(scopes: List<FindMeScope>): List<FindMeScope> =
    scopes.sortedWith(compareBy<FindMeScope> { it.kind.ordinal }.thenBy { it.name.lowercase() })

/** UX section 3 — the screen's explanation under the title. */
public const val FIND_ME_EXPLANATION: String =
    "Whoever can find you by username or name can ask to be your contact. " +
        "People with your invite link or QR code can always reach you."

/** UX section 3 — helper text per row. */
public const val FIND_ME_NOBODY_HELPER: String = "Only people with your invite link or QR code can reach you."
public const val FIND_ME_FOF_HELPER: String = "People your contacts have added."
public const val FIND_ME_MEMBERS_ALL_HELPER: String = "Everyone in the groups and channels you're in."
public const val FIND_ME_EVERYONE_HELPER: String = "Anyone can find you by your username or name."

/** UX section 4 — the empty list. */
public const val FIND_ME_NO_SCOPES: String = "You're not in any groups or channels yet."

/** UX section 8 — offline and failure. */
public const val FIND_ME_OFFLINE: String = "Will apply when you're back online."
public const val FIND_ME_FAILED: String = "Couldn't update who can find you. Try again."

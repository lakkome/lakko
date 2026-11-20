// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.suspension

import uniffi.lakko_ffi_uniffi.InfoSection

/**
 * ADR-0099 section 4 - the info blocks a suspended account is not offered:
 * calls and adding people (a contact, a member, an invite link, invitations).
 * Everything else of the shared rule's answer stays, in its order.
 */
public val SUSPENDED_HIDDEN_SECTIONS: Set<InfoSection> = setOf(
    InfoSection.CALL,
    InfoSection.ADD_CONTACT,
    InfoSection.ADD_MEMBER,
    InfoSection.INVITE_LINK,
    InfoSection.INVITE_PEOPLE,
)

/** [sections] as a suspended account sees them ([suspended] `false`: unchanged). */
public fun readOnlySections(sections: List<InfoSection>, suspended: Boolean): List<InfoSection> =
    if (suspended) sections.filterNot { it in SUSPENDED_HIDDEN_SECTIONS } else sections

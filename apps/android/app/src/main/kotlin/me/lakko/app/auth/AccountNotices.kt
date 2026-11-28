// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import android.content.Context

/**
 * ADR-0075 section 9 / `docs/ux/identity-recovery.md` sections 19.2, 19.5
 * and 19.6 - what happened to the account, told to its own devices.
 * `GET /account/notices?after=<id>` is fetched at each session landing with
 * a per-device cursor; each notice is shown once. The selection is pure.
 */
public enum class AccountNoticeKind(public val wire: String) {
    PasswordChangedByEmail("password-changed-by-email"),
    EmailAdded("recovery-email-added"),
    EmailReplaced("recovery-email-replaced"),
    EmailRemoved("recovery-email-removed"),

    /**
     * "A recovery phrase was saved for your account." Since 2026-11-26 the
     * server sends no such notice (ADR-0075, amended: kind 6 retired, and
     * kind 2 "account opened by email" with the email vault): a device
     * learns it from the account's published protection, when its own seal
     * announcement gains a phrase seal this device did not add
     * ([phraseSavedElsewhere]). The wire name is one the server never sends.
     */
    PhraseSaved("local:recovery-phrase-saved"),
    ;

    public companion object {
        public fun ofWire(wire: String): AccountNoticeKind? = entries.firstOrNull { it.wire == wire }
    }
}

/** One notice as the server sent it. */
public data class AccountNotice(val id: Long, val kind: AccountNoticeKind, val atMs: Long)

/** One row of the wire page, before its kind is known to this build. */
public data class WireAccountNotice(val id: Long, val kind: String, val atMs: Long)

/** How a notice is drawn. Every kind answers with `It was me` / `It was
 *  not me`. */
public enum class AccountNoticeShape {
    /** A title and a body with the date (section 19.6). */
    Alarm,

    /** One line (sections 19.2 and 19.5). */
    Line,
}

public fun accountNoticeShape(kind: AccountNoticeKind): AccountNoticeShape = when (kind) {
    AccountNoticeKind.PasswordChangedByEmail -> AccountNoticeShape.Alarm
    else -> AccountNoticeShape.Line
}

/** The catalog key of a notice's title (an alarm) or its one line. */
public fun accountNoticeTitleKey(kind: AccountNoticeKind): String = when (kind) {
    AccountNoticeKind.PasswordChangedByEmail -> "notice.password_by_email.title"
    AccountNoticeKind.EmailAdded -> "notice.email_added"
    AccountNoticeKind.EmailReplaced -> "notice.email_replaced"
    AccountNoticeKind.EmailRemoved -> "notice.email_removed"
    AccountNoticeKind.PhraseSaved -> "notice.phrase_saved"
}

/** The catalog key of an alarm's body (it takes `{date}`), or `null`. */
public fun accountNoticeBodyKey(kind: AccountNoticeKind): String? = when (kind) {
    AccountNoticeKind.PasswordChangedByEmail -> "notice.password_by_email.body"
    else -> null
}

/** Where `It was not me` goes: the one secure screen, for every kind. */
public enum class NotMeDestination {
    /** `notice.secure.title`: new password, remove the email, and (no phrase
     *  saved) save a phrase. */
    SecureAccount,
}

@Suppress("UNUSED_PARAMETER")
public fun notMeDestination(kind: AccountNoticeKind): NotMeDestination = NotMeDestination.SecureAccount

/** The rows of the `It was not me` screen, in order. */
public data class SecureAccountRows(
    val newPassword: Boolean,
    val removeEmail: Boolean,
    val savePhrase: Boolean,
)

/**
 * The `It was not me` screen (section 19.6): the new password always, `Remove
 * recovery email` while the account has one, and `Save a recovery phrase`
 * only while none is saved. Since the email vault was removed whoever came
 * in by email got no keys, so saving a phrase changes no key.
 */
public fun secureAccountRows(status: RecoveryEmailStatus?, phraseSaved: Boolean): SecureAccountRows =
    SecureAccountRows(
        newPassword = true,
        removeEmail = status?.enabled == true,
        savePhrase = !phraseSaved,
    )

/**
 * Whether "A recovery phrase was saved for your account." is shown on this
 * device (UX 19.5): its account's announcement lists a phrase seal now, the
 * one this device read before did not, and this device is not the one that
 * saved it. A first read ([before] `null`) shows nothing: there is nothing
 * to compare with.
 */
public fun phraseSavedElsewhere(
    before: SealAnnouncementView?,
    now: SealAnnouncementView,
    savedHere: Boolean,
): Boolean = before != null && before.keyGeneration == now.keyGeneration &&
    before.phrase == null && now.phrase != null && !savedHere

/** How close to this device's own action a notice of the same kind must be
 *  to count as that action: ten minutes, one notice per marker. */
public const val OWN_ACTION_WINDOW_MS: Long = 10L * 60L * 1000L

/** How far back a device with no cursor asks (`since_ms = now - 30 days`). */
public const val NOTICE_FIRST_LOOKBACK_MS: Long = 30L * 24L * 60L * 60L * 1000L

/** What one page request names: the cursor, or a time for a device that has
 *  none yet. Exactly one is set. */
public data class NoticeQuery(val after: Long?, val sinceMs: Long?)

/** The first request of a landing. */
public fun noticeQuery(cursor: Long?, nowMs: Long): NoticeQuery =
    if (cursor == null) {
        NoticeQuery(after = null, sinceMs = nowMs - NOTICE_FIRST_LOOKBACK_MS)
    } else {
        NoticeQuery(after = cursor, sinceMs = null)
    }

/** The outcome of one fetch. */
public data class NoticeSelection(
    /** What to show, oldest first. */
    val show: List<AccountNotice>,
    /** The cursor to store. */
    val cursor: Long,
    /** The own-action markers that matched a notice and are spent. */
    val spentOwn: Set<AccountNoticeKind>,
)

/**
 * Choose what a landing shows from one fetched run of notices.
 *
 *   - A device with no cursor yet ([cursor] `null`) is new to the account:
 *     it shows nothing that happened before it landed, and the newest id
 *     becomes its cursor. Its own actions in that run still spend a marker.
 *   - A kind this build does not know is skipped; the cursor still passes it.
 *   - A notice of a kind this device itself just caused ([ownActions]: kind
 *     to the local time of the action) within [OWN_ACTION_WINDOW_MS] is not
 *     shown, and that marker is spent: a device is never told about its own
 *     action, and a LATER notice of the same kind is shown.
 */
public fun selectAccountNotices(
    fetched: List<WireAccountNotice>,
    nextAfter: Long,
    cursor: Long?,
    ownActions: Map<AccountNoticeKind, Long>,
): NoticeSelection {
    val newest = maxOf(nextAfter, fetched.maxOfOrNull { it.id } ?: 0L, cursor ?: 0L)
    val spent = mutableSetOf<AccountNoticeKind>()
    val show = mutableListOf<AccountNotice>()
    for (row in fetched.sortedBy { it.id }) {
        if (cursor != null && row.id <= cursor) continue
        val kind = AccountNoticeKind.ofWire(row.kind) ?: continue
        val ownAt = ownActions[kind]
        if (ownAt != null && kind !in spent && kotlin.math.abs(row.atMs - ownAt) <= OWN_ACTION_WINDOW_MS) {
            spent += kind
            continue
        }
        if (cursor == null) continue
        show += AccountNotice(id = row.id, kind = kind, atMs = row.atMs)
    }
    return NoticeSelection(show = show, cursor = newest, spentOwn = spent)
}

/** The per-device cursor and the own-action markers. Plain storage: ids,
 *  kinds and times, nothing secret. */
public class AccountNoticeStore(private val storage: MarkerStorage) {

    public fun cursor(accountIdB64: String): Long? = read(CURSOR_PREFIX + accountIdB64)?.toLongOrNull()

    public fun setCursor(accountIdB64: String, cursor: Long): Unit =
        write(accountIdB64, CURSOR_PREFIX + accountIdB64, cursor.toString())

    /** This device did [kind] at [atMs]: the notice it causes is not shown here. */
    public fun markOwn(accountIdB64: String, kind: AccountNoticeKind, atMs: Long): Unit =
        write(accountIdB64, ownKey(accountIdB64, kind), atMs.toString())

    /**
     * [markOwn] for an action a sign-in takes BEFORE its account lands (the
     * email route's `recover/open`, which the server records as a notice):
     * the account may still be sealed from its last sign-out in this
     * process, and a refused marker would show the person their own action
     * as someone else's once the landing reads the notices. Refused only for
     * an account whose data was wiped
     * ([me.lakko.app.account.AccountStoreGate.writeUnlessWiped]).
     */
    public fun markOwnBeforeLanding(accountIdB64: String, kind: AccountNoticeKind, atMs: Long) {
        try {
            me.lakko.app.account.AccountStoreGate.writeUnlessWiped(accountIdB64) {
                storage.set(ownKey(accountIdB64, kind), atMs.toString())
            }
        } catch (_: Throwable) {
            // The value then lives for this process only.
        }
    }

    public fun ownActions(accountIdB64: String): Map<AccountNoticeKind, Long> =
        AccountNoticeKind.entries.mapNotNull { kind ->
            read(ownKey(accountIdB64, kind))?.toLongOrNull()?.let { kind to it }
        }.toMap()

    public fun clearOwn(accountIdB64: String, kinds: Set<AccountNoticeKind>) {
        for (kind in kinds) drop(ownKey(accountIdB64, kind))
    }

    /** Markers older than the window can match nothing any more. */
    public fun pruneOwn(accountIdB64: String, nowMs: Long) {
        for ((kind, at) in ownActions(accountIdB64)) {
            if (nowMs - at > OWN_ACTION_KEEP_MS) drop(ownKey(accountIdB64, kind))
        }
    }

    public fun clearAccount(accountIdB64: String) {
        drop(CURSOR_PREFIX + accountIdB64)
        clearOwn(accountIdB64, AccountNoticeKind.entries.toSet())
    }

    private fun ownKey(accountIdB64: String, kind: AccountNoticeKind): String =
        "$OWN_PREFIX${kind.wire}:$accountIdB64"

    private fun read(key: String): String? = try {
        storage.get(key)
    } catch (_: Throwable) {
        null
    }

    private fun write(accountIdB64: String, key: String, value: String) {
        try {
            // Refused for a signed-out or wiped account
            // ([me.lakko.app.account.AccountStoreGate]).
            me.lakko.app.account.AccountStoreGate.write(accountIdB64) { storage.set(key, value) }
        } catch (_: Throwable) {
            // The value then lives for this process only.
        }
    }

    private fun drop(key: String) {
        try {
            storage.remove(key)
        } catch (_: Throwable) {
            // Nothing to drop.
        }
    }

    public companion object {
        public const val PREFS_NAME: String = "lakko-account-notices"
        private const val CURSOR_PREFIX: String = "cursor:"
        private const val OWN_PREFIX: String = "own:"

        /** An own-action marker is kept this long, then dropped unmatched. */
        public const val OWN_ACTION_KEEP_MS: Long = 24L * 60L * 60L * 1000L

        public fun onDevice(context: Context): AccountNoticeStore =
            AccountNoticeStore(SharedPrefsMarkerStorage(context, PREFS_NAME))
    }
}

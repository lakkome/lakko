// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.feed

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * The app-wide home of the feed's "Couldn't check for new messages." state
 * (`docs/ux/feed.md` state table, "refresh failed (network)"). Mirror of web's
 * `feed-refresh-store.ts`.
 *
 * Every check reports here: the app-level group receive loop and the periodic
 * channel post sync in the session shell, and the feed's own pull to refresh
 * and Retry. Each report folds through the pure [recordCheck], so the latest
 * group outcome and the latest channel outcome stay apart and the banner is
 * [feedRefreshFailed] of both.
 *
 * Per account and in memory only: [bind] to a different account starts from
 * nothing, [reset] (sign-out, forget device) clears it, and a report naming an
 * account other than the bound one (a check that finished after a sign-out or
 * an account switch) is dropped.
 */
public class FeedRefreshStatus {
    private val lock = Any()
    private var account: String? = null
    private val mutable = MutableStateFlow(FeedRefreshState())

    /** The latest outcome per kind of source. */
    public val state: StateFlow<FeedRefreshState> = mutable.asStateFlow()

    /** The account reports are kept for; a change starts from nothing. */
    public fun bind(accountIdB64: String?) {
        synchronized(lock) {
            if (account == accountIdB64) return
            account = accountIdB64
            mutable.value = FeedRefreshState()
        }
    }

    /** Forget everything (sign-out, forget device). */
    public fun reset(): Unit = bind(null)

    /**
     * Fold one check in. [accountIdB64] is the account the check ran for,
     * captured before it started; a mismatch with the bound account, or no
     * account at all, drops the report.
     */
    public fun record(accountIdB64: String?, kind: CheckKind, check: SourceCheck) {
        synchronized(lock) {
            if (accountIdB64 == null || accountIdB64 != account) return
            mutable.value = recordCheck(mutable.value, kind, check)
        }
    }

    /** `true` while the banner shows. */
    public val failed: Boolean get() = feedRefreshFailed(mutable.value)
}

/** What a check that failed as a whole reports: something was tried and
 *  nothing answered. */
public val FAILED_CHECK: SourceCheck = SourceCheck(tried = 1, answered = 0)

/**
 * Run one background check and report it: what [check] returns is recorded
 * for [accountIdB64]; a check that throws records [FAILED_CHECK] (a failed
 * poll counts as not answered) and the throwable is rethrown, so the caller
 * keeps its own backoff. Cancellation records nothing.
 */
public suspend fun FeedRefreshStatus.reporting(
    accountIdB64: String?,
    kind: CheckKind,
    check: suspend () -> SourceCheck,
): SourceCheck {
    val result = try {
        check()
    } catch (cancel: CancellationException) {
        throw cancel
    } catch (e: Throwable) {
        record(accountIdB64, kind, FAILED_CHECK)
        throw e
    }
    record(accountIdB64, kind, result)
    return result
}

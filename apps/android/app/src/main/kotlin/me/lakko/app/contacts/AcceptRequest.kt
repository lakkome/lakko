// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import kotlin.coroutines.cancellation.CancellationException

/*
 * Accepting a contact request must complete, every time. A plain accept runs
 * the same order as an invite-link accept ([LinkClaims.runBuild]), with no
 * claim and no note since the person pressed Accept on this device:
 *
 *   1. ask the Delivery Service whether the answer can reach the requester
 *      (nothing else stands yet, so a no leaves the request listed);
 *   2. found the one-to-one and record it durably, nothing sent;
 *   3. hold it, which is also what files the contact with its chat;
 *   4. write the Welcome, this account's own mailbox first;
 *   5. the ack, only after the Welcome landed.
 *
 * Once the founding is recorded the Welcome is OWED: a failure at step 4 keeps
 * the record, and every inbox pass carries on from it. The answer is written
 * under the request's reply grant, then a held `contact` grant, and nothing
 * else (`WritePurpose.REPLY`).
 */

/** How an accept ended. */
public enum class AcceptOutcome {
    /** The Welcome was delivered and the ack attempted. */
    DONE,

    /** The chat stands and its Welcome is owed; the inbox pass retries it. */
    WELCOME_OWED,

    /** Nothing was delivered and no group stands: the request stays listed. */
    FAILED,

    /** ADR-0098 — the requester's account was deleted: the card is removed
     *  and `person.deleted.body` is shown. Nothing was built. */
    ACCOUNT_DELETED,
}

/** Neither the request's reply grant nor a `contact` grant is held for the
 *  person, so nothing may be written: an unsigned write to a closed mailbox is
 *  answered as stored and dropped. */
public class NoAnswerGrant : Exception("no grant opens this person's mailbox")

/** What an accept needs from the device, injected so its order is one tested
 *  function ([runAccept]). */
internal interface AcceptDeps : LinkClaims.BuildDeps {
    /** Whether a grant the answer may use opens the requester's mailbox. A
     *  throw means the question could not be asked. */
    suspend fun answerable(): Boolean

    /** A fixed reason, no identifier. */
    fun noteFailure(reason: String)
}

/** Run one accept, or carry on one whose founding is recorded. Never throws
 *  (a cancellation is let through). */
internal suspend fun runAccept(d: AcceptDeps): AcceptOutcome {
    if (d.founded() == null) {
        val answerable = try {
            d.answerable()
        } catch (cancel: CancellationException) {
            throw cancel
        } catch (_: Throwable) {
            d.noteFailure("reach-check-failed")
            return AcceptOutcome.FAILED
        }
        if (!answerable) {
            d.noteFailure("no-grant-opens-the-mailbox")
            return AcceptOutcome.FAILED
        }
    }
    val completed = try {
        LinkClaims.runBuild(d)
    } catch (cancel: CancellationException) {
        throw cancel
    } catch (_: Throwable) {
        false
    }
    return when {
        completed -> AcceptOutcome.DONE
        d.founded() != null -> {
            d.noteFailure("welcome-owed")
            AcceptOutcome.WELCOME_OWED
        }
        else -> {
            d.noteFailure("founding-failed")
            AcceptOutcome.FAILED
        }
    }
}

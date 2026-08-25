// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

/**
 * ADR-0045 — the send-path ordering: SEAL and PUBLISH under ONE per-group lock
 * hold.
 *
 * A pure orchestration over injected steps, for the same reason
 * [runCrashSafeCommit] and [runSplitPoll] are: the property being fixed is an
 * ORDERING property between a lock, an MLS state mutation and a network write,
 * and an ordering property is only actually pinned if a test can run it.
 *
 * ## The hazard, stated precisely
 *
 * It is NOT "two seals interleave" — the provider lock already prevents that.
 * And it is NOT "two application messages arrive out of order": within one epoch
 * MLS orders by generation, not by journal position, so a reordered pair of
 * same-epoch messages both decrypt.
 *
 * The hazard is that an application message can be SEALED at one epoch and
 * PUBLISHED on the far side of a commit that changes it. `lakko-mls` never calls
 * `MlsGroupCreateConfig::max_past_epochs`, so openmls's default of **0** applies
 * ("If `max_past_epochs` is 0, only the current epoch is being stored"). A peer
 * therefore keeps NO past-epoch secrets: a straddling message is not late, it is
 * **permanently undecryptable for every recipient** — dropped silently, with the
 * sender's own echo still on screen.
 *
 * Both directions are reachable whenever the publish sits outside the lock the
 * commit regions, the poll's apply and the ADR-0045 sweep all hold:
 *
 *  - **sealed AHEAD.** A commit region eager-merges N -> N+1 *inside* its create,
 *    then parks in its durable persist and its publish. A send firing in that
 *    window seals at N+1 and can reach the DS first, so peers still at N see a
 *    future-epoch message and drop it.
 *  - **sealed BEHIND.** An ADR-0045 sweep STAGES (the group stays at N) and
 *    publishes its commit, then spends a second round-trip reading the journal
 *    verdict before merging. `MlsGroup::create_message` gates on `is_active()`,
 *    **not** `is_operational()`, so a pending commit does NOT block a send: it
 *    seals at N and, the commit already being on the wire, necessarily publishes
 *    after it. In that window the loss is not a race but a CERTAINTY, for the
 *    full duration of the verdict read.
 *
 * ## Why the two-tier shape survives
 *
 * G-37's rule is that a 30 s DS long poll must not be awaited under a lock. This
 * does not reintroduce that: the [lock] is PER GROUP (one group's send queues
 * behind that group's own commits, which is exactly the ordering being bought),
 * and the GLOBAL [providerLock] — the one every other group contends on — is
 * released the moment the seal is done, before the publish.
 *
 * @param seal the provider-touching half: advance the ratchet and durably
 *   persist, fail-closed. Runs under BOTH locks. A throw aborts before any
 *   publish, so no gen-N ciphertext ever leaves the device.
 * @param publish the network half. Runs under the per-group lock ONLY.
 */
internal suspend fun <S, T> runGroupSend(
    lock: Mutex,
    providerLock: Mutex,
    seal: suspend () -> S,
    publish: suspend (S) -> T,
): T = lock.withLock {
    val sealed = providerLock.withLock { seal() }
    publish(sealed)
}

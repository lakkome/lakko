// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.content.Context
import java.io.File
import java.util.Base64
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/**
 * Durable pending-commit outbox (ADR-0040, G-35) — Android mirror of the web
 * reference (`apps/web/src/chat/commit-outbox.ts`).
 *
 * ## Why this exists
 *
 * The four group-membership commit paths ([ChatRepository.addMemberToGroup] /
 * [ChatRepository.removeMemberFromGroup] / [ChatRepository.setMemberAdmin] /
 * [ChatRepository.approveExternalJoin]) each run: core create-commit (which
 * eagerly merges epoch N -> N+1 in the in-memory OpenMLS provider) -> publish
 * the commit (and, for add / approve, a Welcome / rendezvous) to the Delivery
 * Service -> persist the N+1 snapshot best-effort. A crash — or a swallowed
 * persist failure — in the window between publish and persist left the commit
 * DURABLE on the DS (peers advance to N+1) while the local snapshot rolled back
 * to N: a silent, single-device membership brick (the device is stuck at N,
 * cannot re-apply its own commit — OpenMLS `CannotDecryptOwnMessage` — and its
 * future N-epoch messages are rejected by peers at N+1).
 *
 * A bare persist-before-publish reorder does NOT fix this — it flips the desync
 * to "device durably at N+1 with the commit never published", stranding the
 * whole group. The commit's *durability*, not its *ordering*, must change: the
 * commit (and its Welcome) has to survive the crash so it can be RE-PUBLISHED to
 * catch the group up. That is what this outbox holds.
 *
 * ## Atomicity — entry-first-then-snapshot (ADR-0040 Open Question 1)
 *
 * Unlike web's IndexedDB (which offers a cross-store atomic transaction spanning
 * the snapshot + outbox stores), the Android MLS snapshot is a Keystore-wrapped
 * file per account ([IdentitySnapshotStore]) — there is NO cross-store atomic
 * transaction. So the commit path writes, BEFORE any publish and BOTH
 * FAIL-CLOSED:
 *
 *   1. the outbox entry (commit + Welcome / rendezvous bytes, un-acked), THEN
 *   2. the N+1 MLS snapshot.
 *
 * If EITHER write throws, the path ABORTS before publishing (see
 * [prepareCrashSafeCommit]). The one torn middle state — an entry present but the
 * snapshot still at N (the snapshot write failed / crashed after the entry
 * write) — is reconciled on the restart sweep: because the publish is STRICTLY
 * after step 2 completes, that torn state provably means the commit was NEVER
 * published, so the sweep DISCARDS the entry and stays at N (clean). This is why
 * [PendingCommitEntry.targetEpoch] is stored: the sweep compares it to the
 * group's restored epoch and discards any entry whose snapshot did not advance
 * to targetEpoch (see [reconcileVerdict]).
 *
 * The forbidden state "snapshot at N+1 with NO outbox entry" (the whole-group
 * strand) is impossible under this ordering too: the entry is written FIRST, so
 * a durable N+1 snapshot always has a preceding durable entry.
 *
 * ## What an entry holds (invariants #1 / #2 preserved)
 *
 * MLS commit ciphertext + a HPKE-sealed Welcome / rendezvous — opaque PUBLIC
 * artefacts the DS already stores verbatim. No plaintext, no PII, nothing that
 * does not already leave the device, so the entry is stored as plain base64
 * JSON (it is NOT the security-critical snapshot, which stays Keystore-wrapped
 * in the sibling store). The store lives under `noBackupFilesDir` — MLS state is
 * device-bound; a cloud restore must not carry stale pending commits across
 * devices.
 *
 * ## Retention
 *
 * A successfully (re)published entry is marked acked ([markAcked]); the next
 * sweep GC-deletes acked entries. An un-acked entry that keeps failing to
 * re-publish (a persistently unreachable DS) is left for the next sweep — a
 * retry/backoff + "membership change pending" UX is a deferred follow-up
 * (ADR-0040 Open questions). The whole store is purged per-account on
 * forgetDevice ([deleteForAccount]), together with the snapshot it pairs with.
 * A recover does NOT purge it: since 2026-10-02 the account backup carries no
 * MLS state, so a recover leaves this device's own snapshot in place, and the
 * un-acked entries recorded against that snapshot still have to be
 * re-published (dropping them would be the very strand this store exists to
 * prevent).
 */

/**
 * The re-publish destination of a DIRECT-path entry's second artifact. `null` =
 * the entry carries only a commit (remove / role-change paths).
 *
 * One member since ADR-0045 S7. The external-join approve used to write a
 * `RENDEZVOUS` entry riding the group journal; it is a proposal now, so it
 * commits nothing, records nothing, and the sweep owns every rendezvous
 * ([runCommitterSweep]).
 */
@Serializable
public enum class SecondArtifactKind {
    /** A Welcome — re-published to the new member's mailbox
     *  (`DsClient.publishMailbox`, addressed by [PendingCommitEntry.secondArtifactMailboxB64]).
     *  Never echoes back to the committer, so the sweep does NOT re-arm the echo
     *  filter for it. */
    WELCOME,
}

/**
 * One durable pending-commit record. The primary key is [id] (a random hex
 * string, the on-disk filename). The commit + Welcome / rendezvous bytes are
 * stored base64 — opaque MLS public artefacts (ADR-0040; invariants #1 / #2).
 */
@Serializable
public data class PendingCommitEntry(
    /** Random 16-byte hex id — the on-disk primary key / filename stem. */
    val id: String,
    /** base64(account_id) — so a logout / account switch can purge stale
     *  entries and the sweep never crosses accounts. */
    val accountId: String,
    /** base64(group_id) — the sweep filters entries on this. */
    val groupId: String,
    /** Decimal string of the post-commit epoch (N+1). The restart sweep compares
     *  it to the group's restored epoch: an un-acked entry whose snapshot did
     *  NOT advance to this epoch is a torn write and is DISCARDED (the commit was
     *  never published). See [reconcileVerdict]. */
    val targetEpoch: String,
    /** base64 of the EXACT commit wire-bytes the core create-commit produced. */
    val commitB64: String,
    /** base64 of the second artifact's EXACT publishable wire-bytes — for
     *  [SecondArtifactKind.WELCOME] the raw MLS Welcome (ADR-0078: sealed per
     *  mailbox when re-published); before ADR-0045 S7 also the
     *  `wrapWelcomeRendezvous(...)` group-journal bytes. `null` for remove /
     *  role (they publish only the commit). */
    val secondArtifactB64: String? = null,
    /** Discriminates the second artifact's re-publish destination. `null` when
     *  there is no second artifact. */
    val secondArtifactKind: SecondArtifactKind? = null,
    /** base64 of the recipient mailbox pseudonym — the `DsClient.publishMailbox`
     *  target for a [SecondArtifactKind.WELCOME] second artifact. `null` for
     *  [SecondArtifactKind.RENDEZVOUS] (that rides the group journal, addressed
     *  by [groupId]). */
    val secondArtifactMailboxB64: String? = null,
    /** `false` until BOTH artifacts have (re)published. An un-acked entry is
     *  swept on restart / group-open; an acked entry is GC-deleted by the next
     *  sweep. */
    val acked: Boolean = false,
    /** DS ids once known — recorded on ack for diagnostics. */
    val commitMessageId: Long? = null,
    val secondMessageId: Long? = null,
    /**
     * ADR-0045 — `true` when this entry records a **staged committer sweep**
     * rather than one of the four direct-commit paths.
     *
     * The two are reconciled by different rules and MUST NOT be mixed:
     *
     *  - a direct-path entry is written AFTER the core eager-merged N -> N+1, so
     *    [reconcileVerdict] can ask "did the durable snapshot reach
     *    [targetEpoch]?" and treat a shortfall as a torn write;
     *  - a sweep entry is written while the group deliberately sits at N holding
     *    an UN-merged commit, so that same question would answer "torn" for
     *    every healthy sweep and DISCARD a commit that is already on the wire —
     *    and, worse, the legacy loop would re-publish the sweep's Welcome, which
     *    ADR-0045 obligation 2 forbids before the verdict.
     *
     * So [ChatRepository.sweepPendingCommits] routes a sweep entry to
     * [resolveOwedVerdict] FIRST and excludes it from the legacy loop. The
     * authority on whether a verdict is owed is the core's `hasStagedSweep()`,
     * never this flag; the flag only says which reconciler owns the entry.
     */
    val isSweep: Boolean = false,
    /**
     * ADR-0045 — this sweep's verdict is SETTLED (its commit won and merged) and
     * the only thing it still owes is the Welcome fan-out.
     *
     * Set in one durable write at the moment of the merge, which is what keeps
     * obligation 2 intact: the flag IS the durable statement "the verdict said
     * WON", so an entry that has not reached a verdict never carries it and a
     * pre-verdict restart therefore cannot be talked into publishing a Welcome.
     *
     * [isSweep] stays `true` alongside it, deliberately — that flag is what marks
     * the entry as a SWEEP entry, and [ChatRepository.sweepPendingCommits]'s
     * legacy loop filters on it. Clearing it would hand a welcome-owed entry to
     * a loop that re-publishes the commit, finds no singular
     * [secondArtifactMailboxB64], publishes no Welcome at all, and then ACKS —
     * destroying the record. The two are read together: `isSweep &&
     * !sweepWelcomesOwed` is verdict-owed, `sweepWelcomesOwed` is fan-out-owed.
     */
    val sweepWelcomesOwed: Boolean = false,
    /**
     * ADR-0045 — base64 signature keys of the leaves a sweep adds, for its
     * Welcome fan-out.
     *
     * A sweep commit can carry SEVERAL adds under ONE Welcome, where every
     * direct path has at most one destination ([secondArtifactMailboxB64]).
     * ADR-0060 §6 — each leaf is resolved to its ACCOUNT mailbox at publish
     * time, after the merge. Empty for a direct-path entry and for a sweep
     * that added nobody.
     */
    val sweepWelcomeLeafKeysB64: List<String> = emptyList(),
    /**
     * ADR-0060 §6 — base64 of the added person's account key for a direct
     * add's [SecondArtifactKind.WELCOME]: what picks the reach grant the
     * Welcome is written with on a restart re-publish. `null` otherwise.
     */
    val secondArtifactAccountKeyB64: String? = null,
    /**
     * ADR-0045 S7 — a sweep's group-journal `WelcomeRendezvous` envelopes, one
     * per member the sweep adds.
     *
     * Whole envelopes rather than addresses, because a rendezvous binds the
     * added member's signature key into its own bytes — so unlike the single
     * Welcome envelope fanned out to N mailboxes above, every recipient gets
     * different bytes. Recorded before any publish so a restart re-publishes
     * exactly what this sweep computed. Empty for a direct-path entry and for a
     * sweep that added nobody.
     */
    val sweepWelcomeRendezvousB64: List<String> = emptyList(),
)

/** The reconciliation verdict for one un-acked entry read on the restart sweep. */
internal enum class SweepVerdict {
    /** The group's restored epoch reached (or passed) [PendingCommitEntry.targetEpoch]
     *  — the commit was durably merged, so re-publish it idempotently. */
    RE_PUBLISH,

    /** Either the entry is already acked (steady-state GC) OR the group's
     *  restored epoch did NOT reach [PendingCommitEntry.targetEpoch] — a torn
     *  write between the entry and the snapshot, which (publish being strictly
     *  after the snapshot write) provably means the commit was NEVER published.
     *  Drop the orphan entry; the device stays at its current epoch (clean). */
    DISCARD,
}

/**
 * The pure restart-sweep reconciliation decision (ADR-0040 §2, Android
 * entry-first-then-snapshot torn-write reconciliation). No I/O — unit-testable.
 *
 *   - `acked` -> DISCARD (GC an already-published entry).
 *   - un-parseable / snapshot below targetEpoch -> DISCARD (torn write: the
 *     commit never durably completed, so it was never published — drop it).
 *   - snapshot at or past targetEpoch -> RE_PUBLISH (the commit was durably
 *     merged; re-publish idempotently to catch the group up / re-arm the echo).
 */
internal fun reconcileVerdict(currentEpoch: ULong, entry: PendingCommitEntry): SweepVerdict {
    if (entry.acked) return SweepVerdict.DISCARD
    val target = entry.targetEpoch.toULongOrNull() ?: return SweepVerdict.DISCARD
    return if (currentEpoch >= target) SweepVerdict.RE_PUBLISH else SweepVerdict.DISCARD
}

/**
 * The crash-safe commit ordering (ADR-0040 §2): the durable writes a commit makes
 * before it may be published, as a pure function over injected steps so it is
 * unit-testable without the FFI Group / Identity.
 *
 * Android has no cross-store atomic transaction (Open Question 1), so the two
 * durable writes run in order — the outbox ENTRY first, THEN the SNAPSHOT — and
 * BOTH are fail-closed. Returning normally is the signal that both landed and
 * the commit is now safe to put on the wire; throwing means the op aborted and
 * the caller publishes nothing (on the next launch the sweep reconciles the
 * orphan-or-absent entry to "clean").
 *
 * G-37 — split from the publish so the caller can release the repository's
 * global provider lock before the DS publish. DELIBERATELY NOT `suspend`: both
 * durable writes are synchronous and every caller runs it inside the provider
 * critical section, so no network wait can be reintroduced here (a suspending
 * call would not compile). Same device as [ChatClient.applyFetchedMessages].
 *
 * Since the ADR-0081 amendment of 2026-10-19 the only caller is the ADR-0045
 * staged sweep: the clients' direct commits (which eager-merged N -> N+1 before
 * this persist and needed an in-memory reset on an abort) are deleted. A staged
 * sweep leaves the group at N, so an abort needs no reset here; the caller's
 * per-group `abandonStagedSweep` is the rollback.
 */
internal fun prepareCrashSafeCommit(
    persistEntryOrThrow: () -> Unit,
    persistSnapshotOrThrow: () -> Unit,
) {
    // 1. durable outbox entry (commit + second-artifact bytes, un-acked).
    persistEntryOrThrow()
    // 2. durable MLS snapshot carrying the staged commit. A throw from EITHER
    //    step propagates before the caller can publish — abort before publish.
    persistSnapshotOrThrow()
}

/**
 * The pure restart-sweep orchestration (ADR-0040 §5), over injected effects so
 * it is unit-testable without the FFI Group / DS / echo filter. For each entry:
 *
 *   - [SweepVerdict.DISCARD] -> [onDiscard] (torn-write orphan or acked GC).
 *   - [SweepVerdict.RE_PUBLISH] -> deliver what the entry still owes (its
 *     Welcome), then [onAck]. A delivery FAILURE (offline DS) leaves the entry
 *     (no [onAck]) for the next sweep.
 *
 * ADR-0081 — a direct commit's entry is written only after the commit route
 * took the commit, so the commit is on the journal already and is never
 * re-sent here (the ack carries no commit id: the entry does not hold it).
 * Since ADR-0045 S7 the only direct-path second artifact is a WELCOME, which
 * goes to the new member's mailbox and never echoes back to the committer.
 */
internal suspend fun sweepPendingCommitEntries(
    entries: List<PendingCommitEntry>,
    currentEpoch: ULong,
    republishWelcome: suspend (mailbox: ByteArray, bytes: ByteArray) -> Long,
    onAck: suspend (id: String, commitMessageId: Long?, secondMessageId: Long?) -> Unit,
    onDiscard: suspend (id: String) -> Unit,
    decodeB64: (String) -> ByteArray = ::outboxUnb64,
) {
    for (entry in entries) {
        when (reconcileVerdict(currentEpoch, entry)) {
            SweepVerdict.DISCARD -> onDiscard(entry.id)
            SweepVerdict.RE_PUBLISH -> {
                try {

                    // ADR-0045 S7 — a direct-path second artifact is now ALWAYS
                    // a mailbox Welcome. The rendezvous arm this loop used to
                    // carry belonged to the external-join approve, which no
                    // longer commits and therefore writes no entry at all; the
                    // sweep owns every rendezvous ([runCommitterSweep], one per
                    // added key).
                    var secondMessageId: Long? = null
                    val second = entry.secondArtifactB64
                    val mailboxB64 = entry.secondArtifactMailboxB64
                    if (second != null && mailboxB64 != null) {
                        secondMessageId = republishWelcome(
                            decodeB64(mailboxB64),
                            decodeB64(second),
                        )
                    }
                    onAck(entry.id, null, secondMessageId)
                } catch (_: Throwable) {
                    // Offline / DS down -> leave the entry un-acked; the next
                    // sweep retries the delivery.
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// base64 helpers — java.util.Base64 (API 26+, our minSdk; host-JVM-safe unlike
// android.util.Base64, which keeps the pure helpers unit-testable).
// ---------------------------------------------------------------------------

/** base64-encode opaque wire-bytes for storage in a [PendingCommitEntry]. */
internal fun outboxB64(bytes: ByteArray): String = Base64.getEncoder().encodeToString(bytes)

/** base64-decode a [PendingCommitEntry] field back to wire-bytes. */
internal fun outboxUnb64(s: String): ByteArray = Base64.getDecoder().decode(s)

/**
 * Per-account, file-per-entry durable store for [PendingCommitEntry] records.
 *
 * The store is Context-free at its core ([putEntryIn] / [loadAllIn] /
 * [deleteEntryIn] take a plain [File] directory) so it is unit-testable on the
 * host JVM with a temp dir — no AndroidKeyStore, no `android.util.*`. The
 * Context-based surface resolves the directory under `noBackupFilesDir` and is
 * what [ChatRepository] calls.
 *
 * On-disk layout:
 * ```text
 *   noBackupFilesDir/lakko-commit-outbox/
 *     <id>.json           one PendingCommitEntry (plain base64 JSON)
 * ```
 * Entries are opaque MLS public artefacts (invariants #1 / #2), so they are NOT
 * encrypted — unlike the sibling Keystore-wrapped [IdentitySnapshotStore].
 */
public object CommitOutboxStore {

    /** Subdirectory under the app's no-backup files dir. */
    private const val OUTBOX_DIR = "lakko-commit-outbox"

    private const val JSON_SUFFIX = ".json"
    private const val TMP_SUFFIX = ".json.tmp"

    private val json = Json { ignoreUnknownKeys = true; encodeDefaults = true }

    // ----- Context-based production surface -----

    /**
     * Persist one entry durably, FAIL-CLOSED (THROWS on write failure). The
     * commit path calls this as step 1 of [prepareCrashSafeCommit], BEFORE any
     * publish; a throw aborts the op.
     *
     * @throws java.io.IOException on disk-write failure.
     */
    public fun putEntry(context: Context, entry: PendingCommitEntry) {
        me.lakko.app.account.AccountStoreGate.writeOrEnd(entry.accountId) {
            putEntryIn(dir(context), entry)
        }
    }

    /** Load every entry for `(accountIdB64, groupIdB64)`. The store is tiny (one
     *  entry per in-flight commit), so a full read + in-memory filter is
     *  simplest and robust; unparseable / partial files are skipped. */
    public fun loadForGroup(
        context: Context,
        accountIdB64: String,
        groupIdB64: String,
    ): List<PendingCommitEntry> =
        loadAllIn(dir(context)).filter {
            it.accountId == accountIdB64 && it.groupId == groupIdB64
        }

    /** Mark an entry acked + record its DS ids (read-modify-write). No-op when
     *  the entry is already gone (a concurrent delete raced us). Best-effort in
     *  the caller — a failure leaves the entry un-acked, which the next sweep
     *  re-publishes idempotently. */
    public fun markAcked(
        context: Context,
        id: String,
        commitMessageId: Long?,
        secondMessageId: Long?,
    ) {
        markAckedIn(dir(context), id, commitMessageId, secondMessageId)
    }

    /**
     * ADR-0045 — flip a sweep entry from *verdict owed* to *Welcome fan-out
     * owed*, recording the DS id its commit published under, in ONE atomic write
     * so it is never half-applied.
     *
     * THROWS on failure. The caller treats that as non-fatal: the marker only
     * buys a retry, and the Welcome it guards is correct to publish either way.
     *
     * A no-op when the entry is already gone (a concurrent ack / discard raced
     * us).
     */
    public fun markWelcomesOwed(context: Context, id: String, commitMessageId: Long) {
        markWelcomesOwedIn(dir(context), id, commitMessageId)
    }

    /** Delete one entry by id. No-op when absent. */
    public fun deleteEntry(context: Context, id: String) {
        deleteEntryIn(dir(context), id)
    }

    /** Purge every entry for an account, with the rest of its local data
     *  (Forget device / Delete account). Not on a recover, which keeps the
     *  snapshot these entries pair with. No-op when there is nothing to
     *  purge. */
    public fun deleteForAccount(context: Context, accountIdB64: String) {
        val d = dir(context)
        for (e in loadAllIn(d)) {
            if (e.accountId == accountIdB64) deleteEntryIn(d, e.id)
        }
    }

    private fun dir(context: Context): File =
        File(context.applicationContext.noBackupFilesDir, OUTBOX_DIR)

    // ----- Context-free core (unit-testable) -----

    /** Write one entry atomically (temp file + rename) into [dir]. THROWS on
     *  any failure — the fail-closed durable write. */
    internal fun putEntryIn(dir: File, entry: PendingCommitEntry) {
        dir.mkdirs()
        val encoded = json.encodeToString(PendingCommitEntry.serializer(), entry)
        val target = File(dir, entry.id + JSON_SUFFIX)
        val tmp = File(dir, entry.id + TMP_SUFFIX)
        // writeText THROWS (FileNotFoundException / IOException) when the dir is
        // unwritable / is a regular file — propagated as the fail-closed signal.
        tmp.writeText(encoded)
        if (!tmp.renameTo(target)) {
            // Rename failed (rare) — fall back to a direct write so we never
            // leave a half-committed state, still throwing on failure.
            target.writeText(encoded)
            tmp.delete()
        }
    }

    /** Read every committed entry file in [dir], skipping the in-progress `.tmp`
     *  files and any unparseable / partial `.json` (treated as "no entry"). */
    internal fun loadAllIn(dir: File): List<PendingCommitEntry> {
        val files = dir.listFiles() ?: return emptyList()
        val out = mutableListOf<PendingCommitEntry>()
        for (f in files) {
            if (!f.isFile || !f.name.endsWith(JSON_SUFFIX)) continue
            val entry = try {
                json.decodeFromString(PendingCommitEntry.serializer(), f.readText())
            } catch (_: Throwable) {
                // Corrupt / partial file — skip. Missing entry == torn write ==
                // clean (the commit was never published).
                continue
            }
            out.add(entry)
        }
        return out
    }

    /** Read-modify-write an entry to acked. No-op when absent / unreadable. */
    internal fun markAckedIn(
        dir: File,
        id: String,
        commitMessageId: Long?,
        secondMessageId: Long?,
    ) {
        val file = File(dir, id + JSON_SUFFIX)
        if (!file.isFile) return
        val existing = try {
            json.decodeFromString(PendingCommitEntry.serializer(), file.readText())
        } catch (_: Throwable) {
            return
        }
        putEntryIn(
            dir,
            existing.copy(
                acked = true,
                commitMessageId = commitMessageId ?: existing.commitMessageId,
                secondMessageId = secondMessageId ?: existing.secondMessageId,
            ),
        )
    }

    /** Read-modify-write an entry to welcomes-owed. No-op when absent /
     *  unreadable; THROWS when the write itself fails. */
    internal fun markWelcomesOwedIn(dir: File, id: String, commitMessageId: Long) {
        val file = File(dir, id + JSON_SUFFIX)
        if (!file.isFile) return
        val existing = try {
            json.decodeFromString(PendingCommitEntry.serializer(), file.readText())
        } catch (_: Throwable) {
            return
        }
        putEntryIn(
            dir,
            existing.copy(sweepWelcomesOwed = true, commitMessageId = commitMessageId),
        )
    }

    internal fun deleteEntryIn(dir: File, id: String) {
        File(dir, id + JSON_SUFFIX).delete()
    }
}

/**
 * ADR-0053 client contract point 3 — run the pre-publish registration of a
 * direct (eager-merged) commit, and on ANY failure reset the group to its
 * durable snapshot before re-throwing: the in-memory group already sits at the
 * un-published N+1, and a registration that did not land means the commit
 * must never reach the wire — the same abort a failed outbox write takes.
 *
 * Called AFTER the create-commit and BEFORE the outbox entry is written, so a
 * refused registration leaves no entry for the restart sweep to re-publish.
 * If [reset] itself throws, THAT is the error (a poisoned group), with the
 * registration failure attached.
 */
internal suspend fun <T> registerOrReset(reset: () -> Unit, register: suspend () -> T): T {
    return try {
        register()
    } catch (registrationFailure: Throwable) {
        try {
            reset()
        } catch (resetFailure: Throwable) {
            resetFailure.addSuppressed(registrationFailure)
            throw resetFailure
        }
        throw registrationFailure
    }
}

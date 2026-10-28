// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0040 (G-35 / G-38) — the ONE per-group serializing lock over every region
 * that mutates a group's MLS state.
 *
 * It lives in its own module because three unrelated modules take it and there
 * must be exactly one queue: `contacts/client.ts` (the four create-commit ->
 * publish regions plus the ADR-0045 sweep), `chat/client.ts` (the poll's apply
 * half AND the send path's seal -> publish region), and
 * `channels/copublisher.ts` (the PublisherGroup poll + approve). Two locks over
 * one resource is worse than none, so the lock does NOT live with any one of its
 * callers. It was extracted from `contacts/client.ts` for exactly that reason —
 * see `contacts/commit-lock.test.ts`, which still exercises it through
 * `ContactsClient`.
 *
 * ## Why a lock at all
 *
 * ADR-0040 (G-35): the `lakko-mls` create-commit paths (`addMember` /
 * `removeMember` / `setGroupRoles` / `approveExternalJoin`) **eagerly merge**
 * epoch N -> N+1 into the shared in-memory provider INSIDE the create call,
 * before the caller has published anything. Two of those regions interleaved on
 * one group therefore both build on N: the first publishes commit(N -> N+1), the
 * second publishes commit(N+1 -> N+2) that peers — still at N when it lands, or
 * having only just adopted N+1 — drop as wrong-epoch. The committing device is
 * then durably ahead of a group that never adopted its last commit, which is the
 * "device-ahead" shape ADR-0040 exists to prevent. The outbox sweep cannot
 * repair it: the entry was acked, so there is nothing to re-publish.
 *
 * G-38 residual 1 (local sub-case) and residual 3 extend the same reasoning to
 * the RECEIVE side. `Group.processIncomingVerifyingRoles` mutates the very same
 * in-memory provider — a commit it applies advances the epoch, an application
 * message drains a ratchet — so a poll applying a batch while a local commit is
 * parked in its publish is the same interleave with the same outcome. Android
 * closed this first (`SplitPoll.kt` `runSplitPoll` takes the same per-group
 * mutex the commits take); this is the web mirror.
 *
 * The SEND path (`ChatClient.sendMessage` / `broadcastControl`) takes it for a
 * THIRD, distinct reason, and one worth stating because the obvious reading is
 * wrong: it is not that two `createApplicationMessage` calls interleave (that
 * wasm call is synchronous, so they cannot), nor that two same-epoch messages
 * arrive out of order (MLS orders those by generation). It is that a message
 * sealed at epoch N and published on the far side of a commit that moved the
 * group to N+1 is undecryptable for every recipient — `lakko-mls` never sets
 * `max_past_epochs`, so it is openmls's default of 0 and no peer retains a
 * past-epoch secret. See `ChatClient.sendMessage` for both reachable directions
 * and for the half this lock does NOT close (a race against a REMOTE commit,
 * which is a race with the DS rather than with local state).
 *
 * ## What is NOT in the region
 *
 * The network fetch. Serializing a DS round-trip behind a commit publish is
 * G-37's own defect at a per-group blast radius, and the poll touches no MLS
 * state until the bytes are in hand. Callers fetch first, then take the lock for
 * the apply — the split Android's `runSplitPoll` documents step by step.
 *
 * ## SERIALIZING, never coalescing
 *
 * A second caller **queues and then runs in full** against the first's committed
 * result. It is NOT folded into the in-flight run. Coalescing here would silently
 * drop a distinct membership change — that was `lakko-security` **F1** in the
 * ADR-0043 review, and the same reasoning applies verbatim: two clicks are two
 * intended commits, and dropping one is a membership change the operator
 * believes happened.
 *
 * ## The lock must span the CREATE, not just the publish
 *
 * Acquiring it after the create would fix nothing — the eager merge is exactly
 * what goes stale. Each caller's `task` therefore contains the create-commit
 * itself, so the commit is built on the epoch its predecessor actually
 * published. For `setMemberAdmin` the region additionally starts before the role
 * table is read, for the same reason ADR-0044 C-62 pins for the push-routing
 * row: a decision taken against a snapshot the predecessor has since replaced is
 * no decision at all. The poll's apply half carries the identical obligation for
 * its cursor — see `ChatClient.pollMessages`.
 *
 * ## Ordering, failure isolation, release
 *
 *   - the predecessor is captured and the queue entry replaced **synchronously**,
 *     so runs execute in call order;
 *   - a predecessor's rejection is swallowed HERE so it never becomes contagious
 *     — its own caller still observes it, and the queued commit is still a real
 *     user-intended membership change regardless of why the previous one failed;
 *   - release is TAIL-GUARDED: the entry is dropped only if it is still this
 *     run's promise, so a later task that already replaced it keeps its place.
 *     The `.catch()` before `.finally()` keeps this bookkeeping chain from
 *     surfacing an unhandled rejection (the caller still sees the real one).
 *
 * This adds NO rollback of its own. Every failure path inside `task` behaves
 * exactly as it did before. (The ADR-0040 F1 reset that served the direct
 * commits went with them in the ADR-0081 amendment of 2026-10-19; a staged
 * sweep is rolled back per group by `abandonStagedSweep`.)
 *
 * ## Deadlock
 *
 * The lock is NON-REENTRANT: a region that re-entered it on the same key would
 * await its own promise forever. Audited, and it holds in both directions:
 *
 *   - nothing awaited inside a membership region reaches an apply. The regions
 *     await only `loadWasm()` (a module-cached promise), the fail-closed
 *     persists (device key + IndexedDB) and plain-HTTP publishes;
 *   - nothing awaited inside an apply region reaches a commit. The poll's region
 *     awaits only `parseBodyEnvelope` / `tryDecodeSecretEnvelope` (wasm codecs)
 *     and the best-effort snapshot persist;
 *   - nothing awaited inside a SEND region reaches any of the others. It awaits
 *     only `persistDurableOrThrow` (device key + IndexedDB) and the plain-HTTP
 *     `publishGroup`; the push wake is `void`-fired and never awaited. In the
 *     other direction, every caller of `sendMessage` / `broadcastControl` is a
 *     plain user handler or a timer — the poll-tail and sweep-tail broadcasts
 *     (`rotatePushRoutingSecretIfMembershipShrank`,
 *     `redistributeSecretToGroup`) are fired from a `void (async () => …)`
 *     AFTER the awaited region has already resolved, so they queue behind it
 *     rather than nesting inside it;
 *   - `routes/Chat.tsx` fires `void refresh()` at the TAIL of each commit
 *     handler, i.e. after `await contacts.<op>()` has already resolved and the
 *     region has been left. The poll it starts therefore QUEUES behind the
 *     commit rather than nesting inside it — correct, and not a deadlock.
 *
 * ## Per-group, and what that does NOT cover
 *
 * Two different groups never block each other. Note though that the durable unit
 * underneath is NOT per-group: `ChatApi.persistSnapshotWithCommit` writes
 * `Identity.snapshotStorage()` — the WHOLE provider — to a row keyed on
 * `accountId`, and the F1 abort path calls `restoreStorage` on that whole
 * provider. Concurrent commits on two DIFFERENT groups would therefore still
 * interact through that shared snapshot. That is not reachable today (only
 * `ChatView` commits on an existing group, and the router mounts one at a time),
 * which is why the key stays per-group; it is recorded here because the moment a
 * second commit-capable view can be mounted the key must widen to the account.
 *
 * And it is a SINGLE-REALM lock: `navigator.locks` is deliberately NOT nested
 * inside it the way `push/group-routing.ts` does, because two tabs on one
 * account are already broken independently of any lock — each holds its own
 * in-memory provider and the snapshot is written whole under an account-only
 * key, so tab B builds on a stale epoch and clobbers tab A even after acquiring
 * a cross-tab lock. Two ordinary messages clobber each other the same way.
 */

/**
 * In-flight MLS region per group, keyed on the base64 `group_id`.
 *
 * MODULE-level, not an instance field, and that is load-bearing: a
 * `ContactsClient` is constructed per route component (`routes/Chat.tsx`,
 * `routes/Contacts.tsx`, `routes/Invite.tsx`, `routes/JoinGroup.tsx` each call
 * `new ContactsClient(...)`) while they all drive the SAME canonical wasm
 * `Identity` through `identityProvider`. An instance field would therefore
 * serialize nothing that matters. Mirrors `routingRowQueue` in
 * `push/group-routing.ts`.
 */
const groupMlsQueue = new Map<string, Promise<unknown>>();

/**
 * Observers of "this group's MLS state has gone quiet", see
 * {@link onGroupMlsIdle}.
 */
const idleListeners = new Set<(groupIdB64: string) => void>();

/**
 * Observe every moment a group's lock queue DRAINS — the last queued region for
 * that group has settled (resolved or rejected) and nothing is queued behind it.
 *
 * ## Why this exists
 *
 * Every region that can move a group's MLS epoch runs under this lock: the
 * poll's apply half (the open chat view's long poll AND the background receive
 * for chats that are not open), the create-commit paths, the ADR-0045 sweep,
 * and the ADR-0040 F1 reset that replaces the group with its durable snapshot.
 * A drained queue is therefore the one place that can say "the epoch may have
 * changed, and nothing is half-way through changing it" for a group whatever
 * screen the user is on. The app-level call host uses it to rotate a live group
 * call's SFrame key on an epoch advance (G-18 / ADR-0029) — before, only the
 * mounted chat view noticed an advance, so leaving the chat froze the call key.
 *
 * ## Why only on DRAIN, and never between two queued regions
 *
 * Between two queued regions the next one may already have eagerly merged a
 * commit it has not published yet (ADR-0040). Reporting at that point would let
 * an observer read an epoch that is still in flight and may be rolled back by
 * the F1 reset. On drain the state is whatever the last region left behind; the
 * regions before it need no report of their own because the drain covers them.
 *
 * ## Contract for listeners
 *
 *   - Called synchronously from the queue's own bookkeeping, AFTER the entry has
 *     been removed, so a listener that queues a new region starts a fresh queue
 *     rather than nesting in the one that drained.
 *   - A listener must not itself take this lock on every call, or its own drain
 *     reports again forever. Reading group state (an epoch, an exporter secret)
 *     needs no lock: those wasm calls are synchronous and run between regions.
 *   - A listener that throws is reported and isolated; it can never affect the
 *     lock or the other listeners.
 *
 * What it does NOT see: `restoreStorage` rolls back the WHOLE provider, so the
 * F1 reset of one group can also move another group's in-memory state, and only
 * the group whose region ran is reported. See the per-group note above.
 *
 * @returns an unsubscribe function.
 */
export function onGroupMlsIdle(listener: (groupIdB64: string) => void): () => void {
  idleListeners.add(listener);
  return () => {
    idleListeners.delete(listener);
  };
}

function notifyIdle(groupIdB64: string): void {
  for (const listener of [...idleListeners]) {
    try {
      listener(groupIdB64);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("group MLS idle listener failed", err);
    }
  }
}

/**
 * Run `task` with exclusive access to one group's MLS state.
 *
 * @param groupIdB64 the group's `group_id`, base64 — every caller MUST derive it
 *   the same way (`base64Encode(group.groupId)`) or they key different queues
 *   and serialize nothing.
 */
export function withGroupMlsLock<T>(
  groupIdB64: string,
  task: () => Promise<T>,
): Promise<T> {
  const previous = groupMlsQueue.get(groupIdB64);

  const run = (async (): Promise<T> => {
    if (previous !== undefined) {
      try {
        await previous;
      } catch {
        /* the predecessor's own caller owns that failure */
      }
    }
    return task();
  })();

  groupMlsQueue.set(groupIdB64, run);
  void run
    .catch(() => undefined)
    .finally(() => {
      if (groupMlsQueue.get(groupIdB64) === run) {
        groupMlsQueue.delete(groupIdB64);
        notifyIdle(groupIdB64);
      }
    });
  return run;
}

// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0045 S4a — the pending-membership-request surface, as
 * [`docs/ux/proposal-based-membership.md`] specifies it.
 *
 * ## Why this is a plain module and not part of `routes/Chat.tsx`
 *
 * `vitest.config.ts` collects `src/**\/*.test.ts` in the plain `node`
 * environment with no jsdom and no Solid JSX transform, so a `.tsx` component
 * cannot be tested at all. Every rule the UX spec states — the quiet window,
 * the visibility gate, the copy selection, the applied-by-effect test —
 * therefore lives here, where it is executable, and `Chat.tsx` renders what
 * this module decides. That is the same split `calls/sfu-call-ui.ts` already
 * uses.
 *
 * ## The three UX rules this module exists to enforce
 *
 * 1. **No optimistic update.** A member with a pending removal STAYS in the
 *    roster, annotated. Dropping the row would assert they are gone while they
 *    are still cryptographically in the group — still receiving every message,
 *    still able to decrypt — until the commit lands. That is the membership
 *    equivalent of a green E2EE pill over an unencrypted call, and Lakko's
 *    honesty corollary to invariant #4 forbids it. The same rule in reverse
 *    governs a pending add: a person who has been *requested* is not a member
 *    and must not appear in the members list, so they get their own section.
 * 2. **No `Cancel`.** Once published, the request is in every member's proposal
 *    store and dies only when some commit lands. OpenMLS does expose a *local*
 *    single-proposal eviction, but there is no **wire** withdraw — evicting
 *    locally would leave the request live in every peer's store, which is a
 *    worse lie than showing it. So the confirmation dialog is the last exit,
 *    and this module deliberately exposes no withdraw verb.
 * 3. **No pending affordance until the quiet window elapses.** The owner is
 *    rank 0 and sweeps its own request immediately, so in the common case the
 *    request lands before anyone could look. A spinner that appears and
 *    vanishes in 300 ms is worse than no spinner: it teaches the user that
 *    membership is slow when it is not.
 *
 * ## What is the source of truth, and what this store adds
 *
 * `group.pendingProposals()` is the truth about *what* is pending — it is the
 * core's own retained set, populated on every member by the receipt gate, and
 * it is emptied by OpenMLS on every merged commit, so it needs no GC of its
 * own. This module never second-guesses it.
 *
 * What the core cannot supply is *when* a request arrived, because a
 * `ProposalRecord` carries no timestamp. The quiet window needs one, and per
 * the UX spec it must be the **DS row timestamp**, never a local clock: every
 * member reads the identical value for the identical row, whereas a local
 * `Date.now()` would make the window mean something different on each device.
 * So this module keeps a small per-`(account, group)` observation store keyed
 * on a synthetic request key, and joins it to the live pending set.
 *
 * Entries are pruned against the live set on every fold, so a swept or
 * superseded request cannot leave residue behind.
 */
import { t } from "../l10n";

/** How a proposal is classified for the panel. Mirrors the wasm
 *  `ProposalRecord.kind` strings one-for-one. */
export type PendingRequestKind =
  | "add"
  | "remove"
  | "leave"
  | "update"
  | "role-change"
  | "unsupported";

/** The shape this module reads off a wasm `ProposalRecord`. */
export interface ProposalRecordView {
  readonly kind: string;
  readonly senderLeaf: number | undefined;
  readonly stored: boolean;
  readonly addSignatureKey: Uint8Array | undefined;
  readonly removeTargetLeaf: number | undefined;
}

/** One pending request, joined from the live core set and the observation store. */
export interface PendingRequest {
  /** Stable synthetic key — see {@link proposalKey}. */
  readonly key: string;
  readonly kind: PendingRequestKind;
  /** base64 of the prospective member's signature key. `add` only. */
  readonly addSignatureKeyB64: string | undefined;
  /** The leaf this request removes. `remove` / `leave` only.
   *
   *  For a `remove` this is the proposal's own target field; for a `leave` it
   *  is the SENDER's leaf, because RFC 9420 `SelfRemove` carries no target and
   *  the removed leaf is the sender's own. `undefined` when the sender could
   *  not be resolved. */
  readonly targetLeaf: number | undefined;
  /** The proposing member's leaf, or `undefined` for an unresolvable sender. */
  readonly senderLeaf: number | undefined;
  /** DS row timestamp in ms, or `undefined` when this device never observed the
   *  row (it restored the proposal from a snapshot rather than seeing it land). */
  readonly observedAtMs: number | undefined;
  /** `true` when this device published it. Drives the UX §10 exception — a
   *  member always sees the status of a request they made themselves. */
  readonly own: boolean;
  /**
   * ADR-0045 UX §6.1.10 — base64 of the successor's signature key, on OUR OWN
   * record only, for a `role-change` that is an ownership transfer.
   *
   * It does two jobs at once, and neither is available any other way. It is the
   * only thing that distinguishes *our pending role change is a transfer* from
   * *our pending role change is a promote* — `ProposalKind::RoleChange` carries
   * no payload, so the core cannot tell them apart and {@link proposalKey}
   * collapses every role change to the single key `role-change` — and it is
   * what names the person in `Waiting to make {name} the owner.`
   *
   * The KEY is stored, never a label: the label is resolved through the LIVE
   * roster at render time, so a successor who has since left degrades to the
   * unnamed variant rather than to a stale name.
   */
  readonly transferToB64: string | undefined;
  /** `"pending"` below the quiet window (render nothing), `"waiting"` above it. */
  readonly state: "pending" | "waiting";
}

/**
 * How long a request must be outstanding before any persistent, group-visible
 * pending affordance renders.
 *
 * **CHOSEN, not measured** — the UX spec leaves `QUIET_WINDOW` open (§12.1) and
 * this module is not in a position to close it, so the number is stated as a
 * judgement rather than dressed up as a finding. The budget it is sized against:
 * the owner's own sweep is one local stage plus two DS round-trips, so a window
 * shorter than that would flash an annotation on the path the UX spec is
 * explicit must look instantaneous; and a window much longer than a few seconds
 * would leave a genuine wait invisible for no gain. Nothing else in the system
 * depends on the value — it decides only *when* an annotation appears, never
 * whether a request is live — so it can be re-tuned from a measurement without
 * touching any other rule.
 */
export const QUIET_WINDOW_MS = 3000;

/**
 * A stable key for one request, so an observation recorded when the row landed
 * can be re-joined to the core's pending set on a later render.
 *
 * Derived from what the request *is*, never from a leaf index alone where a
 * signature key is available: OpenMLS reuses a vacated leaf for the next Add,
 * so a leaf-keyed record can silently come to name someone else. For a removal
 * only the leaf is available (the core's `ProposalKind::Remove` carries no
 * signature key), which is why the fold below resolves it against the live
 * roster at render time rather than trusting a stored label.
 *
 * `role-change` collapses to a single key on purpose: `classify` discards the
 * `GroupContextExtensions` payload, so a received role-change request is only
 * describable at group level (UX §11.5) and two of them are indistinguishable.
 */
export function proposalKey(record: ProposalRecordView): string {
  switch (record.kind) {
    case "add":
      return `add:${record.addSignatureKey === undefined ? "?" : b64(record.addSignatureKey)}`;
    case "remove":
      return `remove:${record.removeTargetLeaf ?? "?"}`;
    case "leave":
      return `leave:${record.senderLeaf ?? "?"}`;
    case "role-change":
      return "role-change";
    case "update":
      return `update:${record.senderLeaf ?? "?"}`;
    default:
      return `unsupported:${record.senderLeaf ?? "?"}`;
  }
}

/**
 * The quiet-window rule.
 *
 * An UNOBSERVED request (no DS timestamp — restored from a snapshot rather than
 * seen to land) renders as `waiting` immediately. That is the deliberate
 * direction: it has necessarily been pending since before this session started,
 * so it is by definition past any window, and treating it as `pending` would
 * hide a genuinely old request behind a timer that never fires.
 */
export function requestState(
  observedAtMs: number | undefined,
  nowMs: number,
  quietWindowMs: number = QUIET_WINDOW_MS,
): "pending" | "waiting" {
  if (observedAtMs === undefined) return "waiting";
  return nowMs - observedAtMs >= quietWindowMs ? "waiting" : "pending";
}

/** One recorded observation of a proposal landing. */
export interface RequestObservation {
  /** DS row timestamp, ms since epoch. */
  readonly atMs: number;
  /** `true` when this device published the request. */
  readonly own: boolean;
  /**
   * ADR-0045 UX §6.1.10 — see {@link PendingRequest.transferToB64}.
   *
   * It lives HERE, on the observation record, rather than in a store of its
   * own, and that is a correctness choice rather than tidiness: this store is
   * pruned against the core's live pending set on every fold
   * ({@link pruneObservations}), so a swept or superseded transfer takes its
   * successor key with it. A separate store would have to re-derive that prune,
   * and a stale key surviving it would make an unrelated LATER role change
   * render `Waiting to make {name} the owner.` about somebody nobody proposed.
   */
  readonly transferToB64?: string;
  /**
   * ADR-0045 S7 residual 1 — `true` when OUR OWN `add` request was published by
   * approving an external **join** rather than by adding a contact.
   *
   * It lives here for the same reason `transferToB64` does: this store is
   * pruned against the core's live pending set, so the flag dies with the
   * request it describes. It is knowable only on the device that made the
   * request, because on the wire an approved add is indistinguishable from an
   * admin adding a contact (ADR-0045 S7) — and that device is the one
   * `chat/unapplied-requests.ts` shows the notice on.
   */
  readonly joinRequest?: boolean;
}

/**
 * Join the core's live pending set to the observation store.
 *
 * A record with `stored === false` is EXCLUDED: the receipt gate refused it, so
 * no sweep will ever commit it and it is not a pending request. That is a
 * refusal reported as data rather than as a throw, because a proposal advances
 * no state — dropping it unstored *is* the refusal.
 *
 * Returns the requests in the core's own order plus the keys that are still
 * live, so the caller can prune the store in the same pass.
 */
export function foldPendingRequests(
  records: readonly ProposalRecordView[],
  observations: ReadonlyMap<string, RequestObservation>,
  nowMs: number,
  quietWindowMs: number = QUIET_WINDOW_MS,
): { readonly requests: PendingRequest[]; readonly liveKeys: string[] } {
  const requests: PendingRequest[] = [];
  const liveKeys: string[] = [];
  for (const record of records) {
    if (!record.stored) continue;
    const key = proposalKey(record);
    liveKeys.push(key);
    const seen = observations.get(key);
    requests.push({
      key,
      kind: normaliseKind(record.kind),
      addSignatureKeyB64:
        record.addSignatureKey === undefined ? undefined : b64(record.addSignatureKey),
      // A `leave` is RFC 9420 `SelfRemove`, which names NO target on the wire:
      // the core maps `ProposalKind::Leave` to `(kind, None, None)`, and
      // `commit_effects` states the rule — "`SelfRemove` names no target: the
      // removed leaf is the sender's own." So the removed leaf is resolved here
      // from the sender, exactly as the core resolves it, and reading
      // `removeTargetLeaf` straight through would leave every leave with
      // `targetLeaf === undefined` — which is what made `rowAnnotation`'s
      // `Waiting to leave.` branch unreachable.
      //
      // This is the same leaf {@link proposalKey} already keys a leave on, so
      // the two agree rather than carrying two notions of which leaf a leave
      // concerns. An UNRESOLVABLE sender stays `undefined` and must not
      // collapse onto a real leaf — the core refuses such a proposal anyway,
      // and defaulting it to `0` would annotate the owner's row for a request
      // nobody made.
      targetLeaf: record.kind === "leave" ? record.senderLeaf : record.removeTargetLeaf,
      senderLeaf: record.senderLeaf,
      observedAtMs: seen?.atMs,
      own: seen?.own ?? false,
      // Only meaningful on our own record, and only for a role change — which
      // is exactly where it is ever written.
      transferToB64: seen?.transferToB64,
      state: requestState(seen?.atMs, nowMs, quietWindowMs),
    });
  }
  return { requests, liveKeys };
}

/**
 * The panel's visibility gate (UX §10).
 *
 * > The pending set is visible to the owner and admins. Every member
 * > additionally sees the status of a request they made themselves. No member
 * > is ever told about a pending change to another member's status unless they
 * > can act on it.
 *
 * Argued rather than laid out: the pending set is a work queue and only
 * owner/admins have a verb over it. Showing a plain member "Bob is waiting to
 * be removed" tells them something they cannot act on, about someone else's
 * standing, in a window that did not exist before ADR-0045 created it — and in
 * a group where a removal is a safety action, advertising it is a hazard.
 *
 * Note what this is NOT: it is not concealment from the target. The receipt
 * gate stores the proposal on **every** member's device including theirs, so a
 * determined target can see it regardless. The choice is only whether to build
 * a surface that broadcasts it. Not rendering something asserts nothing, so it
 * breaks no honesty rule, and it matches what the panel already does — `Add
 * member`, `Remove`, the invite block and `Join requests` are all role-gated
 * today.
 *
 * The own-request exception is non-negotiable: without it `Leave group` is a
 * button with no feedback.
 */
export function visibleRequests(
  requests: readonly PendingRequest[],
  canManageMembers: boolean,
): PendingRequest[] {
  if (canManageMembers) return [...requests];
  return requests.filter((r) => r.own);
}

/** The exact §5.1 pending-notice copy, or `null` when nothing is `waiting`. */
export function pendingNotice(
  requests: readonly PendingRequest[],
): { readonly count: string; readonly liveness: string } | null {
  const waiting = requests.filter((r) => r.state === "waiting").length;
  if (waiting === 0) return null;
  return {
    count: t("chat.members.pending.count", { count: waiting }),
    // ONE line, not two. The escalated variant went with the `stalled` state:
    // ADR-0045 decision 2 gives a waiting request no deadline, so there is no
    // moment at which the client learns something new to say. This sentence is
    // true at ten seconds and at ten days, it states INACTION (the only thing
    // the client can observe), it names no person, and it blames nobody.
    liveness: t("chat.members.pending.liveness"),
  };
}

/**
 * ADR-0045 S7 residual 2 — the queue an approve just grew, named to the person
 * who grew it.
 *
 * ## Why this line, and why only this line
 *
 * Approving a join no longer admits anyone: it publishes an `Add` proposal into
 * the member pending set, and that set is drained only by a sweep, which
 * ADR-0045 decision 2 gives no deadline. So an admin can approve into a queue
 * indefinitely, and before this the only feedback in the whole system was the
 * core's cap refusal — which lands on **whoever proposes next**, who is very
 * often somebody else entirely.
 *
 * This does not bound anything and does not pretend to. Bounding needs a number
 * this layer does not have: `MAX_PENDING_PROPOSALS` is `pub(crate)` in
 * `lakko-mls` and is on neither FFI surface, and a client-side literal `64`
 * would be exactly the stale-literal drift class this repo already names as
 * failing in the dangerous direction. What it does is make the accumulation
 * *visible at the moment it is caused*, which is the part a client can do
 * honestly.
 *
 * `null` below two, because "1 change is now waiting" after making one change
 * is noise rather than information.
 */
export function waitingQueueLine(pendingCount: number): string | null {
  if (pendingCount < 2) return null;
  return t("chat.members.pending.queue", { count: pendingCount });
}

// The "Join requests are not saved" note that used to live here is gone with
// the reason for it: since 2026-09-26 `Group::pending_external_joins` rides the
// MLS storage snapshot (ADR-0027 update note), so a reload shows the same list.
// Rows are still never aged out, which would hide something still live for
// the joiner.

/**
 * The row annotation for a member, or `null` when that member has no pending
 * request against them.
 *
 * `Waiting to leave.` when the request targets the reader's own leaf — the
 * self-remove that is how leaving works — otherwise `Waiting to be removed.`
 * A pending role change deliberately gets no row annotation: it carries no
 * target, so it cannot be attributed to a member (UX §11.5); it contributes to
 * the notice count and shows only at panel level.
 */
export function rowAnnotation(
  requests: readonly PendingRequest[],
  leafIndex: number,
  ownLeaf: number | undefined,
): string | null {
  const hit = requests.find(
    (r) => r.state === "waiting" && (r.kind === "remove" || r.kind === "leave") && r.targetLeaf === leafIndex,
  );
  if (hit === undefined) return null;
  return leafIndex === ownLeaf
    ? t("chat.members.pending.waiting_leave")
    : t("chat.members.pending.waiting_removed");
}

/** The `Waiting to be added ({n})` rows — pending adds only, past the window. */
export function waitingToBeAdded(
  requests: readonly PendingRequest[],
): PendingRequest[] {
  return requests.filter((r) => r.kind === "add" && r.state === "waiting");
}

// ---------------------------------------------------------------------------
// The observation store
// ---------------------------------------------------------------------------

const STORE_PREFIX = "lakko.pending-requests.v1";

function storeKey(accountIdB64: string, groupIdB64: string): string {
  return `${STORE_PREFIX}.${accountIdB64}.${groupIdB64}`;
}

/**
 * Read the observations for one group.
 *
 * Never throws: a corrupt or absent row reads as empty, which degrades the
 * quiet window to "render immediately" rather than losing the request. The
 * request itself lives in the core's pending set, so nothing here is
 * load-bearing for correctness — only for *when* an annotation appears.
 */
export function loadObservations(
  accountIdB64: string,
  groupIdB64: string,
): Map<string, RequestObservation> {
  try {
    const raw = globalThis.localStorage?.getItem(storeKey(accountIdB64, groupIdB64));
    if (raw === null || raw === undefined) return new Map();
    const parsed = JSON.parse(raw) as Record<string, RequestObservation>;
    const out = new Map<string, RequestObservation>();
    for (const [k, v] of Object.entries(parsed)) {
      if (typeof v?.atMs === "number" && typeof v?.own === "boolean") {
        out.set(k, {
          atMs: v.atMs,
          own: v.own,
          // ADR-0045 §6.1.10 — preserved through the round-trip, and validated
          // the same way as the rest: anything that is not a string is simply
          // absent, which degrades a pending transfer to its unnamed variant
          // rather than losing the request.
          ...(typeof v.transferToB64 === "string"
            ? { transferToB64: v.transferToB64 }
            : {}),
          // Same validation shape, same degrade-rather-than-lose direction: an
          // absent flag reads as an ordinary add, which is the copy that offers
          // the weaker action rather than the stronger claim.
          ...(v.joinRequest === true ? { joinRequest: true } : {}),
        });
      }
    }
    return out;
  } catch {
    return new Map();
  }
}

/** Write the observations for one group. Never throws (a quota failure only
 *  costs the quiet window, not the request). */
function saveObservations(
  accountIdB64: string,
  groupIdB64: string,
  observations: ReadonlyMap<string, RequestObservation>,
): void {
  try {
    const obj: Record<string, RequestObservation> = {};
    for (const [k, v] of observations) obj[k] = v;
    globalThis.localStorage?.setItem(
      storeKey(accountIdB64, groupIdB64),
      JSON.stringify(obj),
    );
  } catch {
    /* the quiet window is best-effort */
  }
}

/**
 * Record that a proposal landed, keyed on {@link proposalKey}.
 *
 * The FIRST observation of a key wins: a re-broadcast or a re-request of the
 * same request must not reset its age, or a request that keeps being re-sent
 * would never mature past the quiet window and would stay invisible forever —
 * exactly the wait the notice exists to surface. `own` is upgraded to `true`
 * if this device later publishes the same request, because the own-request
 * visibility exception must not be lost to arrival order.
 *
 * `transferToB64` (ADR-0045 §6.1.10) follows the same first-wins rule for the
 * same reason: the successor belongs to the request that is actually pending,
 * and only the device that published it can supply one. A later observation of
 * the same key without one must not erase it — the receive loop observes its
 * own request landing back off the journal, and that observation carries no
 * successor.
 */
export function recordObservation(
  accountIdB64: string,
  groupIdB64: string,
  key: string,
  atMs: number,
  own: boolean,
  transferToB64?: string,
  joinRequest?: boolean,
): void {
  const observations = loadObservations(accountIdB64, groupIdB64);
  const existing = observations.get(key);
  const transferTo = existing?.transferToB64 ?? transferToB64;
  // Sticky-true, for the same reason `own` is: the receive loop observes our
  // own approval landing back off the journal and that observation knows
  // nothing about where the request came from. Losing the flag to arrival order
  // would silently downgrade the copy to the wrong action.
  const fromJoin = (existing?.joinRequest ?? false) || joinRequest === true;
  observations.set(key, {
    atMs: existing?.atMs ?? atMs,
    own: (existing?.own ?? false) || own,
    ...(transferTo === undefined ? {} : { transferToB64: transferTo }),
    ...(fromJoin ? { joinRequest: true } : {}),
  });
  saveObservations(accountIdB64, groupIdB64, observations);
}

/** Drop every observation whose request is no longer in the core's pending set
 *  — the request was swept, superseded, or the group moved. */
export function pruneObservations(
  accountIdB64: string,
  groupIdB64: string,
  liveKeys: readonly string[],
): void {
  const observations = loadObservations(accountIdB64, groupIdB64);
  const live = new Set(liveKeys);
  let changed = false;
  for (const key of [...observations.keys()]) {
    if (!live.has(key)) {
      observations.delete(key);
      changed = true;
    }
  }
  if (changed) saveObservations(accountIdB64, groupIdB64, observations);
}

/** Drop every observation for one account — `recover` / `forgetDevice` hygiene,
 *  the same discipline the commit outbox follows. */
export function clearObservationsForAccount(accountIdB64: string): void {
  try {
    const store = globalThis.localStorage;
    if (store === undefined || store === null) return;
    const victims: string[] = [];
    for (let i = 0; i < store.length; i++) {
      const k = store.key(i);
      if (k !== null && k.startsWith(`${STORE_PREFIX}.${accountIdB64}.`)) victims.push(k);
    }
    for (const k of victims) store.removeItem(k);
  } catch {
    /* best-effort */
  }
}

// ---------------------------------------------------------------------------

function normaliseKind(kind: string): PendingRequestKind {
  switch (kind) {
    case "add":
    case "remove":
    case "leave":
    case "update":
    case "role-change":
      return kind;
    default:
      return "unsupported";
  }
}

/** Standard base64 of raw bytes — the same encoding `auth/account-id.ts` uses,
 *  reimplemented here so this module stays importable from a test with no app
 *  wiring. */
function b64(bytes: Uint8Array): string {
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s);
}

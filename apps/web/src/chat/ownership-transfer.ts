// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0045 — ownership transfer, the owner's way out
 * ([`docs/ux/proposal-based-membership.md`] §6.1).
 *
 * ## What this is for
 *
 * The owner is the only person in a Lakko group who cannot leave it. The core
 * refuses an owner's self-removal, and it is right to: a role change is
 * owner-only (`set_group_roles` / `propose_group_roles`), so an ownerless group
 * could never appoint another admin. Transfer is the single thing that turns
 * that dead end into a **precondition** — hand the owner slot to someone else,
 * then leave as an ordinary member.
 *
 * The capability has existed in `lakko-mls` since `e780952` and on both FFI
 * surfaces since `6c77cd6`. Until this module, no client called it.
 *
 * ## Why this is a plain module and not part of `routes/Chat.tsx`
 *
 * `vitest.config.ts` collects `src/**\/*.test.ts` in a plain `node` environment
 * with no jsdom and no Solid JSX transform, so a `.tsx` component cannot be
 * tested at all. Every rule §6.1 states therefore lives here, where it is
 * executable, and `Chat.tsx` renders what this module decides — the same split
 * `pending-requests.ts` and `committer-beacon.ts` already use.
 *
 * ## The one framing sentence
 *
 * Transfer is the **single irreversible action in the membership surface**.
 * `Add`, `Remove`, `Leave` and an ordinary role change can all be asked for
 * again; this one cannot be asked back. Every rule below is downstream of that.
 *
 * ## The proposal path, and why there is only one arm
 *
 * Both APIs exist on the wasm surface (`transferOwnership`,
 * `proposeOwnershipTransfer`) and this flow drives **only the proposal one**.
 * That is not a preference — the direct arm is *structurally unreachable here*.
 * The transfer control is gated on `isOwner`, which is read off the role table
 * (`Group.ownRole` → `group_roles()`); `usesProposalPath` is *"the group has a
 * role table"* (`Group.roleTableOwner` → the identical `group_roles()` read).
 * A group with no role table has no owner, so `isOwner && !usesProposalPath`
 * cannot hold, and the control never renders in a group that would take the
 * direct path. Transfer is the one membership action that needs a single arm
 * rather than two, unlike add / remove / role change, each of which carries a
 * direct fallback for the table-less case.
 *
 * Even if it were reachable, the proposal path would still win: the direct path
 * delegates to `set_group_roles`, which is refused while **anything** is
 * pending (`refuse_if_proposals_pending`), where the proposal path is refused
 * only by another *role change*; and a direct commit that loses a race leaves
 * its eagerly-merged author holding a role table in which they are not the
 * owner, with no owner-only path back.
 */

// ---------------------------------------------------------------------------
// Copy (§6.1) — one definition each, so the component cannot drift from it
// ---------------------------------------------------------------------------

/** §6.1.1 — the owner's disabled `Leave group`. The reason names the action
 *  sitting directly above it, which is what makes this a signposted route
 *  rather than a control that always refuses. */
export const LEAVE_BLOCKED_BY_OWNERSHIP =
  "Transfer ownership first — a group cannot be left without an owner.";

/** §6.1.3 — the honest terminal state of a one-person group. There is no design
 *  that produces a successor out of an empty roster, and the owner of a
 *  one-person group is not being kept anywhere against their will. */
export const NO_TRANSFER_CANDIDATES =
  "There is no one else in this group to transfer it to.";

/**
 * §6's SEVENTH refusal string, reused verbatim in both C-65 directions — a
 * pending role change refuses a transfer, and a pending transfer refuses a role
 * change. §6.1.8 argues the table stays at seven rather than gaining an eighth.
 *
 * It is the one **availability** refusal in that table, which is why it is the
 * one string that ends by telling the user to try again; every other row is an
 * **authorisation** refusal and states a fact with no invitation to retry.
 *
 * It is under-informative in the transfer-blocked-a-role-change direction (the
 * owner knows they asked for a transfer and is told "a role change"), and that
 * is deliberate: `ProposalKind::RoleChange` carries no payload, so a client
 * cannot in general tell the two apart, and the blocking proposal may be one
 * this device **no longer remembers** after the ADR-0040 crash window. Telling
 * a user they did something they have no memory of doing is worse than stating
 * the group's state neutrally.
 */
export const ROLE_CHANGE_ALREADY_PENDING =
  "A role change is already waiting to apply. Wait for it to land, then try again.";

/** §6.1.5 — the headline of the irreversibility statement. It sits **above**
 *  the role choice: below it, the pre-selected option reads as the safe one and
 *  the warning reads as being about the *other* one. */
export const TRANSFER_IRREVERSIBLE_HEADLINE = "This cannot be undone.";

/**
 * §6.1.5 — three specific losses rather than an adjective, because all three
 * clauses are literally true. Rewriting the role table is owner-only and the
 * owner slot lives in that table, so after the commit the transferor is not the
 * owner, cannot rewrite the table, and therefore cannot restore the slot,
 * cannot take the group back, and cannot change even their **own** role without
 * the new owner doing it for them.
 */
export const TRANSFER_IRREVERSIBLE_BODY =
  "Only an owner can change roles. Once you transfer this group you cannot " +
  "take it back, you cannot restore your own owner role, and you cannot " +
  "change your own role again unless the new owner does it for you.";

/** §6.1.5 — the trailing line. Note **applied**, not *now*: the transfer is a
 *  proposal, and asserting the exit is open before the commit lands would be
 *  the optimistic update §3 forbids. */
export const TRANSFER_UNLOCKS_LEAVE =
  "Once the change is applied, you can leave the group like any other member.";

/** §6.1.9 — the second line of the new-owner notice. It names the exit
 *  deliberately: it is the one sentence that stops the new owner from arriving
 *  where the old one was. */
export const NEW_OWNER_BODY =
  "You can add and remove members, change admins, and transfer ownership to someone else.";

/** §6.1.9 — the notice's dismiss control. */
export const NEW_OWNER_DISMISS = "Dismiss";

// ---------------------------------------------------------------------------
// §6.1.1 — the exit block
// ---------------------------------------------------------------------------

/** What the exit block knows about a role change already waiting to apply.
 *
 *  `null` means *nothing is waiting* — which includes a request that exists but
 *  has not matured past the quiet window. That is the caller's `state ===
 *  "waiting"` filter arriving here as an absence, and §6.1.6 requires it: below
 *  the window the control stays enabled and a second tap is refused, because a
 *  request that is not yet rendered must not silently disable a control with
 *  nothing to explain it. */
export interface WaitingRoleChange {
  /**
   * `true` when this device published it AND it is a **transfer** — i.e. our
   * own observation record carried a successor key (§6.1.10).
   *
   * The narrower predicate is deliberate. UX §10's own-request exception is
   * about a request *we* made, but the copy it selects here says *ownership
   * transfer*, and our own pending role change may perfectly well be an
   * ordinary promote: `ProposalKind::RoleChange` carries no payload, so the
   * successor key on our own record is the only thing that tells the two apart.
   * A promote that reported itself as `Ownership transfer waiting to apply.`
   * would be a plain falsehood, so a promote falls through to the seventh
   * string, which is true of it.
   */
  readonly ownTransfer: boolean;
  /** The successor's label, resolved through the LIVE roster at render time,
   *  never from a stored label. `null` when the successor can no longer be
   *  resolved (they left in the meantime). */
  readonly successorLabel: string | null;
}

/** Everything the exit block's decision depends on. */
export interface ExitBlockInput {
  /** ADR-0045 — does this group route membership through proposals? A group
   *  with no role table has no owner and no self-remove route, so it gets no
   *  exit block at all. */
  readonly usesProposalPath: boolean;
  /** ADR-0022 — is this device the role table's owner? */
  readonly isOwner: boolean;
  /** `false` for a torn roster read: without our own leaf we cannot name
   *  ourselves in a self-remove, so `Leave group` has nothing to act on. */
  readonly ownLeafKnown: boolean;
  /** How many members this group has other than the owner (§6.1.3). */
  readonly candidateCount: number;
  /** See {@link WaitingRoleChange}. */
  readonly waitingRoleChange: WaitingRoleChange | null;
  /** `true` when our own departure is already waiting to apply. */
  readonly ownRemovalPending: boolean;
}

/** A control that may be rendered disabled, carrying the reason why. */
export interface GuardedControl {
  readonly enabled: boolean;
  /** `null` exactly when {@link GuardedControl.enabled} is `true`. */
  readonly disabledReason: string | null;
}

/**
 * What the exit block renders for this reader.
 *
 * The two controls are **role-disjoint**, so exactly one of them is ever live:
 * `Transfer ownership` is owner-only, and `Leave group` is refused for the
 * owner by the core. That is why they belong in one block at the panel's bottom
 * whose meaning is *the exit*, rather than in two places.
 *
 * The `member` arm carries no `transfer` field at all. A non-owner's transfer is
 * refused by the core and by every peer, and nothing a member could do would
 * make it work — that is the case where *a control that always refuses is worse
 * than no control* still holds, and it is worth naming, because it is the same
 * sentence the owner's `Leave group` no longer satisfies.
 */
export type ExitBlock =
  | { readonly kind: "hidden" }
  | {
      readonly kind: "owner";
      readonly transfer: GuardedControl;
      readonly leave: GuardedControl;
    }
  | { readonly kind: "member"; readonly leave: GuardedControl };

/**
 * Decide the exit block (§6.1.1).
 *
 * **This reverses §6's "hide `Leave group` from the owner" decision, and not
 * because that argument was wrong.** It was right *while transfer was
 * unreachable*: a control that always refuses is worse than no control. It
 * stops being right the moment the precondition is reachable and adjacent. A
 * disabled control whose stated reason names an action sitting directly above
 * it is not a control that always refuses — it is a signposted route, and it is
 * the same visible-but-disabled-with-an-explanation pattern §5.3 already
 * establishes for a pending row's `Remove`. The defect the reversal fixes is
 * concrete: before it, an owner who wanted to leave opened the members panel
 * and found **nothing at all about leaving**.
 *
 * ## Precedence among the transfer control's disabled reasons
 *
 * Three conditions can each disable it and more than one can hold at once, so
 * the order is decided rather than incidental:
 *
 * 1. **Our own waiting transfer** wins outright. It is UX §10's non-negotiable
 *    own-request exception — a member must always see the status of a request
 *    they made — and it is the most specific thing this device knows.
 * 2. **No candidates** beats a waiting role change, because it is the only
 *    **permanent** condition of the three. §6 draws the distinction that
 *    decides this: an authorisation refusal is a statement of fact with no
 *    invitation to retry, an availability refusal ends by telling the user to
 *    try again. Showing a retry-inviting string over a roster that can never
 *    produce a successor would send the user round a loop that cannot
 *    terminate.
 * 3. **Someone else's waiting role change** is last, and is the transient case
 *    the seventh string was written for.
 */
export function exitBlock(input: ExitBlockInput): ExitBlock {
  if (!input.usesProposalPath || !input.ownLeafKnown) return { kind: "hidden" };

  if (!input.isOwner) {
    return {
      kind: "member",
      leave: input.ownRemovalPending
        ? // §5.2's existing row annotation, reused rather than a new string:
          // the member's own row already carries it, and the control's reason
          // should say the same thing the roster says.
          { enabled: false, disabledReason: "Waiting to leave." }
        : { enabled: true, disabledReason: null },
    };
  }

  return {
    kind: "owner",
    transfer: transferControl(input),
    // Present and disabled, never hidden — see the doc comment above.
    leave: { enabled: false, disabledReason: LEAVE_BLOCKED_BY_OWNERSHIP },
  };
}

function transferControl(input: ExitBlockInput): GuardedControl {
  const waiting = input.waitingRoleChange;
  if (waiting !== null && waiting.ownTransfer) {
    return {
      enabled: false,
      disabledReason: transferPendingLabel(waiting.successorLabel),
    };
  }
  if (input.candidateCount === 0) {
    return { enabled: false, disabledReason: NO_TRANSFER_CANDIDATES };
  }
  if (waiting !== null) {
    return { enabled: false, disabledReason: ROLE_CHANGE_ALREADY_PENDING };
  }
  return { enabled: true, disabledReason: null };
}

// ---------------------------------------------------------------------------
// §6.1.3 — the picker
// ---------------------------------------------------------------------------

/** One roster row, as this module reads it. `label` is the roster's OWN
 *  identity treatment — an accepted contact's display name when we have one,
 *  else the short fingerprint — passed in rather than re-derived, so the picker
 *  and the members list cannot render the same person differently. */
export interface RosterEntryView {
  readonly leafIndex: number;
  /** Standard base64 of the member's 32-byte Ed25519 signature public key. */
  readonly signatureKeyB64: string;
  readonly label: string;
}

/** A picker row. */
export interface TransferCandidate extends RosterEntryView {
  /** Renders a muted `Admin` marker. Information the owner assigned and worth
   *  having when choosing a successor — never a reason to hide anyone. */
  readonly isAdmin: boolean;
}

/**
 * Every member except the current owner, in the roster's own order (§6.1.3).
 *
 * **Do not restrict to admins.** The core admits any member
 * (`plan_ownership_transfer` refuses only a non-member and the current owner;
 * `owner_survives` / C-63 enforces the same on every peer), so an admins-only
 * picker would be a rule the protocol does not have — and a narrower rule owes
 * a reason. The candidate reason, *an admin is a proven-trusted successor*,
 * does not survive contact with the commonest small group: **owner plus one
 * member, no admins ever appointed**. There the admins-only picker is empty and
 * the owner is stuck again. That is the dead end this whole feature exists to
 * close, reappearing as a UI rule, and it settles the question.
 *
 * **Mark, do not filter**, and do not sort: hoisting admins to the top would be
 * a soft version of the filter this function rejects. An owner who wants an
 * admin successor can promote first.
 *
 * **Keyed on the 32-byte signature key, never the leaf index** — that is what
 * `proposeOwnershipTransfer` takes and what the role table keys on, and OpenMLS
 * hands a vacated leaf to the next Add, so a leaf-keyed selection can silently
 * come to name someone else.
 */
export function transferCandidates(
  roster: readonly RosterEntryView[],
  ownerKeyB64: string | undefined,
  adminKeysB64: ReadonlySet<string>,
): TransferCandidate[] {
  return roster
    .filter((m) => m.signatureKeyB64 !== ownerKeyB64)
    .map((m) => ({ ...m, isAdmin: adminKeysB64.has(m.signatureKeyB64) }));
}

// ---------------------------------------------------------------------------
// §6.1.4 — the outgoing-owner choice
// ---------------------------------------------------------------------------

/** Which `OutgoingOwner` variant the owner picked. Maps onto the wasm enum at
 *  the call site; kept as a string here so this module needs no wasm import and
 *  stays testable in the plain `node` environment. */
export type OutgoingOwnerChoice = "admin" | "member";

/** One radio in the outgoing-owner choice. */
export interface OutgoingOwnerOption {
  readonly value: OutgoingOwnerChoice;
  readonly label: string;
  /** Each option states its own consequence. */
  readonly consequence: string;
  readonly preSelected: boolean;
}

/**
 * The two options, in order (§6.1.4).
 *
 * **A single-select control, never a checkbox** — a checkbox has an unstated
 * default state and this has none. The core refuses to default the choice and
 * says why: *either silent choice is wrong in one direction*. Silently keeping
 * admin leaves an unwanted administrator in a group the outgoing owner meant to
 * walk away from; silently dropping to plain member strips them, in one
 * irreversible commit, of every power they had in a group they created. Both
 * FFI surfaces carry that property structurally — no `Default` derive on
 * uniffi, discriminants starting at `1` on wasm so a missing argument throws
 * rather than silently meaning `Admin`. The UI must not undo with a layout
 * choice what the core protected with a type.
 *
 * `Stay an admin` is **pre-selected** for the core's stated reason and **not**
 * because it preserves a way back — it does not, and no copy here may imply
 * otherwise. It is the recoverable side of an unrecoverable action: shedding
 * admin later is one ordinary role change the new owner can make, where the
 * reverse mistake needs the same cooperation *and* costs the group an
 * administrator in the meantime, at exactly the moment its leadership is
 * changing hands.
 */
export const OUTGOING_OWNER_OPTIONS: readonly OutgoingOwnerOption[] = [
  {
    value: "admin",
    label: "Stay an admin",
    consequence:
      "You can still add and remove members. You can give this up later by asking the new owner.",
    preSelected: true,
  },
  {
    value: "member",
    label: "Become a regular member",
    consequence:
      "You give up every group power immediately. Only the new owner can give any of it back.",
    preSelected: false,
  },
];

/** The pre-selected choice. Named *pre-selected* rather than *default* on
 *  purpose: the core has no default and this must not read as one. */
export const PRESELECTED_OUTGOING_OWNER: OutgoingOwnerChoice = "admin";

// ---------------------------------------------------------------------------
// §6.1.5 — the action button
// ---------------------------------------------------------------------------

/**
 * The transfer step's primary action (§6.1.5).
 *
 * **There is no second dialog, and that is the design rather than a shortcut.**
 * The transfer step is not a form that then confirms; it **is** the
 * confirmation, which happens to require two choices to be well-formed.
 * Stacking an `Are you sure?` on a modal that already asked two questions buys
 * friction of the worst kind — the kind that gets dismissed reflexively. What
 * replaces it is friction that cannot be discharged without reading: the button
 * **names the person**, it is **disabled until one is selected** so there is no
 * default successor and no path to the action that skips the picker, and it
 * uses the picked row's label **verbatim** — not a re-formatted or re-shortened
 * variant — because that is the string the user cross-checks against the row
 * they tapped.
 *
 * The unselected label is the control's own name, which asserts nothing. §6.1
 * does not specify it (it specifies only that the action is disabled until a
 * successor is picked), so this is the smallest thing that can sit there.
 */
export function transferActionLabel(selected: TransferCandidate | undefined): {
  readonly label: string;
  readonly enabled: boolean;
} {
  if (selected === undefined) {
    return { label: "Transfer ownership", enabled: false };
  }
  return { label: `Make ${selected.label} the owner`, enabled: true };
}

// ---------------------------------------------------------------------------
// §6.1.6 / §6.1.7 — pending and outcome copy
// ---------------------------------------------------------------------------

/**
 * What the transfer control says while our own transfer is waiting (§6.1.6).
 *
 * `successorLabel` is resolved through the LIVE roster at render time, never
 * from a stored label — the rule `PendingRequests.kt` already states for a
 * pending removal's target, and the reason the durable record keeps the
 * successor's *key* rather than their name.
 *
 * Note what does **not** happen: no row annotation. A role change carries no
 * target (§5.2, §11.5), so only the device that made the request could render
 * one — it would appear on that device and not on the other owner/admin devices
 * reading the same pending set, i.e. a per-device fact rendered in the place the
 * panel reserves for group-level ones. This line, on the control the user
 * touched, is where UX §10's own-request exception is satisfied instead.
 */
export function transferPendingLabel(successorLabel: string | null): string {
  return successorLabel === null
    ? "Ownership transfer waiting to apply."
    : `Waiting to make ${successorLabel} the owner.`;
}

/**
 * The outcome notice (§6.1.7).
 *
 * The two swept forms name the transferor's **own new role**, because half the
 * panel changes under them in one tap: an owner who chose `Become a regular
 * member` loses the add form, the invite block, every `Remove` and every admin
 * toggle at once. §5.3's reasoning about a vanishing control applies with more
 * force to a vanishing section.
 *
 * The published-but-unswept form names **no** role, because nothing has changed
 * yet; appending "you are now an admin" to a request that has not landed would
 * be the optimistic-update lie in its purest form.
 */
export function transferOutcomeNotice(
  outcome: "committed" | "proposed",
  successorLabel: string,
  outgoing: OutgoingOwnerChoice,
): string {
  if (outcome === "proposed") {
    return "Ownership transfer requested. It applies when an owner or an admin applies the change.";
  }
  return outgoing === "admin"
    ? `${successorLabel} is now the group owner. You are still an admin.`
    : `${successorLabel} is now the group owner. You are now a regular member.`;
}

// ---------------------------------------------------------------------------
// §6.1.9 — telling the new owner
// ---------------------------------------------------------------------------

/** The new-owner notice: rendered once, dismissible, at panel level. */
export interface NewOwnerNotice {
  readonly headline: string;
  readonly body: string;
  readonly dismiss: string;
}

/**
 * Tell the new owner they are the new owner (§6.1.9) — best-effort, fail-open.
 *
 * **This is a judgement rather than a finding, and its cost is stated.** §8
 * argues *against* telling someone they hold the committer duty, because it
 * "cannot be declined, cannot be delegated and cannot be discharged by any
 * action" — anxiety without agency. That argument does not transfer here, and
 * the difference is the whole reason: **ownership can be discharged**, by
 * exactly the flow this module implements. A duty with an exit is one the
 * holder needs to know they hold, or the exit is unreachable for the same
 * reason it was unreachable before ADR-0045's UX pass.
 *
 * **What it costs.** Nothing on the wire announces a transfer; the role table
 * simply changes and every other device learns of it by re-reading. So this is
 * a **client-side inference from an observed role-table change, not a
 * message**. It needs one durably remembered key per group, and it is
 * structurally unable to fire on a device that first observes the table already
 * naming it owner — a fresh device, a restore, a first sync. Hence **fail-open**
 * everywhere: an absent or unchanged previous owner says nothing rather than
 * claiming a promotion it did not witness. The notice must never be the only way
 * a user learns their role; the panel's affordances stay the primary signal,
 * exactly as they are for admin today.
 *
 * Naming the previous owner is sound rather than a guess: a role change is
 * owner-only and `owner_survives` (C-63) forbids naming a non-member, so the
 * previous owner did author it.
 */
export function newOwnerNotice(
  lastObservedOwnerB64: string | undefined,
  currentOwnerB64: string | undefined,
  ownSignatureKeyB64: string | undefined,
  previousOwnerLabel: string | null,
): NewOwnerNotice | null {
  // Fail-open: a first observation is not a change.
  if (lastObservedOwnerB64 === undefined) return null;
  // No role table, or a torn read of our own key — nothing to claim.
  if (currentOwnerB64 === undefined || ownSignatureKeyB64 === undefined) return null;
  // Not a change.
  if (lastObservedOwnerB64 === currentOwnerB64) return null;
  // A change, but not one that makes US the owner. This is the arm the
  // transferor takes: their last observed owner was themselves and the table now
  // names someone else.
  if (currentOwnerB64 !== ownSignatureKeyB64) return null;
  return {
    headline:
      previousOwnerLabel === null
        ? "You are now the owner of this group."
        : `${previousOwnerLabel} made you the owner of this group.`,
    body: NEW_OWNER_BODY,
    dismiss: NEW_OWNER_DISMISS,
  };
}

// ---------------------------------------------------------------------------
// §6.1.10 — the last-observed-owner store
// ---------------------------------------------------------------------------

/**
 * Per-`(account, group)` durable memory of the owner this device last saw.
 *
 * This is the second of §6.1.10's two local additions (the first, `transferTo`,
 * rides on the pending-request observation record in `pending-requests.ts`,
 * where it inherits that store's prune-against-the-live-set for free — a stale
 * successor key would otherwise make an unrelated later role change render
 * `Waiting to make {name} the owner.`).
 *
 * Nothing here is load-bearing for correctness: losing it costs one notice, not
 * a role. Every read and write is therefore best-effort and never throws.
 */
const OWNER_STORE_PREFIX = "lakko.last-observed-owner.v1";

function ownerStoreKey(accountIdB64: string, groupIdB64: string): string {
  return `${OWNER_STORE_PREFIX}.${accountIdB64}.${groupIdB64}`;
}

/** The owner signature key (base64) this device last observed for the group, or
 *  `undefined` when it never observed one — the fail-open case. */
export function loadLastObservedOwner(
  accountIdB64: string,
  groupIdB64: string,
): string | undefined {
  try {
    const raw = globalThis.localStorage?.getItem(
      ownerStoreKey(accountIdB64, groupIdB64),
    );
    return raw === null || raw === undefined || raw.length === 0 ? undefined : raw;
  } catch {
    return undefined;
  }
}

/** Record the owner this device is observing now. Called after the notice has
 *  been decided, so the decision always compares against the PREVIOUS reading. */
export function rememberObservedOwner(
  accountIdB64: string,
  groupIdB64: string,
  ownerKeyB64: string,
): void {
  try {
    globalThis.localStorage?.setItem(
      ownerStoreKey(accountIdB64, groupIdB64),
      ownerKeyB64,
    );
  } catch {
    /* best-effort — losing it costs one notice */
  }
}

/** Drop every remembered owner for one account — `recover` / `forgetDevice`
 *  hygiene, the same discipline the commit outbox, the pending-request
 *  observations and the beacon observations all follow. */
export function clearLastObservedOwnerForAccount(accountIdB64: string): void {
  try {
    const store = globalThis.localStorage;
    if (store === undefined || store === null) return;
    const victims: string[] = [];
    for (let i = 0; i < store.length; i++) {
      const k = store.key(i);
      if (k !== null && k.startsWith(`${OWNER_STORE_PREFIX}.${accountIdB64}.`)) {
        victims.push(k);
      }
    }
    for (const k of victims) store.removeItem(k);
  } catch {
    /* best-effort */
  }
}

// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0045 UX §6 / §12 item 6 — the typed membership-refusal table.
 *
 * ## What this is for
 *
 * §12 item 6 has been open since the spec was written: *"Prose-matching
 * `Error::Group` strings across three clients would work and would be wrong."*
 * The clients did the defensible thing and built no table at all, so a core
 * refusal reached the user as the core's own developer sentence — on web,
 * literally `formatError(err) === err.message`, which is how a member who
 * tried to add somebody was told
 * `add proposal refused: proposer is not owner or admin`.
 *
 * The core now carries a machine-readable {@link
 * https://docs.rs/lakko-mls `RefusalReason`} beside that sentence, and the
 * wasm surface attaches it to the thrown `Error` as two own properties —
 * `refusalReason` and `refusalDisposition`. This module is the client half:
 * reason in, sentence out. **Nothing here looks at a message.**
 *
 * ## Why the spec's seven-row table is not what got built
 *
 * §6 specifies seven strings and §12 item 6 asserts *"the count stays at
 * seven"*. The core has **eighteen** reasons. Nine of them the spec never
 * names, including `pendingSetFull`, which §7.1 does flag by name as needing
 * exactly this table. So the table is eleven strings, not seven, and the
 * difference is not padding — each addition is a refusal a user can reach by
 * tapping something.
 *
 * Ten reasons get a sentence of their own and eight share §6's fail-closed
 * row. The reasoning for each collapse is on the entry, and the rule behind
 * all of them is one thing: **a reason with no user-initiated entry point
 * gets no sentence**, because copy with no reader is copy that cannot be
 * checked, and §6 row 7 forbids leaking an internal reason outright.
 *
 * ## The retry affordance is the CORE's answer
 *
 * Whether waiting helps is asked of `membershipRefusalRetryMaySucceed` and
 * never restated here. That is not fastidiousness: the rule is narrow and
 * easy to get wrong in a way nothing catches. It is **not** the complement of
 * "permitted" — most non-permission refusals are still not retryable — and a
 * client that inferred it from a variant's name would send a user into a loop
 * on `proposalsPending` (which sounds transient and is not) or tell them they
 * may not do something they may on `pendingSetFull`.
 *
 * So the invitation to try again is **appended** rather than stored, and only
 * when the port says so. A stored sentence would agree with the core by
 * coincidence and keep agreeing after the core changed its mind.
 *
 * ## Why this is a plain module and not part of `routes/Chat.tsx`
 *
 * `vitest.config.ts` collects `src/**\/*.test.ts` in a plain `node`
 * environment with no jsdom and no Solid JSX transform, so a `.tsx` component
 * cannot be tested at all — the same split `remove-member-confirm.ts`,
 * `ownership-transfer.ts` and `pending-requests.ts` already use, and the one
 * the iOS and Android mirrors take (`MembershipRefusal.swift`,
 * `MembershipRefusal.kt`).
 *
 * ## Which rules here actually discriminate (MEASURED — each probe was run)
 *
 * Each was introduced as a deliberate regression, `npx vitest run` was run,
 * the failure below observed, and the probe reverted.
 *
 * 1. **The invitation is stored in the sentence instead of appended from the
 *    port** — i.e. the module decides retryability itself:
 *
 *        AssertionError: expected 'A role change is already waiting to app…'
 *        not to contain 'try again'
 *
 * 2. **`retryMaySucceed` is computed from a client-side list**
 *    (`code === "roleChangePending" || code === "pendingSetFull"`) instead of
 *    from the port — passes every real-core test, fails only the inverted one:
 *
 *        AssertionError: expected true to be false // Object.is equality
 *
 * 3. **An unrecognised reason returns `null`** (the "not a refusal" answer)
 *    rather than the fallback:
 *
 *        TypeError: Cannot read properties of null (reading 'retryMaySucceed')
 *
 * 4. **`refusalReasonOf` reads only the error's own property**, not the
 *    wrapper chain — which is the whole production path, since every
 *    membership call leaves `contacts/client.ts` through `ContactsError.wrap`:
 *
 *        AssertionError: expected undefined to be 'addNotOwnerOrAdmin'
 *
 * 5. **`externalJoinAffordance` returns `actionable` regardless of
 *    `retained`**:
 *
 *        AssertionError: expected 'actionable' to be 'informational'
 */

// ---------------------------------------------------------------------------
// Copy — one definition each, so no component can drift from it
// ---------------------------------------------------------------------------

/**
 * §6 row 7 — the fail-closed fallback, and the default for **anything** this
 * build does not recognise, a reason from a future core included.
 *
 * "It must exist, must never leak an internal reason, and must never be
 * reached in normal use." The third clause is the one worth holding onto: if
 * this string starts appearing, something is wrong that is not a copy
 * problem.
 */
export const REFUSAL_FALLBACK = "This change could not be requested.";

/** §6 row 1, verbatim. */
export const ADD_NOT_OWNER_OR_ADMIN = "Only the group owner or an admin can add members.";

/** §6 row 2, verbatim. */
export const REMOVE_NOT_OWNER_OR_ADMIN = "Only the group owner or an admin can remove members.";

/** §6 row 3, verbatim.
 *
 *  It also answers an owner who tapped their own `Leave group`: the core
 *  checks *is this leaf the owner's* before it checks anything about roles,
 *  so an owner's self-removal reports this and not `ownerCannotLeave`. The
 *  clients disable that control (§6.1.1) so it should not be reachable, but
 *  the string is right if it is. */
export const OWNER_UNREMOVABLE = "The group owner cannot be removed.";

/**
 * §6 row 4, verbatim.
 *
 * **Nothing can reach this today, and it keeps its sentence anyway.** The
 * reason fires only for a `Proposal::SelfRemove` authored by an owner, and
 * `SelfRemove` is unreachable in Lakko at the **wire-format** level, not
 * merely absent from the FFI: RFC 9420 requires it to be sent as a
 * `PublicMessage`, `Group::create` leaves the OpenMLS policy at its
 * pure-ciphertext default, and OpenMLS therefore refuses to craft one
 * (`CannotSelfRemoveWithPureCiphertext`). So no peer can send one either.
 * A client's `Leave group` calls `proposeRemoveMember` with its own leaf,
 * which the core classifies as a `Remove` and — for an owner — answers with
 * {@link OWNER_UNREMOVABLE}, since the owner check runs before the role
 * check. What an owner actually sees is the §6.1.1 pre-check, on a disabled
 * control that never calls the core at all.
 *
 * Folding it into {@link REFUSAL_FALLBACK} would have been defensible on
 * reachability alone and is still the wrong trade, for a reason the core
 * supplies: `self_removal_is_gated_for_its_own_sender_and_refused_for_the_owner`
 * exists precisely to fail the day somebody changes that wire-format policy,
 * and that change is wire-visible (it stops encrypting handshake messages to
 * the zero-trust DS). The day it fires, this reason becomes reachable and the
 * copy is already right. A collapse would instead degrade a precise refusal
 * into the fail-closed row nobody is supposed to see, silently.
 */
export const OWNER_CANNOT_LEAVE = "The group owner cannot leave the group.";

/**
 * §6 row 5, **widened** — the one existing row this table does not take
 * verbatim, and the reason is in the core rather than in taste.
 *
 * §6 says `Only the group owner can change admins.` The core raises ONE
 * reason (`notOwner`) for a role-table change *and* for an ownership
 * transfer, because a transfer **is** a role-table rewrite — it is not a
 * separate commit type, just the owner slot moved. One reason takes one
 * string, and a string that named only admins would be plainly wrong for
 * half of what the reason covers.
 *
 * §12 item 6 predicted this row would not move ("ownership transfer adds
 * none") and predicted the not-the-owner case would land on the fail-closed
 * fallback. It does not: it lands here, on `notOwner`.
 */
export const NOT_OWNER = "Only the group owner can change admins or transfer ownership.";

/**
 * §6 row 6, its **fact** half. The invitation is {@link RETRY_WHEN_IT_LANDS}
 * and is appended only when the core says waiting helps; joined, the two are
 * §6 row 6 verbatim, which a test asserts.
 *
 * §6 argues at length that this is the one *availability* row beside the
 * authorisation ones, and that getting the distinction backwards is a real
 * defect in both directions. Splitting the sentence is how that argument
 * becomes mechanical instead of remembered.
 */
export const ROLE_CHANGE_PENDING = "A role change is already waiting to apply.";

/**
 * NEW — §7.1 names this refusal and says plainly that turning it into fixed
 * copy "needs the typed refusal table §12 already lists as unbuilt".
 *
 * It deliberately states **no number.** `MAX_PENDING_PROPOSALS` is
 * `pub(crate)` and reaches neither FFI surface; §7.1 records that a
 * client-side literal is the stale-drift class this repo has already been
 * bitten by, and that it fails in the dangerous direction — reading as *room
 * left* when there is none.
 */
export const PENDING_SET_FULL = "Too many changes are already waiting to apply.";

/**
 * NEW — reachable from the §6.1 transfer picker when the chosen member has
 * left the group between the picker rendering and the tap.
 *
 * Phrased as the standing rule rather than as *that person is gone*, because
 * the reason also covers a role-table rewrite whose owner slot names a
 * non-member, where there is no "that person" the user chose. One reason, one
 * sentence, true on both paths.
 */
export const OWNER_NOT_A_MEMBER =
  "Ownership can only be held by a current member of this group.";

/**
 * NEW — a stale-view refusal, and the one an admin hits after tapping an
 * affordance that should no longer have been on screen: another admin
 * approved the joiner, or the joiner landed and was reaped.
 *
 * Not phrased as an error, because nothing went wrong. The list refreshes
 * behind the message.
 */
export const NO_SUCH_JOIN_REQUEST = "That join request is no longer waiting.";

/** NEW — two admins approving the same joiner is ordinary. Nothing is wrong
 *  and nothing needs doing. */
export const JOINER_ALREADY_A_MEMBER = "That person has already joined this group.";

/** §6 row 6's second half — singular, for the one pending role change. */
export const RETRY_WHEN_IT_LANDS = "Wait for it to land, then try again.";

/** {@link PENDING_SET_FULL}'s second half — plural, for a queue of them.
 *  Two invitations rather than one because "wait for it to land" after "too
 *  many changes" has no referent. */
export const RETRY_WHEN_THEY_LAND = "Wait for them to land, then try again.";

/**
 * NEW — the state §7.1 recorded as unsurfaceable.
 *
 * "At 64 retained external joins `retain_external_join` silently stops
 * retaining, and nothing distinguishes that from a request that never
 * arrived." The core's `retained` flag **is** that distinction, so the state
 * is statable now and silence stopped being the honest option. Naming no
 * number, for the {@link PENDING_SET_FULL} reason.
 */
export const JOIN_REQUEST_NOT_KEPT =
  "A join request arrived while too many were already waiting, so it was not kept. " +
  "Ask the person to send it again later.";

// ---------------------------------------------------------------------------
// The table
// ---------------------------------------------------------------------------

interface Copy {
  /** The statement of fact. Always shown. */
  readonly fact: string;
  /** Appended after a space **only** when the core says a plain retry may
   *  succeed. Absent for every reason the core never calls retryable. */
  readonly invitation?: string;
}

/**
 * Reason code → copy. Keyed on the core's stable `code()` strings, which the
 * core's own documentation pins as wire-visible and non-drifting.
 *
 * The eight reasons that are absent are absent **by a recorded decision**,
 * listed in {@link COLLAPSED_TO_FALLBACK} rather than merely omitted — see
 * that constant for why the difference is load-bearing.
 */
/** ADR-0050 — the per-account device cap (`MAX_LEAVES_PER_ACCOUNT`). */
export const DEVICE_LIMIT_REACHED = "Up to 8 devices per account. Remove one first.";
/** ADR-0050 — a device whose binding the group refuses. */
export const DEVICE_BINDING_INVALID =
  "Something is wrong with this device's key. Remove it from Devices and add it again.";

/** ADR-0060 — a one-to-one is always the same two people. No screen offers
 *  an add, remove or leave there, so these are plain facts for a refusal
 *  that should never be reached from the UI. */
export const ONE_TO_ONE_MEMBERSHIP_FIXED = "A chat is always the same two people.";
/** ADR-0060 — a one-to-one has no admins and no ownership to hand over. */
export const ONE_TO_ONE_HAS_NO_ROLES = "A chat has no admins.";
/** ADR-0060 — a change that would alter what kind of conversation this is. */
export const GROUP_KIND_CHANGED = "This change can't be made to this conversation.";

const TABLE: Readonly<Record<string, Copy>> = {
  addNotOwnerOrAdmin: { fact: ADD_NOT_OWNER_OR_ADMIN },
  removeNotOwnerOrAdmin: { fact: REMOVE_NOT_OWNER_OR_ADMIN },
  ownerUnremovable: { fact: OWNER_UNREMOVABLE },
  ownerCannotLeave: { fact: OWNER_CANNOT_LEAVE },
  notOwner: { fact: NOT_OWNER },
  ownerNotAMember: { fact: OWNER_NOT_A_MEMBER },
  noSuchJoinRequest: { fact: NO_SUCH_JOIN_REQUEST },
  joinerAlreadyAMember: { fact: JOINER_ALREADY_A_MEMBER },
  roleChangePending: { fact: ROLE_CHANGE_PENDING, invitation: RETRY_WHEN_IT_LANDS },
  pendingSetFull: { fact: PENDING_SET_FULL, invitation: RETRY_WHEN_THEY_LAND },
  // ADR-0050 (docs/ux/second-device.md section 5): the only per-device
  // refusal a user is shown is the cap; a binding the peers reject is a
  // client bug and gets the same plain sentence rather than a screen.
  deviceLimitReached: { fact: DEVICE_LIMIT_REACHED },
  deviceBindingInvalid: { fact: DEVICE_BINDING_INVALID },
  // ADR-0060: neutral facts, no invitation — none of the three resolves by
  // waiting (NotPermitted / NotPermitted / DoNotRetry in the core).
  oneToOneMembershipFixed: { fact: ONE_TO_ONE_MEMBERSHIP_FIXED },
  oneToOneHasNoRoles: { fact: ONE_TO_ONE_HAS_NO_ROLES },
  groupKindChanged: { fact: GROUP_KIND_CHANGED },
};

/**
 * The reasons that deliberately share §6's fail-closed row.
 *
 * **Why this exists as a list rather than as an absence.** Falling back is
 * what {@link membershipRefusalOf} does for *anything* it does not know, so
 * a reason with no row renders a correct-looking sentence whether it was
 * collapsed on purpose or forgotten. A test over the table alone therefore
 * cannot fail on a new core reason — it would pass, quietly, showing the
 * fail-closed row for a refusal somebody meant to write copy for. Recording
 * the collapse turns that into an assertion: this list plus {@link TABLE}
 * must be exactly the core's set, so a nineteenth reason lands in neither and
 * CI says so.
 *
 * The rule behind every entry is the same — **a reason with no
 * user-initiated entry point gets no sentence** — and each still earns its
 * place separately:
 *
 * - `notAMember`, `unsupportedProposal`, `noRoleTable` — §6 row 7 names these
 *   three explicitly ("unsupported / unresolvable / no role table"). No
 *   change; the spec already decided them.
 * - `leafUpdateRefused` — the core states it is reachable from no
 *   client-facing propose API; it exists so a *log* can tell it apart from
 *   `unsupportedProposal`. To a user the two are the same fact — a kind of
 *   change Lakko will not make — and the distinction survives where it is
 *   useful, in `err.message`.
 * - `roleTableWouldVanish` — a role-table rewrite that leaves no readable
 *   table behind, which no client can compose from its own UI. A peer-gate
 *   refusal on somebody else's commit.
 * - `notACommitter`, `sweepAlreadyStaged` — sweep-side. §8: the committer
 *   sees "in the steady state, nothing", and all three clients swallow sweep
 *   failures by design (`"Never surfaced as a user-facing error: nothing the
 *   user did failed"`). A sentence here would have no reader.
 * - `proposalsPending` — refuses a **direct** commit, and a client takes the
 *   direct path only in a group with no role table, where the pending set is
 *   permanently empty. Unreachable by construction rather than by policy.
 */
export const COLLAPSED_TO_FALLBACK: ReadonlySet<string> = new Set([
  "notAMember",
  "leafUpdateRefused",
  "unsupportedProposal",
  "noRoleTable",
  "roleTableWouldVanish",
  "notACommitter",
  "proposalsPending",
  "sweepAlreadyStaged",
]);

/** The reasons {@link TABLE} gives a sentence of their own. Exported for the
 *  completeness assertion, not for branching on. */
export const REASONS_WITH_OWN_COPY: readonly string[] = Object.keys(TABLE);

// ---------------------------------------------------------------------------
// Reading a refusal off a thrown value
// ---------------------------------------------------------------------------

/** How deep to follow a wrapper chain. Four is well past anything this app
 *  builds and keeps a cyclic `cause` from spinning. */
const MAX_WRAP_DEPTH = 4;

/**
 * The core's refusal code carried by `err`, or `undefined` when `err` is not
 * a membership refusal.
 *
 * **It follows wrappers, and that is not defensive coding — it is the only
 * production path.** Every membership call leaves `contacts/client.ts`
 * through `ContactsError.wrap`, which constructs a *new* `Error` holding only
 * the message and parks the original on `variant.cause`. Reading just the
 * outermost object would make this whole table dead code on the one route
 * that reaches the UI.
 */
export function refusalReasonOf(err: unknown): string | undefined {
  let current: unknown = err;
  for (let depth = 0; depth < MAX_WRAP_DEPTH; depth++) {
    if (typeof current !== "object" || current === null) return undefined;
    const { refusalReason } = current as { refusalReason?: unknown };
    // A non-string is not a reason. Coercing one would produce a lookup key
    // that misses, and a plain error would then be rendered as the
    // fail-closed row — worse than showing its own message.
    if (typeof refusalReason === "string") return refusalReason;
    const wrapped = current as {
      cause?: unknown;
      variant?: { cause?: unknown };
    };
    current = wrapped.variant?.cause ?? wrapped.cause;
  }
  return undefined;
}

/** A refused membership change, ready to render. */
export interface MembershipRefusal {
  /** The core's stable code, verbatim — even when this build has no row for
   *  it. Carry it into a log; never show it. */
  readonly reason: string;
  /**
   * `true` when this build has a *considered* answer for the reason — either
   * a sentence of its own or a recorded collapse onto
   * {@link REFUSAL_FALLBACK}.
   *
   * `false` therefore means exactly one thing: **a reason from a core newer
   * than this build.** That is worth logging distinctly, because it is the
   * only case where the fail-closed row is showing for a refusal nobody
   * decided about.
   */
  readonly recognised: boolean;
  /** The sentence to show. Never empty. */
  readonly message: string;
  /** The core's answer to *will the identical call work later*. Drives the
   *  retry affordance; never re-derived here. */
  readonly retryMaySucceed: boolean;
}

/**
 * Asks the core whether a plain retry may succeed.
 *
 * Production passes `membershipRefusalRetryMaySucceed` from the wasm surface.
 * It is a port rather than a direct import so the tests can prove the module
 * *asks* instead of agreeing by coincidence — see the header, probe 2.
 */
export type RetryPort = (reason: string) => boolean;

/**
 * Classify a thrown value as a membership refusal and produce its copy, or
 * `null` when it is not a refusal at all.
 *
 * `null` and the fallback are different answers and the difference matters.
 * `null` means *this is some other error* — a failed upload, a dead socket —
 * and its own message is the best thing to show. The fallback means *this is
 * a refusal whose reason this build has no row for*, which includes a reason
 * added to a future core, and showing the core's developer sentence there
 * would leak an internal reason §6 row 7 forbids leaking.
 */
export function membershipRefusalOf(
  err: unknown,
  retryMaySucceed: RetryPort,
): MembershipRefusal | null {
  const reason = refusalReasonOf(err);
  if (reason === undefined) return null;

  const copy = TABLE[reason];
  // Fail-closed on the retry question too: an unknown reason gets no
  // invitation, because nothing is known about whether waiting helps. The
  // core answers `false` for a code it does not recognise, so this agrees
  // with it rather than second-guessing it.
  const retry = retryMaySucceed(reason);
  const message =
    copy === undefined
      ? REFUSAL_FALLBACK
      : retry && copy.invitation !== undefined
        ? `${copy.fact} ${copy.invitation}`
        : copy.fact;

  return {
    reason,
    recognised: copy !== undefined || COLLAPSED_TO_FALLBACK.has(reason),
    message,
    retryMaySucceed: retry,
  };
}

// ---------------------------------------------------------------------------
// ADR-0027 — an external-join request the quarantine did not keep
// ---------------------------------------------------------------------------

/** What a client may offer for one external-join request. */
export type ExternalJoinAffordance =
  | { readonly kind: "actionable" }
  | { readonly kind: "informational"; readonly notice: string };

/**
 * Whether an external-join request may be offered with Approve / Reject.
 *
 * The core fires `ExternalJoinProposal` **even when the quarantine was full
 * and the request was not kept**. Approving such a request has nothing to
 * look up and is refused with `noSuchJoinRequest`, so an Approve button for
 * it is an affordance that cannot work — the same class of dishonesty as a
 * cosmetic security badge, which is why the honesty rule reaches a members
 * panel and not only a lock icon.
 *
 * All three clients build their pending list from the core's retained set, so
 * an unretained request is *already* absent from the list and no Approve is
 * rendered. What was missing is the other half: the request is then invisible,
 * and the joiner is left waiting on an admin who was never told. This returns
 * something to say rather than nothing.
 */
export function externalJoinAffordance(input: {
  readonly retained: boolean;
}): ExternalJoinAffordance {
  return input.retained
    ? { kind: "actionable" }
    : { kind: "informational", notice: JOIN_REQUEST_NOT_KEPT };
}

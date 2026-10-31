// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0045 UX §6.2 — the end of your own membership.
 *
 * ## Since ADR-0087 section 9
 *
 * A person out of a group loses it: `membership-end.ts` carries that out, and
 * the view is gone before any of this applies. What is below now describes one
 * device only: one whose own leaf was retired while the account stays in the
 * group (ADR-0050). It keeps its history and the ended presentation here
 * (the terminal row, the withdrawn composer, the frozen roster, `Delete this
 * chat`) until it is re-admitted. "The answer to keep the history is yes"
 * below is true of that device and no other.
 *
 * ## What this is for
 *
 * The commit that removes your own leaf arrives on an ordinary poll, merges,
 * and the group goes permanently dead in the MLS core. Before this module the
 * client carried on as though nothing had happened: the chat stayed in the
 * list, the composer stayed enabled, the members panel kept offering
 * `Leave group` on a group already left, and the first thing the user learned
 * was that sending failed.
 *
 * ## Why this is a plain module and not part of `routes/Chat.tsx`
 *
 * `vitest.config.ts` collects `src/**\/*.test.ts` in a plain `node`
 * environment with no jsdom and no Solid JSX transform, so a `.tsx` component
 * cannot be tested at all. Every rule §6.2 states therefore lives here, where
 * it is executable, and `Chat.tsx` renders what this module decides — the same
 * split `pending-requests.ts`, `ownership-transfer.ts` and
 * `committer-beacon.ts` already use.
 *
 * ## The one asymmetry the whole design falls out of
 *
 * After the removal the client can still **read** everything about the group
 * and can no longer **do** anything with it. `members()`, `epoch`,
 * `memberCount`, `ownLeafIndex` and `groupRoles()` are plain reads of the
 * public group with no state gate, while `createApplicationMessage`,
 * `exportSecret` and every further decrypt throw `UseAfterEviction`. That is
 * not a limitation to work around — it is precisely the shape of the correct
 * UI, and it is why the answer to *keep the history?* is yes and the answer to
 * *keep the composer?* is no.
 *
 * Two consequences worth stating because they are easy to get wrong:
 *
 *   * **The state is terminal and survives a restart.** OpenMLS calls
 *     `Inactive` "a terminal state that the group can not exit from", and
 *     `write_group_state` runs before the merge, so a snapshot taken
 *     afterwards restores an inactive group.
 *   * **The local view is frozen for good.** `unprotect_message` refuses on
 *     `!is_active()` before it looks at the message at all, and the refusal is
 *     NOT epoch-scoped — so rows still sitting on the DS are unreadable
 *     forever, even ones sealed at an epoch this device still holds keys for.
 *     The roster, the epoch and the role table stop at the values that commit
 *     produced and never move again.
 *
 * ## Which rules here actually discriminate (MEASURED — each probe was run)
 *
 * Each of the following was introduced as a deliberate regression, `vitest`
 * was run, the failure below observed, and the probe reverted.
 *
 * 1. **`membershipState` returns `active` on `isActive === false`** — i.e. the
 *    eviction is never detected:
 *
 *        AssertionError: expected { kind: 'active' } to deeply equal { kind: 'ended', …(1) }
 *        AssertionError: expected { kind: 'composer' } to be 'withdrawn'
 *        AssertionError: expected [] to deeply equal [ 'stop-beacon-ticker', …(1) ]
 *
 *    Seven tests go red across four describes — the composer, the teardown,
 *    the terminal row and the chat-list line all hang off this one predicate.
 *
 * 2. **`membershipState` treats an unknown `isActive` as evicted** (the
 *    fail-closed reflex, which is the wrong direction here):
 *
 *        AssertionError: expected { kind: 'ended', left: true } to deeply equal { kind: 'active' }
 *
 *    A torn handle is not evidence of anything. Asserting `You are no longer a
 *    member of this group.` on it is a false statement the user cannot check,
 *    where leaving the chat as it is today is merely the status quo.
 *
 * 3. **`membershipState` reads the leave record before `isActive`** — i.e. it
 *    treats *asked to leave* as *left*:
 *
 *        AssertionError: expected { kind: 'ended', left: true } to deeply equal { kind: 'active' }
 *
 *    That is §6.2.11's optimistic transition. Between publishing the request
 *    and the commit merging you are still fully in the group, still receiving
 *    and still decrypting.
 *
 * 4. **The fallback string is the LEFT one** (the narrowing and the neutral
 *    case swapped):
 *
 *        AssertionError: expected 'You left this group.' to be 'You are no longer a member of this group.'
 *
 *    A device that lost the record would then tell a user they left a group
 *    they were thrown out of.
 *
 * 5. **`recordLeaveRequested` overwrites an existing stamp:**
 *
 *        AssertionError: expected 9000 to be 1000 // Object.is equality
 *
 * 6. **`pendingTeardownSteps` ignores `alreadyRun`:**
 *
 *        AssertionError: expected [ 'stop-beacon-ticker', …(1) ] to deeply equal []
 *
 *    Re-firing `retire-push-subscription` is a network call per reactive
 *    re-run; re-firing it is harmless but unbounded, and a teardown that
 *    cannot say it is finished is one nothing can be sequenced after.
 *
 * 7. **`composerState` keeps the composer when the membership has ended:**
 *
 *        AssertionError: expected 'composer' to be 'withdrawn' // Object.is equality
 *
 * 8. **`composerState` preserves a whitespace-only draft:**
 *
 *        AssertionError: expected '   ' to be null // Object.is equality
 *
 *    An `Unsent draft` block containing three spaces is furniture pretending
 *    to be data.
 *
 * 9. **`messageActionsState` always returns `available`** — i.e. the defect it
 *    closes, the per-message menu still offered on a dead group:
 *
 *        AssertionError: expected 'available' to be 'withdrawn'
 *        AssertionError: expected true to be false // Object.is equality
 *
 *    Two tests go red, and the second is the one that matters: it is the
 *    equivalence with {@link composerState}, so a menu that stops agreeing
 *    with the composer fails whichever of the two was changed.
 */
import { t } from "../l10n";

// ---------------------------------------------------------------------------
// Copy (§6.2) — one definition each, so the component cannot drift from it
// ---------------------------------------------------------------------------

/**
 * §6.2.3 — the narrowing, applied only on positive evidence.
 *
 * True of exactly one of the two cases, which is why it needs the durable
 * record and why {@link ENDED_REMOVED} is the fallback rather than this.
 */
export const ENDED_LEFT = (): string => t("chat.ended.left");

/**
 * §6.2.3 — the neutral string, and the FALLBACK.
 *
 * This inverts the usual reflex to treat the neutral case as the special one.
 * "You are no longer a member of this group" is true of someone who left as
 * well as someone who was removed; "you left this group" is true of only one.
 * So a device that lost the record — the ADR-0040 crash window, a reinstall, a
 * restore — degrades to a sentence that is still correct rather than to one
 * that is wrong.
 *
 * And nothing here says **who**. Not because the information is unobtainable
 * (the removing proposal's author leaf is in hand inside the core's own gate)
 * but because a client that names a remover it inferred is one bad inference
 * away from a serious accusation, and because §5.1's discipline — state
 * inaction, name nobody, blame nobody — is the same discipline whether the
 * news is a wait or an ending.
 */
export const ENDED_REMOVED = (): string => t("chat.ended.no_longer_member");

/**
 * §6.2.5 line 2 — the standing consequence, identical in both cases.
 *
 * Written to claim exactly what is claimed and no more. A device that was
 * offline across its own removal loses the tail of its history silently and
 * permanently (§6.2.6): the decrypt refusal is not epoch-scoped, so rows still
 * on the DS at the moment the commit is applied are unreachable forever. The
 * design deliberately does not mention that — the user cannot tell how much
 * they are missing and no client can tell them — and *the messages you already
 * received are still here* stays true either way.
 */
export const ENDED_CONSEQUENCE = (): string => t("chat.ended.consequence");

/** §6.2.4 — the chat-list row's one muted secondary line. Deliberately neutral
 *  between left and removed: the list is a glance surface and the distinction
 *  belongs in the transcript. */
export const CHAT_LIST_ENDED_LINE = (): string => t("chats.row.ended");

/** §6.2.5 — the static bar that REPLACES the composer, in the same slot so the
 *  layout does not jump. */
export const COMPOSER_WITHDRAWN = (): string => t("chat.ended.composer");

/** §6.2.5 — the muted label above the preserved draft. */
export const UNSENT_DRAFT_LABEL = (): string => t("chat.ended.unsent_draft");

/**
 * §6.2.7 — one line at the top of the frozen roster.
 *
 * A statement about the **list**, not about any row in it, and that is
 * load-bearing: the role table is untouched by a `Remove` — a removal rewrites
 * no `GroupContext` — so a removed admin's frozen roster still names them an
 * admin, and nothing in the UI may present that as a live role.
 * *"Stopped updating"* is the literal truth of the frozen local view rather
 * than a euphemism for it.
 */
export const FROZEN_ROSTER_HEADER = (): string => t("chat.members.frozen");

/** §6.2.4 — the one control still honest once you are out. */
export const DELETE_CHAT_CONTROL = (): string => t("chat.delete.control");

/** §6.2.4 — confirmation title. */
export const DELETE_CHAT_TITLE = (): string => t("chat.delete.title");

/**
 * §6.2.4 — confirmation body, exact rather than sweeping.
 */
export const DELETE_CHAT_BODY = (): string => t("chat.delete.body");

// ---------------------------------------------------------------------------
// The state
// ---------------------------------------------------------------------------

/** Whether this device is still in the group, and — if not — how that
 *  happened, as far as this device can honestly tell. */
export type MembershipState =
  | { readonly kind: "active" }
  | {
      readonly kind: "ended";
      /** `true` only on positive local evidence that we asked (§6.2.3). */
      readonly left: boolean;
    };

/**
 * Read the core's own membership predicate, or `undefined` when the handle
 * cannot be read at all.
 *
 * `Group.isActive` is the variable `createApplicationMessage`, `exportSecret`
 * and `unprotect_message` each consult before refusing, so a composer gated on
 * it is disabled for the reason the send would have failed. The available
 * alternative — *no member record whose signature key is ours* — is sound
 * today and would work, but it rests on a leaf's signature key being the
 * account's signature key, which ADR-0022 names as an open follow-up (the
 * Phase-2b per-device-leaf resolution). The day that lands, the inference
 * breaks silently, in the dangerous direction, in three clients at once.
 * `isActive` is about *this handle* and is immune to it.
 *
 * A throw reads as `undefined` rather than as `false` — see
 * {@link membershipState}.
 */
export function readIsActive(group: { readonly isActive: boolean }): boolean | undefined {
  try {
    return group.isActive;
  } catch {
    return undefined;
  }
}

/**
 * Decide the membership state.
 *
 * **The order of the two reads is the rule.** `isActive` is consulted first
 * and alone decides whether the membership has ended; the leave record only
 * ever *narrows the copy* afterwards. Reading the record first would make
 * *asked to leave* mean *left*, which is §6.2.11's forbidden optimistic
 * transition: between publishing the request and the commit merging you are
 * still fully in the group — still receiving, still decrypting — and §3's
 * argument against an optimistic `Remove` is the same argument pointed at
 * yourself.
 *
 * **An unknown `isActive` fails toward `active`, and that is deliberate even
 * though it is the un-reflexive direction.** Fail-closed is the house rule for
 * a claim about *cryptographic capability*; this is a claim about *fact*. A
 * torn handle is not evidence of an eviction, and asserting `You are no longer
 * a member of this group.` on one is a false statement the user has no way to
 * check. Getting it wrong the other way costs only the status quo: the
 * composer stays and a send fails, which is exactly what happened before this
 * module existed.
 */
export function membershipState(input: {
  readonly isActive: boolean | undefined;
  readonly leaveRequestedAtMs: number | undefined;
}): MembershipState {
  if (input.isActive !== false) return { kind: "active" };
  return { kind: "ended", left: input.leaveRequestedAtMs !== undefined };
}

/**
 * The standing gate every background discipline consults (§6.2.8).
 *
 * A gate rather than a one-shot, because four of the five disciplines are
 * things that must *keep not happening*: the poll can be re-entered from a
 * dozen call sites, the rotation decision is re-evaluated per commit batch,
 * and the call button is re-rendered on every roster change.
 */
export function membershipEnded(state: MembershipState): boolean {
  return state.kind === "ended";
}

// ---------------------------------------------------------------------------
// §6.2.5 — the terminal row and the composer
// ---------------------------------------------------------------------------

/** The two lines of the terminal timeline row. */
export interface TerminalRowCopy {
  /** {@link ENDED_LEFT} or {@link ENDED_REMOVED}. */
  readonly headline: string;
  /** {@link ENDED_CONSEQUENCE}. */
  readonly consequence: string;
}

/**
 * The terminal row, or `null` while the membership holds.
 *
 * It goes in the **timeline**, not in a banner, and §10 already says why:
 * *only outcomes belong in the timeline* — and this is an outcome, one that
 * happened at a definite point in the transcript. A row marks *where* the
 * history stops, survives scrollback and needs no dismissal state.
 */
export function terminalRow(state: MembershipState): TerminalRowCopy | null {
  if (state.kind !== "ended") return null;
  return {
    headline: state.left ? ENDED_LEFT() : ENDED_REMOVED(),
    consequence: ENDED_CONSEQUENCE(),
  };
}

/** The chat-list row's secondary line, or `null` while the membership holds. */
export function chatListLine(state: MembershipState): string | null {
  return state.kind === "ended" ? CHAT_LIST_ENDED_LINE() : null;
}

/** What occupies the composer's slot. */
export type ComposerState =
  | { readonly kind: "composer" }
  | {
      readonly kind: "withdrawn";
      /** {@link COMPOSER_WITHDRAWN}. */
      readonly notice: string;
      /** The unsent draft, verbatim, or `null` when there was nothing worth
       *  keeping. */
      readonly draft: string | null;
      /** {@link UNSENT_DRAFT_LABEL}, or `null` when there is no draft. */
      readonly draftLabel: string | null;
    };

/**
 * Decide the composer's slot (§6.2.5).
 *
 * **Replaced, not disabled, and the distinction is principled.** §5.3 argues a
 * pending row's `Remove` must stay *visible but disabled*, because hiding it
 * would read as the permission having vanished when it is merely suspended.
 * The permission here has vanished, permanently and by design. So the rule is:
 * **disable while the state is transient, remove when it is terminal** — and a
 * disabled Send button on a group you will never be in again is a control
 * promising a future it does not have.
 *
 * **The draft is preserved, and NOT in an input.** The commit lands during a
 * poll, which can be mid-sentence, and silently discarding what the user typed
 * is a small data loss with no upside. But the obvious fix — leave the text
 * field there, disabled — is wrong on two platforms of three: a disabled
 * SwiftUI `TextField` and a disabled Compose `OutlinedTextField` are not
 * selectable, so the user can see their words and cannot get them out. The
 * cross-platform shape is therefore **selectable, non-editable text**; which
 * widget expresses that is each platform's business (`readonly` — never
 * `disabled` — on web).
 *
 * A blank-or-whitespace draft yields no block at all: an `Unsent draft`
 * heading over three spaces is furniture pretending to be data. The draft that
 * IS kept is preserved **verbatim**, leading and trailing whitespace included
 * — it is the user's text, and this decides whether to show it, not what it
 * should have been.
 */
export function composerState(
  state: MembershipState,
  draft: string,
): ComposerState {
  if (!membershipEnded(state)) return { kind: "composer" };
  const worthKeeping = draft.trim().length > 0;
  return {
    kind: "withdrawn",
    notice: COMPOSER_WITHDRAWN(),
    draft: worthKeeping ? draft : null,
    draftLabel: worthKeeping ? UNSENT_DRAFT_LABEL() : null,
  };
}

/** Whether the per-message action menu is offered at all. */
export type MessageActionsState = "available" | "withdrawn";

/**
 * Decide the per-message action menu — Edit, Delete, React (§6.2.5's rule,
 * reached from the other end).
 *
 * **Withdrawn, not disabled, and it is the composer's own question.** All
 * three actions compose an MLS application message, so all three are refused
 * by `create_message`'s `is_operational()` gate — the *same* gate, at the
 * *same* instant, for the *same* permanent reason as Send. §5.3's
 * visible-but-disabled rule is for a permission that is merely *suspended*; a
 * greyed React button on a group that can never become live again is a
 * control promising a future it does not have, which is exactly the shape
 * §6.2.5 rejects for the composer.
 *
 * **Why this is a function here and not an `&&` at the call site.** Two gates
 * over one predicate drift when somebody later adds a condition to one of
 * them, and the drift is silent in both directions: an ungated menu offers
 * three actions that cannot work, and an over-gated one hides working
 * controls. Both this and {@link composerState} therefore route through
 * {@link membershipEnded}, and the test enumerates every state and asserts
 * they agree.
 *
 * **What this does NOT withdraw is the information.** A reaction *chip* is
 * rendered content — part of the frozen transcript §6.2.6 keeps whole — so
 * the counts stay. What goes is the affordance that would toggle one.
 */
export function messageActionsState(state: MembershipState): MessageActionsState {
  return membershipEnded(state) ? "withdrawn" : "available";
}

// ---------------------------------------------------------------------------
// §6.2.8 — the one-shot half of the teardown
// ---------------------------------------------------------------------------

/**
 * The teardown steps that are **effects** rather than gates.
 *
 * §6.2.8 lists five disciplines and they are not all the same shape, so they
 * are not modelled as though they were. Three of the five — the poll loop, the
 * shrink rotation and the exporter-backed call button — are pure **gates**:
 * there is nothing to dispose, only something that must keep not happening,
 * and {@link membershipEnded} is what they consult. These two are the ones
 * with an effect to fire, and firing an effect more than once is what
 * {@link pendingTeardownSteps} exists to prevent.
 *
 *   * `stop-beacon-ticker` — the beacon emitter runs on its own `setInterval`
 *     and composes an application message, which now throws. A candidate that
 *     cannot commit must not claim it can: a beacon means *able to sweep*, and
 *     an evicted device can never sweep again. (The same rule that keeps a
 *     backgrounded device from emitting, reached from the other direction —
 *     there the device could sweep and is not running; here it is running and
 *     never can.) The emit path is gated too, so the interval is belt and
 *     braces; clearing it is what stops the wasted wake.
 *   * `retire-push-subscription` — the interesting one. ADR-0043 recorded that
 *     an evicted member's push-gw row *"is never deleted — nobody else can
 *     delete it and no client has 'you were removed' handling"*, so it goes
 *     **inert** rather than away. This is that handling: the evicted device
 *     can delete its own row, and `retireOldPushGid` already does exactly the
 *     right three things. It closes the other half of G-36 residual 2 —
 *     rotation stops the group notifying the old id; this stops the ex-member
 *     being subscribed to anything at all. **The honest bound:** it needs the
 *     evicted device to come online and run it, so it *complements* rotation
 *     and replaces nothing.
 */
export type EndedTeardownStep = "stop-beacon-ticker" | "retire-push-subscription";

/** The canonical set, so a caller cannot quietly implement four of five. */
export const ENDED_TEARDOWN_STEPS: readonly EndedTeardownStep[] = [
  "stop-beacon-ticker",
  "retire-push-subscription",
];

/**
 * Which one-shot teardown steps are owed right now.
 *
 * Returns `[]` while the membership holds, and `[]` again once every step has
 * been run — so the caller can drive it from a reactive effect that re-runs
 * for unrelated reasons without re-firing a network call each time. A
 * partially-completed teardown resumes rather than restarting, which matters
 * because the two steps fail independently: the ticker is cleared locally and
 * cannot fail, while the push retire is bounded network IO that can.
 */
export function pendingTeardownSteps(
  state: MembershipState,
  alreadyRun: ReadonlySet<EndedTeardownStep>,
): EndedTeardownStep[] {
  if (state.kind !== "ended") return [];
  return ENDED_TEARDOWN_STEPS.filter((step) => !alreadyRun.has(step));
}

/**
 * {@link pendingTeardownSteps}, but it **claims** what it returns in the same
 * synchronous call.
 *
 * This is what the driver must use, and the reason is a defect that
 * `pendingTeardownSteps` alone cannot prevent. One of the two steps is async
 * (the push retire is bounded network IO), so marking it done only after the
 * await leaves a window in which the driving effect re-runs — and it re-runs
 * on every roster change, for reasons that have nothing to do with the
 * eviction — reads the step as still owed, and fires a second concurrent
 * unsubscribe. "Exactly once" has to be decided *before* the first await, not
 * after the last one.
 *
 * A step that FAILS is handed back with {@link releaseTeardownStep}, so the
 * next re-run retries it. Claim-then-release rather than
 * mark-on-success is deliberate: the two orderings differ only under
 * concurrency, and that is precisely the case this exists for.
 */
export function claimTeardownSteps(
  state: MembershipState,
  alreadyRun: Set<EndedTeardownStep>,
): EndedTeardownStep[] {
  const owed = pendingTeardownSteps(state, alreadyRun);
  for (const step of owed) alreadyRun.add(step);
  return owed;
}

/** Hand a claimed step back after it failed, so a later pass retries it. */
export function releaseTeardownStep(
  alreadyRun: Set<EndedTeardownStep>,
  step: EndedTeardownStep,
): void {
  alreadyRun.delete(step);
}

// ---------------------------------------------------------------------------
// §6.2.3 / §9 — the durable `leaveRequestedAt` record
// ---------------------------------------------------------------------------

const LEAVE_STORE_PREFIX = "lakko.leave-requested.v1";

function leaveStoreKey(accountIdB64: string, groupIdB64: string): string {
  return `${LEAVE_STORE_PREFIX}.${accountIdB64}.${groupIdB64}`;
}

/**
 * Record that this device published a self-targeted leave request.
 *
 * **This has to be its own durable field, and the reason is that the failure
 * is silent.** The client already knows locally whether it asked — a device
 * that requested a leave wrote an observation under `remove:{ownLeaf}` with
 * `own: true` — but that record is pruned by the same pass that makes the
 * question askable. The observation store's whole discipline is to drop an
 * observation once the core no longer holds the request, and the sweep that
 * removes you is exactly what empties the core's pending set. So the poll
 * batch that carries your eviction also destroys the only evidence that you
 * asked for it, and the order in which those two happen is an implementation
 * detail no test pins.
 *
 * Hence: written at **publish**, never pruned with the pending sweep, and
 * discarded only with the group record itself.
 *
 * A timestamp rather than a boolean costs nothing and is strictly more useful.
 * The FIRST stamp wins, for the same reason `recordObservation`'s does: a
 * re-request is the same intent, not a new one.
 *
 * Never throws — a quota failure costs the *narrowing*, and the fallback
 * string is the one that is true either way.
 */
export function recordLeaveRequested(
  accountIdB64: string,
  groupIdB64: string,
  atMs: number,
): void {
  try {
    const key = leaveStoreKey(accountIdB64, groupIdB64);
    if (globalThis.localStorage?.getItem(key) !== null) return;
    globalThis.localStorage?.setItem(key, String(atMs));
  } catch {
    /* the narrowing is best-effort; the fallback string is still true */
  }
}

/** Read the stamp, or `undefined` when absent or unparseable. A corrupt row
 *  degrades to the removed copy, which is still true. */
export function loadLeaveRequestedAt(
  accountIdB64: string,
  groupIdB64: string,
): number | undefined {
  try {
    const raw = globalThis.localStorage?.getItem(
      leaveStoreKey(accountIdB64, groupIdB64),
    );
    if (raw === null || raw === undefined) return undefined;
    const n = Number(raw);
    return Number.isFinite(n) ? n : undefined;
  } catch {
    return undefined;
  }
}

/** Discard the record for one group — the `Delete this chat` path, which is
 *  the only thing that discards the group record itself. */
export function clearLeaveRequested(
  accountIdB64: string,
  groupIdB64: string,
): void {
  try {
    globalThis.localStorage?.removeItem(leaveStoreKey(accountIdB64, groupIdB64));
  } catch {
    /* best-effort */
  }
}

/** Drop every record for one account — `recover` / `forgetDevice` hygiene, the
 *  same discipline `clearObservationsForAccount` follows. */
export function clearLeaveRequestedForAccount(accountIdB64: string): void {
  try {
    const store = globalThis.localStorage;
    if (store === undefined || store === null) return;
    const victims: string[] = [];
    for (let i = 0; i < store.length; i++) {
      const k = store.key(i);
      if (k !== null && k.startsWith(`${LEAVE_STORE_PREFIX}.${accountIdB64}.`)) {
        victims.push(k);
      }
    }
    for (const k of victims) store.removeItem(k);
  } catch {
    /* best-effort */
  }
}

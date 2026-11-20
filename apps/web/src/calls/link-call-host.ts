// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The call-link (`/c/<slug>`, ADR-0032) call as the app-level call host holds
 * it — the decisions, kept pure so they can be tested (vitest runs in plain
 * node, without jsdom and without the Solid transform).
 *
 * ## Why it moved
 *
 * `routes/JoinCall.tsx` owned the `MultiJoinerCall`: its cleanup hung the call
 * up, so leaving the `/c/` route (or the owner's screen) ended it for this
 * device, and nothing stopped a link call from starting while a 1x1 or group
 * call was live — both then drove the one shared in-call UI store. The host now
 * owns the orchestrator, the overlay, the audio sinks, the controls and the
 * hang-up; the route only decodes the link, runs the guest sign-up, asks the
 * host for the call and shows the host's state for it.
 *
 * Nothing about the protocol changed: `MultiJoinerCall` still founds (owner) or
 * joins (joiner), elects the committer, dedups admissions, rekeys per
 * admission, checks the owner is in the joined group and times out a call with
 * no live committer. What is here is only where its outcome goes.
 */
import { t } from "../l10n";
import { callBusyMessage, type CallKind, type CallText } from "./call-slot";
import type { StandaloneCallEntry } from "./call-log";

/** Where a link call is, from this device's point of view. */
export type LinkCallPhase =
  /** Minting the call-admission token and connecting to the SFU. */
  | "starting"
  /** Joiner: signaling is open and the KeyPackage is out; no Welcome yet. */
  | "waiting"
  /** In the call's MLS group with the seed derived (owner: at founding). */
  | "live"
  /** The session closed or failed. */
  | "ended"
  /** Joiner: nobody admitted this device within the admission window — the
   *  call has no live committer (the owner left, or the call is over). */
  | "dead"
  /** A setup error, or a Welcome into a group without the owner (refused). */
  | "error";

/** What the orchestrator reported. */
export type LinkCallEvent =
  | { readonly kind: "signaling-open"; readonly role: "owner" | "joiner" }
  | { readonly kind: "joined" }
  | { readonly kind: "ended" }
  | { readonly kind: "no-committer" }
  | { readonly kind: "error" };

/** The phases in which the call holds the one call slot. */
export function linkCallOccupiesSlot(phase: LinkCallPhase): boolean {
  return phase === "starting" || phase === "waiting" || phase === "live";
}

/**
 * Fold one orchestrator report.
 *
 * `ended` is a weak terminal: `MultiJoinerCall` tears its session down BEFORE it
 * reports why — an admission timeout closes the session (reported `ended`) and
 * then reports `no-committer`; a refused redirect closes it and then reports
 * `error`. The later, specific report replaces `ended`. `dead` and `error` are
 * final: nothing after them changes what the user is told.
 */
export function reduceLinkCallPhase(phase: LinkCallPhase, event: LinkCallEvent): LinkCallPhase {
  if (phase === "dead" || phase === "error") return phase;
  switch (event.kind) {
    case "signaling-open":
      // The owner is live from founding; a joiner that is still starting now
      // waits for its Welcome (which may already have arrived).
      if (phase === "starting" && event.role === "joiner") return "waiting";
      return phase;
    case "joined":
      return phase === "starting" || phase === "waiting" ? "live" : phase;
    case "ended":
      return linkCallOccupiesSlot(phase) ? "ended" : phase;
    case "no-committer":
      return phase === "live" ? phase : "dead";
    case "error":
      return "error";
  }
}

export interface LinkCallStartInput {
  /** The call occupying the slot, or `null`. */
  readonly occupant: CallKind | null;
  /** The link call in the host's link slot, if it holds the slot. */
  readonly activeLinkCallIdHex: string | null;
  /** The call the link names. */
  readonly requestedCallIdHex: string;
  readonly verb: "start" | "join";
}

export type LinkCallStartDecision =
  | { readonly kind: "start" }
  /** This very call is already starting / waiting / live here: show it, never a
   *  second session for it. */
  | { readonly kind: "already-in-this-call" }
  | { readonly kind: "busy"; readonly message: CallText };

/** Whether a link call may start now. One call at a time, across all kinds. */
export function decideLinkCallStart(input: LinkCallStartInput): LinkCallStartDecision {
  if (input.occupant === null) return { kind: "start" };
  if (input.occupant === "link" && input.activeLinkCallIdHex === input.requestedCallIdHex) {
    return { kind: "already-in-this-call" };
  }
  return { kind: "busy", message: callBusyMessage(input.occupant, input.verb) };
}

/** A status line for a link call on a screen other than its `/c/` route. */
export interface LinkCallNotice {
  readonly text: string;
  /** `open` returns to the link's route; `cancel` leaves the call;
   *  `dismiss` forgets an outcome. */
  readonly actions: readonly ("open" | "cancel" | "dismiss")[];
}

/** Who this device is in the call. */
export type LinkCallRole = "owner" | "joiner";

/**
 * Whether the in-call overlay is up, which is the one rule deciding both what
 * `CallHostView` mounts and when the notice below must stay silent. Two copies
 * of it would drift into a banner under a call screen saying the same thing.
 *
 * Live for either role. Also from `starting` for a founder: `New call` is a
 * button that promised a call screen (`docs/ux/call-links.md` section 5), and
 * a founder has nothing to wait for but its own session. A joiner keeps its
 * notice until it is admitted — it is not in a call yet, and the notice says
 * that better than a connecting screen would.
 */
export function linkCallOverlayShown(
  phase: LinkCallPhase | null,
  role: LinkCallRole | null,
): boolean {
  if (phase === null) return false;
  if (phase === "live") return true;
  return phase === "starting" && role === "owner";
}

/**
 * What to show about a link call away from its route, or `null`.
 *
 * Nothing while the `/c/` route is showing the call itself, and nothing while
 * the in-call overlay is up. Before the call is live the user can move around
 * (the admission wait can take a while), so they are told it is still going and
 * can cancel it; after it ends they are told why once, since the route that
 * would have said so is gone.
 */
export function linkCallNotice(
  phase: LinkCallPhase | null,
  role: LinkCallRole | null,
  routeShowingIt: boolean,
  errorMessage: CallText | null,
): LinkCallNotice | null {
  if (phase === null || routeShowingIt) return null;
  if (linkCallOverlayShown(phase, role)) return null;
  switch (phase) {
    case "live":
      return null;
    case "starting":
      return { text: t("call.link.notice.connecting"), actions: ["open", "cancel"] };
    case "waiting":
      return { text: t("call.link.waiting"), actions: ["open", "cancel"] };
    case "ended":
      return { text: t("call.link.ended"), actions: ["dismiss"] };
    case "dead":
      return { text: t("call.link.dead"), actions: ["dismiss"] };
    case "error":
      return {
        text:
          errorMessage === null
            ? t("call.link.error.generic")
            : t("common.sentence_pair", {
                first: t("call.link.error.generic"),
                second: errorMessage(),
              }),
        actions: ["dismiss"],
      };
  }
}

/**
 * What this device knows about one link-call attempt, for its row in the
 * Calls list (`docs/ux/navigation.md` section 5.1.1).
 */
export interface LinkCallJournal {
  /** Lowercase hex of the call id: the row's id, one row per call. */
  readonly callIdHex: string;
  readonly role: LinkCallRole;
  readonly video: boolean;
  /** When this device got past the join step (the slot went `live`), or
   *  `null` while it has not. The row's time. */
  liveAtMs: number | null;
  /** The row was written for this attempt. */
  recorded: boolean;
}

/** Inputs to {@link linkCallHistoryEntry} that are read at the moment the
 *  call is over for this device. */
export interface LinkCallEndFacts {
  readonly endedAtMs: number;
  /** The signed-in account is a guest (ADR-0032): a guest never lands in the
   *  app, so it keeps no history. */
  readonly isGuest: boolean;
}

/**
 * The Calls-list row a link call leaves on this device, or `null` when it
 * leaves none.
 *
 * None when this device never got past the join step (a founder that never
 * came up, a joiner never admitted): it was not in the call. None for a guest
 * account. None when the row was already written for this attempt — the call
 * is over for this device once, whichever of leaving and ending came first.
 *
 * `outgoing` for a call this device founded (`New call`, or the link's owner
 * opening its own link), `incoming` for one it joined from a link. The time is
 * when this device got past the join step, the duration how long it was in
 * the call.
 */
export function linkCallHistoryEntry(
  journal: LinkCallJournal,
  facts: LinkCallEndFacts,
): StandaloneCallEntry | null {
  if (journal.recorded || journal.liveAtMs === null || facts.isGuest) return null;
  return {
    id: journal.callIdHex,
    outcome: journal.role === "owner" ? "outgoing" : "incoming",
    video: journal.video,
    startedAtMs: journal.liveAtMs,
    durationMs: Math.max(0, facts.endedAtMs - journal.liveAtMs),
  };
}

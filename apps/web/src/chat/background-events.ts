// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Where each event the background receive decrypted goes.
 *
 * A row the background receive applies can never be decrypted again, so every
 * event kind needs a destination that does not depend on the chat being open —
 * or a hand-off to the chat view that survives until it is. This is the table,
 * kept pure so it can be tested (vitest runs in node without the Solid
 * transform) and so the provider cannot quietly drop a kind a later ADR adds:
 * an unknown kind is a compile error in the exhaustive switch below.
 *
 *   | kind                  | settled where it is received  | handed to the chat view |
 *   |-----------------------|-------------------------------|-------------------------|
 *   | application, system   | persisted to the message log  | yes (timeline)          |
 *   | op                    | —                             | yes (edit / delete /    |
 *   |                       |                               | reaction / typing /     |
 *   |                       |                               | read-receipt overlays)  |
 *   | externalJoinRequest   | —                             | yes (members panel)     |
 *   | membershipProposal    | observation recorded if stored| yes (panel refresh)     |
 *   | callSignaling         | the app-level call host       | no                      |
 *   | profile               | the profile store             | no                      |
 *   | pushRoutingSecret     | the gated receive             | no                      |
 *   | committerBeacon       | the beacon observation store  | no                      |
 *
 * The overlays (`op`) are in-memory state in the chat view and were never
 * persisted — a reload always started them empty — so handing them over
 * matches what the chat would have shown had it been open this session.
 */
import type { InboundSignalingRow } from "../calls/call-signaling-dispatch";
import type { IncomingChatEvent } from "./client";

type Of<K extends IncomingChatEvent["kind"]> = Extract<IncomingChatEvent, { kind: K }>;

export interface BackgroundEventPlan {
  /** Rows filed into the persisted timeline. */
  readonly timeline: readonly Of<"application" | "system">[];
  /** Events handed to the chat view (now, or when it next mounts). */
  readonly toView: readonly IncomingChatEvent[];
  /** Call signaling for the call host, in journal order. */
  readonly signaling: readonly InboundSignalingRow[];
  readonly profiles: readonly Of<"profile">[];
  readonly pushSecrets: readonly Of<"pushRoutingSecret">[];
  readonly beacons: readonly Of<"committerBeacon">[];
  /** Proposals the receipt gate STORED; a refused one is a pending request
   *  nowhere. */
  readonly storedProposals: readonly Of<"membershipProposal">[];
  /** The batch carried a commit, so the roster may have changed (ADR-0043). */
  readonly sawCommit: boolean;
}

/** Split a batch per the table in the module header. Order within each list is
 *  journal order. */
export function planBackgroundEvents(events: readonly IncomingChatEvent[]): BackgroundEventPlan {
  const timeline: Of<"application" | "system">[] = [];
  const toView: IncomingChatEvent[] = [];
  const signaling: InboundSignalingRow[] = [];
  const profiles: Of<"profile">[] = [];
  const pushSecrets: Of<"pushRoutingSecret">[] = [];
  const beacons: Of<"committerBeacon">[] = [];
  const storedProposals: Of<"membershipProposal">[] = [];
  let sawCommit = false;
  for (const e of events) {
    switch (e.kind) {
      case "application":
        timeline.push(e);
        toView.push(e);
        break;
      case "system":
        timeline.push(e);
        toView.push(e);
        if (e.systemKind === "commit") sawCommit = true;
        break;
      case "op":
      case "externalJoinRequest":
        toView.push(e);
        break;
      case "membershipProposal":
        if (e.stored) storedProposals.push(e);
        toView.push(e);
        break;
      case "callSignaling":
        signaling.push({
          messageId: e.messageId,
          createdAt: e.createdAt,
          payloadBytes: e.payloadBytes,
          ...(e.senderLeaf === undefined ? {} : { senderLeaf: e.senderLeaf }),
        });
        break;
      case "profile":
        profiles.push(e);
        break;
      case "pushRoutingSecret":
        pushSecrets.push(e);
        break;
      case "committerBeacon":
        beacons.push(e);
        break;
      default: {
        const unhandled: never = e;
        throw new Error(
          `background receive: unhandled event kind ${(unhandled as { kind: string }).kind}`,
        );
      }
    }
  }
  return { timeline, toView, signaling, profiles, pushSecrets, beacons, storedProposals, sawCommit };
}

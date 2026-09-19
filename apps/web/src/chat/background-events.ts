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
 *   | op (read-receipt)     | the read-watermark store      | yes (idempotent)        |
 *   | op                    | —                             | yes (edit / delete /    |
 *   |                       |                               | reaction / typing /     |
 *   |                       |                               | read-receipt overlays)  |
 *   | externalJoinRequest   | —                             | yes (members panel)     |
 *   | membershipProposal    | observation recorded if stored| yes (panel refresh)     |
 *   | callSignaling         | the app-level call host       | no                      |
 *   | profile               | the profile store             | no                      |
 *   | profileRequest        | the profile publish scheduler | no                      |
 *   | pushRoutingSecret     | the gated receive             | no                      |
 *   | committerBeacon       | the beacon observation store  | no                      |
 *   | group-name            | the gated group-name store    | no                      |
 *
 * The edit / delete / reaction / typing overlays are in-memory state in the
 * chat view and were never persisted — a reload always started them empty —
 * so handing them over matches what the chat would have shown had it been
 * open this session. Read receipts are the exception: they are durable
 * (`read-watermark-store.ts`) and settled here, because a receipt the chat
 * view never sees would otherwise leave a read message showing "Sent".
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
  /** ADR-0048 §2 trigger 4 — requests to introduce ourselves; the provider
   *  answers each (from another member) with a throttled profile publish. */
  readonly profileRequests: readonly Of<"profileRequest">[];
  readonly pushSecrets: readonly Of<"pushRoutingSecret">[];
  readonly beacons: readonly Of<"committerBeacon">[];
  /** ADR-0024 — read receipts, for the durable watermark store. Also in
   *  `toView`; the store's fold is monotone, so the second ingest is a no-op. */
  readonly readReceipts: readonly Of<"op">[];
  /** ADR-0051 — shared group names, for the role-gated receive. */
  readonly groupNames: readonly Of<"group-name">[];
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
  const profileRequests: Of<"profileRequest">[] = [];
  const pushSecrets: Of<"pushRoutingSecret">[] = [];
  const beacons: Of<"committerBeacon">[] = [];
  const groupNames: Of<"group-name">[] = [];
  const readReceipts: Of<"op">[] = [];
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
        if (e.op === "read-receipt") readReceipts.push(e);
        toView.push(e);
        break;
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
          ...(e.senderMemberKey === undefined ? {} : { senderMemberKey: e.senderMemberKey }),
          ...(e.senderKey === undefined ? {} : { senderPersonKey: e.senderKey }),
        });
        break;
      case "profile":
        profiles.push(e);
        break;
      case "profileRequest":
        profileRequests.push(e);
        break;
      case "pushRoutingSecret":
        pushSecrets.push(e);
        break;
      case "committerBeacon":
        beacons.push(e);
        break;
      case "group-name":
        groupNames.push(e);
        break;
      default: {
        const unhandled: never = e;
        throw new Error(
          `background receive: unhandled event kind ${(unhandled as { kind: string }).kind}`,
        );
      }
    }
  }
  return {
    timeline,
    toView,
    signaling,
    profiles,
    profileRequests,
    pushSecrets,
    beacons,
    readReceipts,
    groupNames,
    storedProposals,
    sawCommit,
  };
}

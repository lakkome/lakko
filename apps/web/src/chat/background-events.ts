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
 *   | op (edit/delete/      | folded into the stored rows   | yes (overlay)           |
 *   |     reaction)         |                               |                         |
 *   | op (typing)           | —                             | yes (overlay)           |
 *   | externalJoinRequest   | —                             | yes (members panel)     |
 *   | membershipProposal    | observation recorded if stored| yes (panel refresh)     |
 *   | callSignaling         | the app-level call host       | no                      |
 *   | profile               | the profile store             | no                      |
 *   | profileRequest        | the profile publish scheduler | no                      |
 *   | pushRoutingSecret     | the gated receive             | no                      |
 *   | committerBeacon       | the beacon observation store  | no                      |
 *   | group-name            | the gated group-name store    | no                      |
 *   | pin                   | the gated pin store           | no                      |
 *   | shared-channels       | the shared-channels store     | no                      |
 *   | serviceEnvelope       | dropped (never rendered)      | no                      |
 *
 * Typing is in-memory state in the chat view, so handing it over matches what
 * the chat would have shown had it been open this session. Read receipts are
 * durable (`read-watermark-store.ts`) and settled here, because a receipt the
 * chat view never sees would otherwise leave a read message showing "Sent".
 *
 * Edit / delete / reaction used to be handed over the same way as typing, and
 * that was wrong: they are not status, they are the message. The view that
 * would have applied them may never mount, the buffer holding them does not
 * survive a reload, and MLS forward secrecy means the op rows can never be
 * decrypted again - so a message deleted while its chat was closed came back
 * permanently. They are now also settled here, against the stored rows
 * (`messageOps`, folded by `message-ops-store.ts`), and still handed to the
 * view, whose own fold is idempotent for a delete and an edit. A reaction
 * TOGGLES, so the view must not apply one it has already seen; it does not,
 * because an event delivered while a view is attached never enters the
 * buffer it would later replay.
 */
import type { InboundSignalingRow } from "../calls/call-signaling-dispatch";
import type { IncomingChatEvent } from "./client";

type Of<K extends IncomingChatEvent["kind"]> = Extract<IncomingChatEvent, { kind: K }>;

/** An `op` event that mutates a message, as opposed to typing or a receipt. */
export type MessageOpEvent = Of<"op"> & { op: "edit" | "delete" | "reaction" };

/** Narrows an op event to the three that change a message. A predicate rather
 *  than an inline test because `op` is a union of literals on ONE object type,
 *  not a discriminant between several, so testing it narrows the property and
 *  not the event. */
function isMessageOp(e: Of<"op">): e is MessageOpEvent {
  return e.op === "edit" || e.op === "delete" || e.op === "reaction";
}

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
  /** ADR-0023 — edit / delete / reaction, for the stored rows. Also in
   *  `toView`, for a chat view that is attached now or mounts later.
   *  Narrowed to the three message ops so a consumer cannot be handed a
   *  typing signal to fold into a transcript. */
  readonly messageOps: readonly MessageOpEvent[];
  /** ADR-0051 — shared group names, for the role-gated receive. */
  readonly groupNames: readonly Of<"group-name">[];
  /** ADR-0087 — the group photo, allowed reactions and handle, for the
   *  role-gated receive beside the name. */
  readonly groupSettings: readonly Of<"group-setting">[];
  /** ADR-0073 R.6 — successor claims, for the claim rule. */
  readonly groupClosed: readonly Of<"group-closed">[];
  /** ADR-0052 decision 3 — shared pins, for the role-gated receive. */
  readonly pins: readonly Of<"pin">[];
  /** ADR-0059 section 6 — members' shared channel lists. */
  readonly sharedChannels: readonly Of<"shared-channels">[];
  /** ADR-0060 — scoped-discovery service envelopes, for their handlers. */
  readonly scoped: readonly Of<"scopedDiscovery">[];
  /** Proposals the receipt gate STORED; a refused one is a pending request
   *  nowhere. */
  readonly storedProposals: readonly Of<"membershipProposal">[];
  /** The batch carried a commit, so the roster may have changed (ADR-0043). */
  readonly sawCommit: boolean;
}

type ServiceEnvelopeEvent = Of<
  | "profile"
  | "profileRequest"
  | "pushRoutingSecret"
  | "committerBeacon"
  | "group-name"
  | "group-setting"
  | "group-closed"
  | "pin"
  | "shared-channels"
  | "scopedDiscovery"
>;

/** The control and service envelopes a member posts (never a timeline row). */
function isServiceEnvelopeKind(e: IncomingChatEvent): e is ServiceEnvelopeEvent {
  switch (e.kind) {
    case "profile":
    case "profileRequest":
    case "pushRoutingSecret":
    case "committerBeacon":
    case "group-name":
    case "group-setting":
    case "group-closed":
    case "pin":
    case "shared-channels":
    case "scopedDiscovery":
      return true;
    default:
      return false;
  }
}

/** The inert-sender question of {@link dropsFromSupersededSender}. */
export interface InertSender {
  /** `reset-repair.ts` `supersededSenderInert` for this conversation. */
  readonly sender: (senderKeyB64: string) => boolean;
  /** The PERSON key at a leaf, for the two envelope kinds that carry a leaf
   *  only (the push-routing secret, the committer beacon). */
  readonly senderKeyOfLeaf?: (leaf: number) => string | undefined;
}

/**
 * ADR-0073 R.5 / R.8 — whether an event is dropped because of WHO sent it.
 * The one rule both consumers apply (the background settle through
 * {@link planBackgroundEvents}, the open chat's own poll):
 *
 * | event                                         | dropped when its sender ...                          |
 * |-----------------------------------------------|------------------------------------------------------|
 * | a chat row, an op, a call signal              | is a key an accepted reset retired (any conversation)|
 * | a control or service envelope                 | is inert in this conversation (`InertSender`)        |
 * | a commit, a proposal, an external-join request| never: MLS state, already applied                    |
 */
export function dropsFromSupersededSender(
  e: IncomingChatEvent,
  senderSuperseded: (senderKeyB64: string) => boolean,
  inert: InertSender = { sender: () => false },
): boolean {
  if (e.kind === "application" || e.kind === "op" || e.kind === "callSignaling") {
    return e.senderKey !== undefined && senderSuperseded(e.senderKey);
  }
  if (!isServiceEnvelopeKind(e)) return false;
  const sender =
    "senderKey" in e && e.senderKey !== undefined
      ? e.senderKey
      : e.senderLeaf !== undefined
        ? inert.senderKeyOfLeaf?.(e.senderLeaf)
        : undefined;
  return sender !== undefined && inert.sender(sender);
}

/** Split a batch per the table in the module header. Order within each list is
 *  journal order. */
export function planBackgroundEvents(
  events: readonly IncomingChatEvent[],
  /** ADR-0073 R.5 / R.8 — whether a sender's ACCOUNT key was retired by an
   *  accepted reset. A message, an op or a call signal from such a sender (a
   *  removed device still holding the old keys) is dropped here, before it
   *  is stored, shown, notified or rung. Commits and every other row are
   *  not judged by this. */
  senderSuperseded: (senderKeyB64: string) => boolean = () => false,
  /** ADR-0073 R.7 case 1 — whether a message is a RE-SEND whose original
   *  this device already holds (`resend-marker.ts` `resendVerdict` said
   *  `skip`). Such a message is dropped here, before it is stored, shown,
   *  counted or notified: the history row is the one copy. */
  resendAlreadyHeld: (e: Of<"application">) => boolean = () => false,
  /** ADR-0073 R.8 — whether NOTHING from a sender acts in this conversation
   *  (see {@link dropsFromSupersededSender}). */
  inert: InertSender = { sender: () => false },
): BackgroundEventPlan {
  const timeline: Of<"application" | "system">[] = [];
  const toView: IncomingChatEvent[] = [];
  const signaling: InboundSignalingRow[] = [];
  const profiles: Of<"profile">[] = [];
  const profileRequests: Of<"profileRequest">[] = [];
  const pushSecrets: Of<"pushRoutingSecret">[] = [];
  const beacons: Of<"committerBeacon">[] = [];
  const groupNames: Of<"group-name">[] = [];
  const groupSettings: Of<"group-setting">[] = [];
  const groupClosed: Of<"group-closed">[] = [];
  const pins: Of<"pin">[] = [];
  const sharedChannels: Of<"shared-channels">[] = [];
  const scoped: Of<"scopedDiscovery">[] = [];
  const readReceipts: Of<"op">[] = [];
  const messageOps: MessageOpEvent[] = [];
  const storedProposals: Of<"membershipProposal">[] = [];
  let sawCommit = false;
  for (const e of events) {
    if (dropsFromSupersededSender(e, senderSuperseded, inert)) continue;
    if (e.kind === "application" && e.resendOrigin !== undefined && resendAlreadyHeld(e)) {
      continue;
    }
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
        if (isMessageOp(e)) messageOps.push(e);
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
      case "group-setting":
        groupSettings.push(e);
        break;
      case "group-closed":
        groupClosed.push(e);
        break;
      case "pin":
        pins.push(e);
        break;
      case "shared-channels":
        sharedChannels.push(e);
        break;
      case "scopedDiscovery":
        scoped.push(e);
        break;
      case "registrarSeed":
      case "groupContactRequest":
        // ADR-0057 / who-can-find-me 6.4 — consumed at receipt by the chat
        // client and never handed on; a stray one is dropped here rather than
        // rendered or notified.
        break;
      case "serviceEnvelope":
        // An unrecognised `lakko/…` service envelope: dropped at receipt by
        // the chat client; a stray one is dropped here, never rendered.
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
    messageOps,
    groupNames,
    groupSettings,
    groupClosed,
    pins,
    sharedChannels,
    scoped,
    storedProposals,
    sawCommit,
  };
}

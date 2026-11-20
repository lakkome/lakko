// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The chat of a group call (`docs/ux/calls.md` section 4.1.4,
 * `lakko_calls::call_chat`).
 *
 * Text messages between the people in the call, for as long as the call
 * lasts. A message rides the sealed SFU relay (ADR-0078 section 2), signed
 * with the sender's MLS leaf key, and a receiver shows it only once it
 * verified against the leaf key that peer's announcement verified with.
 * Nothing is kept: the chat belongs to the call's session and is cleared when
 * the call ends for this device.
 *
 * {@link CallChatChannel} is the per-call state, living in the
 * `SfuCallSession` beside the call-name channel (`call-name.ts`):
 *
 * - this device's messages, numbered 1, 2, 3, ... for the life of its SFU
 *   peer id; a message that could not be sent keeps its number and its bytes,
 *   and `Send again` sends those same bytes;
 * - a message from a peer whose announcement has not verified is held (at
 *   most {@link MAX_PENDING_PER_PEER} per peer, oldest dropped) and verified
 *   once it does; one from a peer that never verifies is never shown;
 * - each `(peer, seq)` is shown once: a relay frame re-sent after a reconnect
 *   does not show twice;
 * - a peer leaving does not take its messages away.
 *
 * Unlike the call name, a chat message is never re-sent on a re-announcement.
 *
 * {@link callChatState} / {@link callChatActions} are the small store the
 * call screen reads.
 *
 * Pure apart from the Solid signals of the store: no wasm, no network.
 */
import { createSignal } from "solid-js";

import {
  CHAT_PREVIEW_MS,
  initialChatPanel,
  panelClose,
  panelExpirePreview,
  panelIncoming,
  panelOpen,
  panelOwnSent,
  panelScrolled,
  type ChatPanelState,
} from "./call-chat-panel-state";

/** Messages held per peer before its announcement verified. */
export const MAX_PENDING_PER_PEER = 50;

/** Messages kept in one call's chat; the oldest go first. Chosen, not
 *  measured: a bound so a peer flooding the relay cannot grow memory without
 *  limit, far above what a call's chat holds. */
export const MAX_CALL_CHAT_MESSAGES = 1000;

/** What a message on screen is. */
export type CallChatDelivery = "sent" | "not-sent";

/** One message in the call's chat. */
export interface CallChatMessage {
  /** Unique in the call: `own:<seq>` for this device's, `<peerId>:<seq>`
   *  for others'. */
  readonly id: string;
  /** Written on this device. */
  readonly own: boolean;
  /** The sender's SFU peer id; `null` for this device's own messages. */
  readonly peerId: string | null;
  readonly seq: bigint;
  readonly text: string;
  /** When the message arrived on this device (or was written here). */
  readonly arrivedAtMs: number;
  /** Always `sent` for others' messages. */
  readonly delivery: CallChatDelivery;
}

/** What the channel needs from the call's announcer (`call-relay.ts`). */
export interface CallChatSigner {
  /** This device's signed message as `selfPeerId`, numbered `seq`, or `null`
   *  when it cannot make one yet (no group). */
  composeCallChat?(selfPeerId: string, seq: bigint, text: string): Uint8Array | null;
  /** A message from `from`, verified against the leaf key its announcement
   *  verified with, or `null`. */
  verifyCallChat?(
    from: string,
    signerKey: Uint8Array,
    bytes: Uint8Array,
  ): { readonly seq: bigint; readonly text: string } | null;
}

/** What the channel is plugged into. */
export interface CallChatChannelDeps {
  readonly signer: CallChatSigner | undefined;
  /** Seal and send a relay plaintext to the call; `false` when it could not
   *  (no relay key, no socket). */
  sendSealed(plaintext: Uint8Array): boolean;
  /** This device's SFU peer id, once the SFU's Welcome named it. */
  selfPeerId(): string | null;
  now(): number;
  /** The whole list changed (a message added, a delivery state changed, the
   *  chat cleared). */
  onChange(messages: readonly CallChatMessage[]): void;
  /** A message from someone else is now shown. */
  onIncoming(message: CallChatMessage): void;
  /** One plain line, never with text or ids in it. */
  log?(line: string): void;
}

interface Held {
  readonly bytes: Uint8Array;
  readonly atMs: number;
}

export class CallChatChannel {
  private ownSeq = 0n;
  private messages: CallChatMessage[] = [];
  /** The bytes of this device's messages, by seq, kept so `Send again` sends
   *  exactly those. */
  private readonly ownWires = new Map<bigint, Uint8Array>();
  private readonly peerKeys = new Map<string, Uint8Array>();
  private readonly pending = new Map<string, Held[]>();
  /** The `(peer, seq)` pairs already shown. */
  private readonly seen = new Map<string, Set<bigint>>();

  constructor(private readonly deps: CallChatChannelDeps) {}

  /** The messages on screen, oldest first. */
  list(): readonly CallChatMessage[] {
    return this.messages;
  }

  /**
   * Send `text` to everyone in the call. `text` already passed the core's
   * rule (`checkCallChatText`). The message is shown at once, as sent or as
   * not sent when it could not leave the device.
   */
  send(text: string): CallChatMessage {
    this.ownSeq += 1n;
    const seq = this.ownSeq;
    const sent = this.transmit(seq, text);
    const message: CallChatMessage = {
      id: `own:${seq}`,
      own: true,
      peerId: null,
      seq,
      text,
      arrivedAtMs: this.deps.now(),
      delivery: sent ? "sent" : "not-sent",
    };
    this.push(message);
    this.deps.onChange(this.messages);
    return message;
  }

  /** Send a message that was not sent again: the same number and, once it
   *  was composed, the same bytes. Returns whether it left the device. */
  sendAgain(id: string): boolean {
    const at = this.messages.findIndex((m) => m.id === id);
    const message = this.messages[at];
    if (message === undefined || !message.own || message.delivery === "sent") return false;
    if (!this.transmit(message.seq, message.text)) return false;
    this.messages = [...this.messages];
    this.messages[at] = { ...message, delivery: "sent" };
    this.deps.onChange(this.messages);
    return true;
  }

  /** `peerId`'s announcement verified with `signerKey`: messages it sent
   *  before are verified now, in the order they arrived. */
  onPeerIdentified(peerId: string, signerKey: Uint8Array): void {
    this.peerKeys.set(peerId, signerKey);
    const held = this.pending.get(peerId);
    if (held === undefined) return;
    this.pending.delete(peerId);
    for (const h of held) this.verifyAndShow(peerId, signerKey, h.bytes, h.atMs);
  }

  /** A sealed call-chat message from `peerId` opened. */
  onReceived(peerId: string, bytes: Uint8Array): void {
    if (peerId === this.deps.selfPeerId()) return;
    const at = this.deps.now();
    const key = this.peerKeys.get(peerId);
    if (key === undefined) {
      const held = this.pending.get(peerId) ?? [];
      held.push({ bytes, atMs: at });
      while (held.length > MAX_PENDING_PER_PEER) held.shift();
      this.pending.set(peerId, held);
      return;
    }
    this.verifyAndShow(peerId, key, bytes, at);
  }

  /** `peerId` left: what it already said stays; what it never got to prove
   *  goes. */
  onPeerLeft(peerId: string): void {
    this.peerKeys.delete(peerId);
    this.pending.delete(peerId);
  }

  /** The call ended for this device: every message goes. */
  clear(): void {
    const hadAny = this.messages.length > 0;
    this.messages = [];
    this.ownWires.clear();
    this.peerKeys.clear();
    this.pending.clear();
    this.seen.clear();
    if (hadAny) this.deps.onChange(this.messages);
  }

  private transmit(seq: bigint, text: string): boolean {
    let wire = this.ownWires.get(seq) ?? null;
    if (wire === null) {
      const self = this.deps.selfPeerId();
      if (self === null) return false;
      wire = this.deps.signer?.composeCallChat?.(self, seq, text) ?? null;
      if (wire === null) return false;
      this.ownWires.set(seq, wire);
    }
    return this.deps.sendSealed(wire);
  }

  private verifyAndShow(peerId: string, key: Uint8Array, bytes: Uint8Array, atMs: number): void {
    const verified = this.deps.signer?.verifyCallChat?.(peerId, key, bytes) ?? null;
    if (verified === null) {
      this.deps.log?.("call chat: a message did not verify and was dropped");
      return;
    }
    let seen = this.seen.get(peerId);
    if (seen === undefined) {
      seen = new Set();
      this.seen.set(peerId, seen);
    }
    if (seen.has(verified.seq)) return;
    seen.add(verified.seq);
    const message: CallChatMessage = {
      id: `${peerId}:${verified.seq}`,
      own: false,
      peerId,
      seq: verified.seq,
      text: verified.text,
      arrivedAtMs: atMs,
      delivery: "sent",
    };
    this.push(message);
    this.deps.onIncoming(message);
    this.deps.onChange(this.messages);
  }

  private push(message: CallChatMessage): void {
    const next = [...this.messages, message];
    while (next.length > MAX_CALL_CHAT_MESSAGES) {
      const dropped = next.shift();
      if (dropped?.own === true) this.ownWires.delete(dropped.seq);
    }
    this.messages = next;
  }
}

// ============================================================================
// The store the call screen reads
// ============================================================================

/** What the call screen can do with the current call's chat. */
export interface CallChatControl {
  /** Send `text` (already passed `checkCallChatText`). */
  send(text: string): void;
  /** Send a message that was not sent again. */
  sendAgain(id: string): void;
}

const [control, setControl] = createSignal<CallChatControl | null>(null);
const [messages, setMessages] = createSignal<readonly CallChatMessage[]>([]);
const [panel, setPanel] = createSignal<ChatPanelState>(initialChatPanel());
/** The last name each sender's tile showed, so a bubble keeps a name after
 *  the person left the call. */
const lastNames = new Map<string, string>();
let previewTimer: ReturnType<typeof setTimeout> | null = null;

/** The call screen's view of the chat. `control` is `null` on a call without
 *  one (a one-to-one call, before the SFU session). */
export const callChatState = {
  control,
  messages,
  panel,
  /** The name a sender's tile last showed, or `undefined`. */
  lastName(peerId: string): string | undefined {
    return lastNames.get(peerId);
  },
} as const;

function clearPreviewTimer(): void {
  if (previewTimer !== null) clearTimeout(previewTimer);
  previewTimer = null;
}

/** Written by the call adapter that owns the current call, and by the call
 *  screen for the panel. */
export const callChatActions = {
  setControl,
  setMessages,
  /** A message from someone else is shown: the count and the preview while
   *  the panel is closed, the pill while it is open and scrolled up. */
  incoming(message: CallChatMessage): void {
    const now = Date.now();
    const next = panelIncoming(panel(), message, now);
    setPanel(next);
    if (next.preview !== null && next.preview.messageId === message.id) {
      clearPreviewTimer();
      previewTimer = setTimeout(() => {
        previewTimer = null;
        setPanel((p) => panelExpirePreview(p, Date.now()));
      }, CHAT_PREVIEW_MS);
    }
  },
  open(): void {
    clearPreviewTimer();
    setPanel((p) => panelOpen(p));
  },
  close(): void {
    setPanel((p) => panelClose(p));
  },
  scrolled(atBottom: boolean): void {
    const p = panel();
    const next = panelScrolled(p, atBottom);
    if (next !== p) setPanel(next);
  },
  ownSent(): void {
    setPanel((p) => panelOwnSent(p));
  },
  rememberName(peerId: string, name: string): void {
    if (name !== "") lastNames.set(peerId, name);
  },
  reset(): void {
    clearPreviewTimer();
    setControl(null);
    setMessages([]);
    setPanel(initialChatPanel());
    lastNames.clear();
  },
} as const;

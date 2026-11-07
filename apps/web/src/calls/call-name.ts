// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The name a participant gives themself for one call
 * (`docs/ux/calls.md` section 4.1, 2026-11-07; `lakko_calls::call_name`).
 *
 * Any participant may change the name the others see on their tile, for
 * this call only. The profile name never changes. The name rides the sealed
 * SFU relay, signed with the sender's MLS leaf key, and a receiver takes it
 * only from the peer whose verified announcement named that leaf key.
 *
 * {@link CallNameChannel} is the per-call state: this device's own name and
 * its sequence number (re-sent every time the session re-announces itself,
 * so a late joiner learns it), and the newest name received per peer.
 *
 * {@link callNameState} / {@link callNameActions} are the small store the
 * call screen reads: whether the current call can carry a call name, the
 * own name, and the default the field shows as its placeholder.
 *
 * Pure apart from the Solid signals of the store: no wasm, no network.
 */
import { createSignal } from "solid-js";

/** What the channel needs from the call's announcer (`call-relay.ts`). */
export interface CallNameSigner {
  /** This device's signed call name as `selfPeerId`, numbered `seq`, or
   *  `null` when it cannot make one yet (no group). */
  composeCallName?(
    selfPeerId: string,
    seq: bigint,
    name: string,
  ): Uint8Array | null;
  /** A call name from `from`, verified against the leaf key its
   *  announcement verified with, or `null`. */
  verifyCallName?(
    from: string,
    signerKey: Uint8Array,
    bytes: Uint8Array,
  ): { readonly seq: bigint; readonly name: string | null } | null;
}

/** What the channel is plugged into. */
export interface CallNameChannelDeps {
  readonly signer: CallNameSigner | undefined;
  /** Seal and send a relay plaintext to the call; `false` when it could
   *  not (no relay key, no socket). */
  sendSealed(plaintext: Uint8Array): boolean;
  /** This device's SFU peer id, once the SFU's Welcome named it. */
  selfPeerId(): string | null;
  /** A peer's call name changed: the name, or `null` when they went back
   *  to their default naming (or left). */
  onName(peerId: string, name: string | null): void;
}

/** Received names held per peer: bounded by the roster, and a peer whose
 *  announcement has not verified keeps only its latest payload. */
export class CallNameChannel {
  private ownName = "";
  private ownSeq = 0n;
  private readonly peerKeys = new Map<string, Uint8Array>();
  private readonly pending = new Map<string, Uint8Array>();
  private readonly seqs = new Map<string, bigint>();

  constructor(private readonly deps: CallNameChannelDeps) {}

  /** The name this device set for the call (`""` = none). */
  ownCallName(): string {
    return this.ownName;
  }

  /**
   * Set this device's name for the call. `name` is already normalised (the
   * display-name rule, trimmed); `""` goes back to the default. Returns
   * whether it changed. Sent at once when possible, and again with every
   * re-announcement.
   */
  setOwnName(name: string): boolean {
    // Nothing to send: the same name again, or "" before any name was set.
    if (name === this.ownName) return false;
    this.ownName = name;
    this.ownSeq += 1n;
    this.resend();
    return true;
  }

  /** Send the current own name again (after the Welcome, a rekey, a join,
   *  the periodic announcement). Nothing before a name was ever set. */
  resend(): void {
    if (this.ownSeq === 0n) return;
    const self = this.deps.selfPeerId();
    if (self === null) return;
    const wire =
      this.deps.signer?.composeCallName?.(self, this.ownSeq, this.ownName) ??
      null;
    if (wire === null) return;
    this.deps.sendSealed(wire);
  }

  /** `peerId`'s announcement verified with `signerKey`: a name it sent
   *  before is read now. */
  onPeerIdentified(peerId: string, signerKey: Uint8Array): void {
    this.peerKeys.set(peerId, signerKey);
    const held = this.pending.get(peerId);
    if (held === undefined) return;
    this.pending.delete(peerId);
    this.onReceived(peerId, held);
  }

  /** A sealed call name from `peerId` opened. */
  onReceived(peerId: string, bytes: Uint8Array): void {
    const key = this.peerKeys.get(peerId);
    if (key === undefined) {
      // Before its announcement verified: keep the latest one only.
      this.pending.set(peerId, bytes);
      return;
    }
    const verified =
      this.deps.signer?.verifyCallName?.(peerId, key, bytes) ?? null;
    if (verified === null) return;
    const stored = this.seqs.get(peerId);
    // `lakko_calls::call_name::call_name_is_newer`: strictly greater; a
    // re-send is not an update.
    if (stored !== undefined && verified.seq <= stored) return;
    this.seqs.set(peerId, verified.seq);
    this.deps.onName(peerId, verified.name);
  }

  /** `peerId` left: what it said goes with it. */
  onPeerLeft(peerId: string): void {
    this.peerKeys.delete(peerId);
    this.pending.delete(peerId);
    this.seqs.delete(peerId);
  }
}

/** What the call screen can do with the current call's name. */
export interface CallNameControl {
  /** Save `name` as this device's name for the call. `name` already passed
   *  the display-name rule (the core's `normalizeDisplayName`); `""` goes
   *  back to the default. */
  save(name: string): void;
}

const [control, setControl] = createSignal<CallNameControl | null>(null);
const [ownName, setOwnName] = createSignal("");
const [defaultName, setDefaultName] = createSignal("");

/** The call screen's view of the call name. `control` is `null` on a call
 *  that cannot carry one (a one-to-one call, before the SFU session). */
export const callNameState = {
  control,
  ownName,
  /** What others see when no call name is set (the profile name), for the
   *  field's placeholder; `""` when none. */
  defaultName,
} as const;

/** Written by the call adapter that owns the current call. */
export const callNameActions = {
  setControl,
  setOwnName,
  setDefaultName,
  reset(): void {
    setControl(null);
    setOwnName("");
    setDefaultName("");
  },
} as const;

/** Names this device set, per call id (hex), for a rejoin of the same call
 *  within this app run (`docs/ux/calls.md` section 4.1). Never stored. */
const rememberedNames = new Map<string, string>();

/** The call name this device set for `callIdHex` earlier in this app run. */
export function rememberedCallName(callIdHex: string): string {
  return rememberedNames.get(callIdHex) ?? "";
}

/** Remember (or forget, for `""`) this device's name for `callIdHex`. */
export function rememberCallName(callIdHex: string, name: string): void {
  if (name === "") rememberedNames.delete(callIdHex);
  else rememberedNames.set(callIdHex, name);
}

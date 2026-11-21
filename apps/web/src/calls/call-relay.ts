// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The sealed `signal-relay` channel of an SFU call and who a peer is
 * (ADR-0078 section 2).
 *
 * The SFU admits a peer by an anonymous token carrying a random peer id and
 * knows no account, so it can no longer say who a peer is. Each participant
 * says it itself: it announces its own SFU peer id, signed with its MLS leaf
 * key (`composeCallPeerAnnouncement`), sealed under a key derived from the
 * call seed (`CallRelayKey`), and every receiver checks the signature
 * against the leaf in the call's MLS roster (`verifyCallPeerAnnouncement`).
 * The leaf is account-bound (ADR-0050), so a verified announcement names
 * the person. The camera on/off media state rides the same sealed channel.
 *
 * The relay key changes with the call seed (every MLS epoch, ADR-0029); the
 * previous epoch's key is kept for opening, so a payload sealed just before
 * a rekey still opens.
 *
 * Everything that is not opened by a relay key (the link call's KeyPackage,
 * Welcome and MLS commits, which carry their own protection) is left to the
 * caller unchanged.
 */
import type { Wasm } from "../wasm";
import type {
  CallRelayKey,
  Group as WasmGroup,
  Identity as WasmIdentity,
  VerifiedCallScreenShare,
} from "../../wasm/lakko_ffi_wasm";
import { base64Encode } from "../auth/account-id";
import type { CallChatSigner } from "./call-chat";
import type { CallNameSigner } from "./call-name";

/** The wasm surface this module uses. */
export type CallRelayWasm = Pick<
  Wasm,
  | "CallRelayKey"
  | "classifyCallRelayPlaintext"
  | "composeCallPeerAnnouncement"
  | "verifyCallPeerAnnouncement"
  | "composeCallScreenShare"
  | "verifyCallScreenShare"
  | "composeCallName"
  | "verifyCallName"
  | "composeCallChat"
  | "composeCallChatPicture"
  | "verifyCallChat"
>;

/** What an opened relay payload carries. */
export type OpenedRelay =
  | { readonly kind: "announcement"; readonly bytes: Uint8Array }
  /** The sender's camera and microphone (`false` = muted). */
  | {
      readonly kind: "media-state";
      readonly cameraOn: boolean;
      readonly microphoneOn: boolean;
    }
  /** A signed screen-share state (ADR-0083), to verify against the sender's
   *  verified announcement. */
  | { readonly kind: "screen-share"; readonly state: Uint8Array }
  /** A signed name for this call (`call-name.ts`), to verify against the
   *  sender's verified announcement. */
  | { readonly kind: "call-name"; readonly bytes: Uint8Array }
  /** A signed message of the call's chat (`call-chat.ts`), to verify against
   *  the sender's verified announcement. */
  | { readonly kind: "call-chat"; readonly bytes: Uint8Array }
  | { readonly kind: "unknown" };

/** The relay keys of the current and the previous call epoch. */
export class CallRelay {
  private current: CallRelayKey | null = null;
  private previous: CallRelayKey | null = null;

  constructor(private readonly wasm: CallRelayWasm) {}

  /** Whether a seed has been set (a link joiner has none before its
   *  Welcome, and seals nothing until then). */
  hasKey(): boolean {
    return this.current !== null;
  }

  /** The call seed of a new epoch: the current key becomes the previous. */
  setSeed(seed: Uint8Array): void {
    let next: CallRelayKey;
    try {
      next = this.wasm.CallRelayKey.fromCallSeed(seed);
    } catch {
      // A seed that is not 32 bytes: the relay keeps what it had. The relay
      // is a naming and camera-state channel, never the media path, so this
      // never stands in the way of a rekey.
      return;
    }
    this.previous?.free();
    this.previous = this.current;
    this.current = next;
  }

  /** Seal a relay payload under the current key, or `null` with no key. */
  seal(plaintext: Uint8Array): Uint8Array | null {
    if (this.current === null) return null;
    try {
      return this.current.seal(plaintext);
    } catch {
      return null;
    }
  }

  /** Open a relay payload with the current, then the previous key; `null`
   *  when neither opens it (not a sealed relay payload, or another call). */
  open(sealed: Uint8Array): OpenedRelay | null {
    for (const key of [this.current, this.previous]) {
      if (key === null) continue;
      let plaintext: Uint8Array;
      try {
        plaintext = key.open(sealed);
      } catch {
        continue;
      }
      const classified = this.wasm.classifyCallRelayPlaintext(plaintext);
      try {
        switch (classified.kind) {
          case "peer-announcement":
            return { kind: "announcement", bytes: classified.announcement };
          case "media-state":
            return {
              kind: "media-state",
              cameraOn: classified.cameraOn,
              microphoneOn: classified.microphoneOn,
            };
          case "screen-share":
            return { kind: "screen-share", state: classified.state };
          case "call-name":
            return { kind: "call-name", bytes: classified.callName };
          case "call-chat":
            return { kind: "call-chat", bytes: classified.message };
          default:
            return { kind: "unknown" };
        }
      } finally {
        classified.free();
      }
    }
    return null;
  }

  /** Drop both keys. */
  dispose(): void {
    this.current?.free();
    this.previous?.free();
    this.current = null;
    this.previous = null;
  }
}

/** Who a verified announcement names. */
export interface VerifiedPeerIdentity {
  /** The person: base64 of the 32-byte account key (ADR-0050). */
  readonly personKeyB64: string;
  /** The signer's MLS leaf signature key, lower-case hex: the handle the
   *  link call's committer election keys members on. */
  readonly memberKeyHex: string;
}

/**
 * Composes this device's announcement and verifies everyone else's. The
 * group is read at each call, since the call's MLS group changes epoch (and
 * a link joiner has none before its Welcome).
 */
export interface PeerAnnouncer extends CallNameSigner, CallChatSigner {
  /** This device's announcement of `selfPeerId`, or `null` when it cannot
   *  make one yet. */
  compose(selfPeerId: string): Uint8Array | null;
  /** Who a verified announcement from `from` names, or `null` when it
   *  does not verify. */
  verify(from: string, announcement: Uint8Array): VerifiedPeerIdentity | null;
  /** This device's leaf signature key: the key its share states are folded
   *  under (ADR-0083). Absent: this call shares nothing. */
  ownSignerKey?(): Uint8Array;
  /** This device's signed share state as `selfPeerId`, or `null`. */
  composeScreenShare?(
    selfPeerId: string,
    seq: bigint,
    sharing: boolean,
    withSound: boolean,
  ): Uint8Array | null;
  /** A share state from `from`, verified against the leaf key its
   *  announcement verified with, or `null`. The caller owns (and frees) the
   *  returned object. */
  verifyScreenShare?(
    from: string,
    signerKey: Uint8Array,
    state: Uint8Array,
  ): VerifiedCallScreenShare | null;
}

/** The announcer for a call whose roster is `group()`. */
export function groupPeerAnnouncer(
  wasm: CallRelayWasm,
  identity: WasmIdentity,
  callId: Uint8Array,
  group: () => WasmGroup | null,
): PeerAnnouncer {
  return {
    compose: (selfPeerId) => {
      if (group() === null) return null;
      try {
        return wasm.composeCallPeerAnnouncement(identity, callId, selfPeerId);
      } catch {
        return null;
      }
    },
    verify: (from, announcement) => {
      const g = group();
      if (g === null) return null;
      try {
        const verified = wasm.verifyCallPeerAnnouncement(g, callId, from, announcement);
        try {
          const account = verified.accountKey;
          if (account.length !== 32) return null;
          return {
            personKeyB64: base64Encode(account),
            memberKeyHex: hexOf(verified.signatureKey),
          };
        } finally {
          verified.free();
        }
      } catch {
        return null;
      }
    },
    ownSignerKey: () => identity.signaturePublicKey,
    composeScreenShare: (selfPeerId, seq, sharing, withSound) => {
      if (group() === null) return null;
      try {
        return wasm.composeCallScreenShare(identity, callId, selfPeerId, seq, sharing, withSound);
      } catch {
        return null;
      }
    },
    verifyScreenShare: (from, signerKey, state) => {
      const g = group();
      if (g === null) return null;
      try {
        return wasm.verifyCallScreenShare(g, callId, from, signerKey, state);
      } catch {
        return null;
      }
    },
    composeCallName: (selfPeerId, seq, name) => {
      if (group() === null) return null;
      try {
        return wasm.composeCallName(identity, callId, selfPeerId, seq, name);
      } catch {
        return null;
      }
    },
    verifyCallName: (from, signerKey, bytes) => {
      const g = group();
      if (g === null) return null;
      try {
        const verified = wasm.verifyCallName(g, callId, from, signerKey, bytes);
        try {
          return { seq: verified.seq, name: verified.name ?? null };
        } finally {
          verified.free();
        }
      } catch {
        return null;
      }
    },
    composeCallChat: (selfPeerId, seq, text) => {
      if (group() === null) return null;
      try {
        return wasm.composeCallChat(identity, callId, selfPeerId, seq, text);
      } catch {
        return null;
      }
    },
    composeCallChatPicture: (selfPeerId, seq, p) => {
      if (group() === null) return null;
      try {
        return wasm.composeCallChatPicture(
          identity,
          callId,
          selfPeerId,
          seq,
          p.blobId,
          p.key,
          p.nonce,
          p.size,
          p.width,
          p.height,
        );
      } catch {
        return null;
      }
    },
    verifyCallChat: (from, signerKey, bytes) => {
      const g = group();
      if (g === null) return null;
      try {
        const verified = wasm.verifyCallChat(g, callId, from, signerKey, bytes);
        try {
          // The relay `from` is what the message was verified against; a
          // message naming another peer id is not this sender's.
          if (verified.peerId !== from) return null;
          const picture = verified.isPicture
            ? {
                blobId: verified.pictureBlobId,
                key: verified.pictureKey,
                nonce: verified.pictureNonce,
                size: verified.pictureSize,
                width: verified.pictureWidth,
                height: verified.pictureHeight,
              }
            : null;
          return { seq: verified.seq, text: verified.text, picture };
        } finally {
          verified.free();
        }
      } catch {
        return null;
      }
    },
  };
}

/** The bytes of a lower-case hex string, or `null` when it is not one. */
export function bytesOfHex(hex: string): Uint8Array | null {
  if (hex.length % 2 !== 0 || !/^[0-9a-f]*$/.test(hex)) return null;
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i += 1) out[i] = parseInt(hex.slice(2 * i, 2 * i + 2), 16);
  return out;
}

function hexOf(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

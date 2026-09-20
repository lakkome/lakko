// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0032 multi-joiner call-link orchestrator (web reference).
 *
 * Drives the "owner-founds + deterministic committer election" protocol on
 * top of the EXISTING SFU signal-relay — no server change. This is the web
 * reference the iOS + Android clients mirror.
 *
 * ## Roles
 *
 *   * **Owner** ({@link MultiJoinerCall.host}) — the sole founder. Calls
 *     `new Group(identity)`, derives the SFrame seed, joins the SFU room,
 *     and admits joiners (it is the acting committer while present).
 *   * **Joiner** ({@link MultiJoinerCall.join}) — never founds a group.
 *     Mints a fresh KeyPackage, connects to the SFU room signaling-only,
 *     relays `wrapCallKeyPackage(kp)` to the room, waits for the committer's
 *     `wrapCallWelcome(welcome)`, then `Group.joinFromWelcome` + derives the
 *     same seed. Media publishes ONLY after the group is joined (invariant
 *     #4 — no plaintext / half-open media).
 *
 * ## Committer election (survives the owner leaving)
 *
 * On every SFU roster change the acting committer is recomputed as a pure
 * function of public state (`electCommitter`, via {@link electActingCommitterLeaf}):
 * the owner if present, else the lowest-leaf present member. Only the member
 * that IS the acting committer processes an inbound KeyPackage — everyone
 * else drops it. Failover + reclaim fall out with no election protocol.
 *
 * ## Transport (reuse, no new SFU frame)
 *
 * The KeyPackage / Welcome / commit ride the existing opaque SFU
 * `signal-relay` (`sendSignalRelayToCall` / `...ToPeer`). The committer
 * classifies each inbound payload with `dispatchIncomingEnvelope`
 * (CallKeyPackage / CallWelcome / IntroMessage). An MLS `Commit` that must
 * reach existing members is broadcast raw; a receiver's dispatch classifies
 * it as `IntroMessage` (fallback = verbatim bytes) and feeds it to
 * `Group.processIncoming` inside a try/catch — a real intro / junk simply
 * fails to apply and is ignored.
 *
 * ## Per-admission epoch advance + rekey
 *
 * Adding a member advances the MLS epoch, so the per-call SFrame seed
 * changes. The committer (after `addMember`) and every existing member
 * (after applying the broadcast commit) re-derive the seed and `rekey`
 * their SFrame (G-18 / ADR-0029 overlap keyer); the new joiner starts at
 * the post-add epoch. All participants converge on one seed per epoch.
 *
 * ## Deferred / stubbed (honest)
 *
 *   * **Admission** is AUTO for the reference — the acting committer adds
 *     any valid KeyPackage. The ADR-0027 approve-toggle UI is a follow-up.
 *   * **Non-owner presence** binding is best-effort (see `CallPresence`) —
 *     the successor election among stranger joiners is the ADR-0032
 *     low-severity residual.
 *   * **Cold start** (owner not in a live room) via the stranger-inbox
 *     welcome-ferry is not wired here — the reference covers the live path.
 *   * A **live deployed-SFU** N-joiner + failover media round-trip is the
 *     deferred runtime gate (like G-26).
 */

import { createSignal, type Accessor } from "solid-js";

import { callActions, callState, type Participant } from "../state/call-state";
import { loadWasm } from "../wasm";
import { groupIdHex } from "../devices/sync/state";
import { CallJwtClient } from "./call-jwt-client";
import {
  SfuCallSession,
  type SfuCallState,
  type SfuRemoteTrack,
  type VideoPublishState,
} from "./sfu-call-session";
import type { SfuPeerSnapshot } from "./sfu-signaling-client";
import {
  CallPresence,
  electActingCommitterLeaf,
  hexKey,
  type ElectionMember,
} from "./multi-joiner-election";
import type {
  Group as WasmGroup,
  Identity as WasmIdentity,
} from "../../wasm/lakko_ffi_wasm";

/** MLS exporter label for the per-call SFrame seed (audit-pack C-1). Must
 *  match every other client. */
const CALL_SEED_LABEL = "lakko/call/v1";
const CALL_SEED_LEN = 32;
const PEER_PREFIX = "peer-";
/**
 * How long a joiner keeps relaying its KeyPackage before giving up. In the
 * owner-founds model a call with no live committer is simply dead — the
 * owner shared the link then left before anyone joined, or the call ended.
 * After this window the joiner stops rebroadcasting and surfaces "this call
 * has ended or isn't active" rather than waiting forever.
 */
const JOINER_ADMISSION_TIMEOUT_MS = 18_000;

/** Runtime view of the loaded wasm namespace. */
type WasmModule = Awaited<ReturnType<typeof loadWasm>>;
/** Runtime shape of a `Group.members()` element. */
interface WasmGroupMember {
  readonly leafIndex: number;
  readonly signaturePublicKey: Uint8Array;
}

/** Inputs shared by both roles. */
export interface MultiJoinerCallOptions {
  /** The canonical device `Identity` (`chats.identity()`). */
  readonly identity: WasmIdentity;
  /** 16-byte `call_id` from the capability. */
  readonly callId: Uint8Array;
  /** Owner's 16-byte `account_id` from the capability (for the owner's
   *  per-call peer_tag → presence match). */
  readonly ownerAccountId: Uint8Array;
  /** Owner's 32-byte Ed25519 signature key from the capability (identifies
   *  the owner's MLS leaf + is the election's `is_owner` anchor). */
  readonly ownerSignatureKey: Uint8Array;
  /** SFU WebSocket origin (`VITE_LAKKO_SFU_URL`). */
  readonly sfuBaseUrl: string;
  /** Live session token — mints the call-admission JWT. */
  readonly sessionToken: string;
  /** Mic / camera request. */
  readonly media: { readonly audio: boolean; readonly video: boolean };
  /** The name on the local tile (§4.4: the user's own profile name). Defaults
   *  to "You". */
  readonly localDisplayName?: string;
  /** `services/auth` base URL for the JWT mint (default `/api/auth`). */
  readonly authBaseUrl?: string;
  /** ICE servers (default public STUN; TURN is a follow-up). */
  readonly iceServers?: readonly RTCIceServer[];
  /** Resolve a peer to a contact name (ADR-0021 peer_tag lookup). */
  readonly resolveDisplayName?: (
    peerId: string,
    accountPseudonym: string,
    peerTag: string | undefined,
  ) => string;
  /** Fired once this client has an MLS group + derived seed (owner: at
   *  founding; joiner: on the Welcome). The app-level call host switches from
   *  "waiting for owner" to the in-call view. */
  readonly onJoined?: () => void;
  /** Fired on the first `connected` SFU transition (media flowing). */
  readonly onLive?: () => void;
  /** Fired on a terminal (`closed` / `failed`) transition. */
  readonly onEnded?: () => void;
  /** Joiner only: no committer admitted this client within
   *  {@link JOINER_ADMISSION_TIMEOUT_MS} — the call has no live committer
   *  (owner left before anyone joined, or the call ended). The screen
   *  surfaces "this call has ended or isn't active" instead of waiting. */
  readonly onNoCommitter?: () => void;
  /** Fired on a fatal setup error. */
  readonly onError?: (err: unknown) => void;
}

/** The orchestrator instance IS the caller's handle. */
export class MultiJoinerCall {
  private readonly session: SfuCallSession;
  private readonly presence: CallPresence;

  /** Remote MSID stream id → live `MediaStream` (for the Call.tsx tiles). */
  private readonly setRemoteStreams: (
    fn: (prev: ReadonlyMap<string, MediaStream>) => ReadonlyMap<string, MediaStream>,
  ) => void;
  readonly remoteStreams: Accessor<ReadonlyMap<string, MediaStream>>;
  /** Local capture stream once media is published (the self-preview tile). */
  readonly localStream: Accessor<MediaStream | null>;
  private readonly setLocalStream: (stream: MediaStream | null) => void;
  /** Mid-call camera-on lifecycle, for the camera button. */
  readonly videoPublishState: Accessor<VideoPublishState>;
  private readonly setVideoPublishState: (state: VideoPublishState) => void;

  /** The call MLS group (owner: founded; joiner: after the Welcome). */
  private group: WasmGroup | null = null;
  /** This client's MLS leaf once it has a group. */
  private localLeaf: number | null = null;
  private joined = false;

  /** G-22 dedup (FINDING 4). Keyed on the relay `from` SFU peer id → wrapped
   *  Welcome, so ONE SFU-admitted peer is admitted at most once per call —
   *  even if it re-mints FRESH KeyPackages — and a retry re-serves the stored
   *  Welcome. Plus an exact-KP-bytes guard for an identical re-send.
   *  TODO(ADR-0032/G-22): the ideal is per-joiner-SIGNATURE-KEY dedup, but the
   *  wasm exposes no KeyPackage signature-key reader yet — switch to it once an
   *  FFI helper (e.g. `keyPackageSignatureKey`) lands. */
  private readonly admittedByPeer = new Map<string, Uint8Array>();
  private readonly admittedByKp = new Set<string>();
  /** G-22: the PRIMARY admission dedup — the joiner's stable MLS signature
   *  (leaf) key, so a peer re-minting FRESH KeyPackages under one identity is
   *  admitted at most once. Maps signature-key hex → the stored wrapped Welcome. */
  private readonly admittedBySigKey = new Map<string, Uint8Array>();
  /** The joiner's wrapped KeyPackage, (re)broadcast until it has joined. */
  private pendingKeyPackage: Uint8Array | null = null;
  /** Joiner admission-timeout handle; `null` once cleared (joined / dead). */
  private admissionTimer: ReturnType<typeof setTimeout> | null = null;
  /** `true` once the admission timeout fired — stops any further rebroadcast. */
  private timedOut = false;
  /** Canonical safety number of the CURRENT epoch (owner: from founding;
   *  joiner: on join; both: refreshed on rekey). */
  private safetyNumber: string | undefined;
  private firedLive = false;
  /** Sticky last-known camera state per remote roster tile (keyed on
   *  `peer-<peerId>`). Closes the join-time reorder race: a newcomer
   *  subscribing to an already camera-muted peer still receives that peer's
   *  present-but-disabled video track, which fires `onRemoteTrack` and would
   *  flip `camOn` back on. `onRemoteMediaState` is authoritative and records the
   *  true state here; the track-arrival path defers to it so a known-muted peer
   *  is never wrongly shown as camera-on. Adapter-local (one adapter per call);
   *  both callbacks are delivered on the same session scope, so no extra
   *  synchronisation is needed. Cleared on `hangUp`. */
  private readonly lastKnownCameraOn = new Map<string, boolean>();

  private constructor(
    private readonly wasm: WasmModule,
    private readonly opts: MultiJoinerCallOptions,
    private readonly role: "owner" | "joiner",
    jwt: string,
    private readonly ownerKeyHex: string,
    ownerPeerTag: string | null,
    initialSeed: Uint8Array | undefined,
    initialLeaf: number | undefined,
  ) {
    const [remoteStreams, setRemoteStreams] = createSignal<
      ReadonlyMap<string, MediaStream>
    >(new Map());
    this.remoteStreams = remoteStreams;
    this.setRemoteStreams = setRemoteStreams;
    const [localStream, setLocalStream] = createSignal<MediaStream | null>(null);
    this.localStream = localStream;
    this.setLocalStream = (stream): void => {
      setLocalStream(stream);
    };
    const [videoPublishState, setVideoPublishState] = createSignal<VideoPublishState>("off");
    this.videoPublishState = videoPublishState;
    this.setVideoPublishState = (state): void => {
      setVideoPublishState(state);
    };
    this.localLeaf = initialLeaf ?? null;

    this.presence = new CallPresence(
      hexKey(opts.identity.signaturePublicKey),
      this.ownerKeyHex,
      ownerPeerTag,
    );

    this.session = new SfuCallSession(
      {
        sfuBaseUrl: opts.sfuBaseUrl,
        jwt,
        media: opts.media,
        // The joiner defers media until it holds a Welcome + seed; the owner
        // has its seed at founding and publishes immediately.
        deferPublish: role === "joiner",
        // Omit undefined optionals (exactOptionalPropertyTypes).
        ...(initialSeed !== undefined ? { seed: initialSeed } : {}),
        ...(initialLeaf !== undefined ? { localLeaf: initialLeaf } : {}),
        ...(opts.iceServers !== undefined
          ? { iceServers: opts.iceServers }
          : {}),
      },
      {
        onStateChange: (s): void => this.onSessionState(s),
        onWelcome: (self, roster): void => this.onSfuWelcome(self, roster),
        onPeerJoined: (peerId, pseudo, tag): void =>
          this.onSfuPeerJoined(peerId, pseudo, tag),
        onPeerLeft: (peerId): void => this.onSfuPeerLeft(peerId),
        onRemoteTrack: (rt): void => this.onRemoteTrack(rt),
        onRemoteMediaState: (peerId, cameraOn): void => {
          // Media-state is authoritative — record it so a later (racy) track
          // arrival for a muted peer defers to it (the join-race guard in
          // `onRemoteTrack`).
          const id = `${PEER_PREFIX}${peerId}`;
          this.lastKnownCameraOn.set(id, cameraOn);
          callActions.setParticipantCamera(id, cameraOn);
        },
        onSignalRelay: (from, payload): void => this.onRelay(from, payload),
        onLocalStream: (stream): void => this.setLocalStream(stream),
        onVideoPublishState: (state): void => this.setVideoPublishState(state),
      },
    );
  }

  // -----------------------------------------------------------------
  // Factories
  // -----------------------------------------------------------------

  /** Owner: found the call MLS group, derive the seed, connect + publish,
   *  and start admitting joiners. */
  static async host(opts: MultiJoinerCallOptions): Promise<MultiJoinerCall> {
    const wasm = await loadWasm();
    const callIdHex = groupIdHex(opts.callId);
    const jwt = await mintJwt(wasm, opts, callIdHex);
    const ownerKeyHex = hexKey(opts.ownerSignatureKey);

    // Found the group (sole-founder — no race).
    const group = new wasm.Group(opts.identity);
    const seed = group.exportSecret(
      opts.identity,
      CALL_SEED_LABEL,
      opts.callId,
      CALL_SEED_LEN,
    );
    // Owner-presence rides `self` (ownerPeerTag = null on the owner's client).
    const call = new MultiJoinerCall(
      wasm,
      opts,
      "owner",
      jwt,
      ownerKeyHex,
      null,
      seed,
      group.ownLeafIndex,
    );
    call.group = group;
    call.safetyNumber = safeSafetyNumber(wasm, seed);
    call.joined = true;
    opts.onJoined?.();
    await call.start();
    return call;
  }

  /** Joiner: mint a fresh KeyPackage, connect signaling-only, relay the KP,
   *  and wait for the committer's Welcome. */
  static async join(opts: MultiJoinerCallOptions): Promise<MultiJoinerCall> {
    const wasm = await loadWasm();
    const callIdHex = groupIdHex(opts.callId);
    const jwt = await mintJwt(wasm, opts, callIdHex);
    const ownerKeyHex = hexKey(opts.ownerSignatureKey);
    // The owner's per-call peer_tag lets a joiner detect owner presence on
    // the SFU roster (ADR-0021 + ADR-0032 election).
    const ownerPeerTag = wasm.sfuPeerTag(callIdHex, groupIdHex(opts.ownerAccountId));

    const call = new MultiJoinerCall(
      wasm,
      opts,
      "joiner",
      jwt,
      ownerKeyHex,
      ownerPeerTag,
      undefined,
      undefined,
    );
    // Mint a fresh KeyPackage whose private keys persist in this identity's
    // provider (so the SAME identity opens the Welcome later).
    const kp = opts.identity.createKeyPackage();
    call.pendingKeyPackage = wasm.wrapCallKeyPackage(kp);
    await call.start();
    // Start the admission clock only once signaling is open + the first
    // broadcast is on its way (a call with no live committer is dead).
    call.admissionTimer = setTimeout(
      () => call.onAdmissionTimeout(),
      JOINER_ADMISSION_TIMEOUT_MS,
    );
    return call;
  }

  private async start(): Promise<void> {
    try {
      await this.session.connect();
    } catch (err) {
      this.opts.onError?.(err);
      throw err;
    }
  }

  // -----------------------------------------------------------------
  // Public handle
  // -----------------------------------------------------------------

  /** Look up a roster participant id (`peer-<id>`) to its live stream. */
  remoteStreamFor(participantId: string): MediaStream | undefined {
    const peerId = participantId.startsWith(PEER_PREFIX)
      ? participantId.slice(PEER_PREFIX.length)
      : participantId;
    return this.remoteStreams().get(peerId);
  }

  /** Tear down the call. Idempotent. */
  hangUp(): void {
    this.clearAdmissionTimer();
    this.lastKnownCameraOn.clear();
    this.session.hangUp();
    this.setRemoteStreams(() => new Map());
    this.setLocalStream(null);
    this.setVideoPublishState("off");
    callActions.reset();
  }

  /** The SFU session's lifecycle state. `closed` / `failed` means the call
   *  ended on its own (or was torn down), not that the user is ending it. */
  sessionState(): SfuCallState {
    return this.session.getState();
  }

  private clearAdmissionTimer(): void {
    if (this.admissionTimer !== null) {
      clearTimeout(this.admissionTimer);
      this.admissionTimer = null;
    }
  }

  /** Joiner admission window elapsed with no Welcome — the call has no live
   *  committer. Stop rebroadcasting, tear down signaling, surface the state. */
  private onAdmissionTimeout(): void {
    this.admissionTimer = null;
    if (this.joined) return;
    this.timedOut = true;
    this.pendingKeyPackage = null;
    this.session.hangUp();
    this.opts.onNoCommitter?.();
  }

  /** Mute / unmute the local mic. */
  setMicrophoneEnabled(on: boolean): void {
    this.session.setMicrophoneEnabled(on);
  }

  /** Enable / disable the local camera. */
  setCameraEnabled(on: boolean): void {
    this.session.setCameraEnabled(on);
  }

  /** Turn the camera on mid-call (acquire + SFrame-attach + renegotiate when
   *  the call started audio-only). A no-op before media is published — a
   *  joiner publishes only once admitted (invariant #4). Throws on a capture
   *  failure so the caller can revert the toggle. */
  publishVideo(): Promise<void> {
    return this.session.publishVideo();
  }

  /** In-call device picker: `replaceTrack` on the live sender (SFrame stays
   *  armed). */
  switchAudioInput(deviceId: string): Promise<void> {
    return this.session.switchAudioInput(deviceId);
  }

  /** In-call device picker for the camera; a no-op while audio-only. */
  switchVideoInput(deviceId: string): Promise<void> {
    return this.session.switchVideoInput(deviceId);
  }

  // -----------------------------------------------------------------
  // SFU session callbacks → store + presence
  // -----------------------------------------------------------------

  private onSessionState(state: string): void {
    switch (state) {
      case "connecting":
        callActions.setPhase({ kind: "connecting" });
        break;
      case "connected":
        callActions.setPhase({ kind: "connected" });
        callActions.setE2ee({
          active: true,
          shortFingerprint: this.safetyNumber ?? null,
        });
        if (!this.firedLive) {
          this.firedLive = true;
          this.opts.onLive?.();
        }
        break;
      case "failed":
      case "closed":
        callActions.setPhase({ kind: "ended", endedAt: Date.now() });
        this.opts.onEnded?.();
        break;
      default:
        break;
    }
  }

  private onSfuWelcome(
    selfPeerId: string,
    roster: readonly SfuPeerSnapshot[],
  ): void {
    this.presence.setRoster(
      roster.map((p) => ({ peerId: p.peer_id, peerTag: p.peer_tag })),
    );
    const remotes = roster
      .filter((p) => p.peer_id !== selfPeerId)
      .map((p) =>
        this.remoteParticipant(
          this.nameOf(p.peer_id, p.account_pseudonym, p.peer_tag),
          p.peer_id,
        ),
      );
    callActions.setParticipants([this.localParticipant(), ...remotes]);
    this.broadcastKeyPackage();
  }

  private onSfuPeerJoined(
    peerId: string,
    accountPseudonym: string,
    peerTag: string | undefined,
  ): void {
    this.presence.addPeer(peerId, peerTag);
    callActions.participantJoined(
      this.remoteParticipant(
        this.nameOf(peerId, accountPseudonym, peerTag),
        peerId,
      ),
    );
    // A newly-present peer may be a freshly-elected committer (owner
    // reclaim / a successor); re-broadcast so it sees a still-pending join.
    this.broadcastKeyPackage();
  }

  private onSfuPeerLeft(peerId: string): void {
    this.presence.removePeer(peerId);
    const id = `${PEER_PREFIX}${peerId}`;
    // Forget the departed peer's sticky camera state so a later rejoin under
    // the same id starts from a clean slate (join-race guard).
    this.lastKnownCameraOn.delete(id);
    callActions.participantLeft(id);
    // The committer may have left → recompute (this client might now be it).
    // No action needed beyond keeping presence current; the next inbound KP
    // is evaluated against the fresh election.
  }

  private onRemoteTrack(rt: SfuRemoteTrack): void {
    this.setRemoteStreams((prev) => {
      const next = new Map(prev);
      next.set(rt.streamId, rt.stream);
      return next;
    });
    const participantId = `${PEER_PREFIX}${rt.streamId}`;
    if (rt.track.kind === "video") {
      // Join-race guard: defer to the sticky last-known state so a track
      // arrival for a known-muted peer is not flipped back on; an unknown peer
      // defaults on (they published video). Native pure decision
      // `trackPresent ? (lastKnown ?? true) : false` — the track is present
      // here, and the removal case is the `ended` handler below. Mirrors
      // `sfu-call-ui.ts` `addStream`.
      callActions.setParticipantCamera(
        participantId,
        this.lastKnownCameraOn.get(participantId) ?? true,
      );
    }
    // Prune when the SFU stops forwarding this track (peer left / unpublished).
    // The `ended` event is the reliable track-gone signal — complementary to
    // the `onRemoteMediaState` camera-toggle (a mute fires no `ended`). Mirrors
    // `sfu-call-ui.ts` `addStream` exactly.
    rt.track.addEventListener("ended", () => {
      if (rt.track.kind === "video") {
        callActions.setParticipantCamera(participantId, false);
      }
      this.setRemoteStreams((prev) => {
        // Only drop the stream entry when no live tracks remain on it (a peer
        // that toggles camera off keeps its audio track + entry).
        if (rt.stream.getTracks().some((t) => t.readyState === "live")) {
          return prev;
        }
        if (!prev.has(rt.streamId)) return prev;
        const next = new Map(prev);
        next.delete(rt.streamId);
        return next;
      });
    });
  }

  // -----------------------------------------------------------------
  // ADR-0032 handshake dispatch
  // -----------------------------------------------------------------

  private onRelay(from: string, payload: Uint8Array): void {
    let env: { kind: number; payload: Uint8Array };
    try {
      env = this.wasm.dispatchIncomingEnvelope(payload);
    } catch {
      return; // malformed relay payload — drop
    }
    const K = this.wasm.IncomingEnvelopeKind;
    switch (env.kind) {
      case K.CallKeyPackage:
        this.onIncomingKeyPackage(from, env.payload);
        break;
      case K.CallWelcome:
        this.onIncomingWelcome(from, env.payload);
        break;
      case K.IntroMessage:
        // Not a call envelope — the fallback carries verbatim bytes, which
        // for us is a broadcast MLS commit an existing member must apply.
        this.onMaybeCommit(env.payload);
        break;
      case K.Profile:
        // ADR-0048 — a profile belongs to a chat group's MLS application
        // stream, never to the SFU relay. One arriving here is dropped: it is
        // not a commit, and the call roster names people through the ADR-0021
        // peer tag, not through relayed profiles.
        break;
      default:
        break;
    }
  }

  /** Committer side: admit a joiner's KeyPackage (auto-admit for the
   *  reference). Only the ACTING committer acts; everyone else drops it. */
  private onIncomingKeyPackage(from: string, kpEnvelope: Uint8Array): void {
    if (this.group === null || this.localLeaf === null) return;
    if (!this.isActingCommitter()) return;

    // G-22: dedup PRIMARILY on the joiner's stable MLS signature (leaf) key so a
    // peer re-minting FRESH KeyPackages under one identity is added at most once;
    // the per-`from`-peer + exact-KP-bytes guards stay as fast-path fallbacks.
    let sigKeyHex: string | null = null;
    try {
      sigKeyHex = hexKey(this.wasm.keyPackageSignatureKey(kpEnvelope));
    } catch {
      // Malformed KeyPackage — addMember below refuses too; leave sigKeyHex null.
    }
    const kpKey = hexKey(kpEnvelope);
    const priorWelcome =
      (sigKeyHex !== null ? this.admittedBySigKey.get(sigKeyHex) : undefined) ??
      this.admittedByPeer.get(from);
    if (priorWelcome !== undefined || this.admittedByKp.has(kpKey)) {
      // Idempotent: re-serve the stored Welcome (a peer that lost it).
      if (priorWelcome !== undefined) {
        this.session.sendSignalRelayToPeer(from, priorWelcome);
      }
      return;
    }

    // The joiner sends `wrapCallKeyPackage(kp)`; dispatch already stripped
    // the domain separator, so `kpEnvelope` is the raw TLS KeyPackage.
    const beforeKeys = new Set(
      this.groupMembers().map((m) => m.signatureKeyHex),
    );
    let welcomeWrapped: Uint8Array;
    let commit: Uint8Array;
    try {
      const added = this.group.addMember(this.opts.identity, kpEnvelope);
      commit = added.commit;
      welcomeWrapped = this.wasm.wrapCallWelcome(added.welcome);
    } catch (err) {
      // A malformed / already-used KeyPackage — refuse, do not advance.
      // eslint-disable-next-line no-console
      console.warn("multi-joiner: addMember failed", err);
      return;
    }
    if (sigKeyHex !== null) this.admittedBySigKey.set(sigKeyHex, welcomeWrapped);
    this.admittedByPeer.set(from, welcomeWrapped);
    this.admittedByKp.add(kpKey);
    // Bind the new joiner's peer → its member key for presence.
    for (const m of this.groupMembers()) {
      if (!beforeKeys.has(m.signatureKeyHex)) {
        this.presence.bindPeerToMember(from, m.signatureKeyHex);
      }
    }
    // Deliver the Welcome to the joiner + broadcast the commit to the rest.
    this.session.sendSignalRelayToPeer(from, welcomeWrapped);
    this.session.sendSignalRelayToCall(commit);
    // The add advanced our epoch → rekey to the new seed.
    this.rekeyToCurrentEpoch();
    this.refreshRoster();
  }

  /** Joiner side: open the committer's Welcome and go live. */
  private onIncomingWelcome(from: string, welcomeEnvelope: Uint8Array): void {
    if (this.role !== "joiner" || this.joined) return;
    let group: WasmGroup;
    try {
      group = this.wasm.Group.joinFromWelcome(welcomeEnvelope, this.opts.identity);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("multi-joiner: joinFromWelcome failed", err);
      return;
    }

    // FINDING 2 (defence-in-depth vs the successor-redirect residual): confirm
    // the JOINED group actually contains the OWNER whose signature the SIGNED
    // capability carried. The owner key comes from the capability (not from any
    // peer), so a Welcome that lands us in an impostor's isolated group — no
    // owner leaf — is a redirect. Refuse it fail-closed (invariant #4): never
    // publish media, tear down, surface a clear error. A guest gets this check
    // too (the owner key is in the signed capability, not peer-asserted).
    const joinedKeys = (group.members() as WasmGroupMember[]).map((m) =>
      hexKey(m.signaturePublicKey),
    );
    if (!joinedKeys.includes(this.ownerKeyHex)) {
      // eslint-disable-next-line no-console
      console.warn(
        "multi-joiner: joined a group without the capability owner — redirect refused",
      );
      this.timedOut = true;
      this.pendingKeyPackage = null;
      this.clearAdmissionTimer();
      this.session.hangUp();
      this.opts.onError?.(
        new Error("Joined the wrong call — the owner is not in this group."),
      );
      return;
    }

    this.group = group;
    this.localLeaf = group.ownLeafIndex;
    this.joined = true;
    this.pendingKeyPackage = null;
    this.clearAdmissionTimer();

    const seed = this.deriveSeed(group);
    this.safetyNumber = safeSafetyNumber(this.wasm, seed);
    // The committer (during the happy path, the owner) admitted us; bind its
    // peer for presence when we can identify it as the owner.
    if (this.presence.ownerPresent()) {
      this.presence.bindPeerToMember(from, this.ownerKeyHex);
    }
    this.opts.onJoined?.();
    // Publish media ONLY now — after joining + deriving the seed (invariant #4).
    void this.session.publish(seed, this.localLeaf).catch((err) => {
      this.opts.onError?.(err);
    });
    this.refreshRoster();
  }

  /** Existing member: apply a broadcast commit (epoch advance) + rekey. */
  private onMaybeCommit(bytes: Uint8Array): void {
    if (this.group === null) return;
    try {
      // The call group has no ADR-0022 role table — plain apply (the call
      // path caveat). A non-commit / wrong-epoch / junk payload throws and
      // is ignored.
      this.group.processIncoming(this.opts.identity, bytes);
    } catch {
      return;
    }
    this.rekeyToCurrentEpoch();
    this.refreshRoster();
  }

  // -----------------------------------------------------------------
  // Election + epoch helpers
  // -----------------------------------------------------------------

  private isActingCommitter(): boolean {
    if (this.group === null || this.localLeaf === null) return false;
    const leaf = electActingCommitterLeaf(
      this.wasm,
      this.groupMembers(),
      this.ownerKeyHex,
      this.presence.presentKeySet(),
    );
    return leaf === this.localLeaf;
  }

  private groupMembers(): ElectionMember[] {
    if (this.group === null) return [];
    return (this.group.members() as WasmGroupMember[]).map((m) => ({
      leafIndex: m.leafIndex,
      signatureKeyHex: hexKey(m.signaturePublicKey),
    }));
  }

  private deriveSeed(group: WasmGroup): Uint8Array {
    return group.exportSecret(
      this.opts.identity,
      CALL_SEED_LABEL,
      this.opts.callId,
      CALL_SEED_LEN,
    );
  }

  /** Re-derive the seed at the current epoch and rekey SFrame + the pill. */
  private rekeyToCurrentEpoch(): void {
    if (this.group === null) return;
    const seed = this.deriveSeed(this.group);
    this.session.rekey(seed);
    this.safetyNumber = safeSafetyNumber(this.wasm, seed);
    callActions.setE2ee({ active: true, shortFingerprint: this.safetyNumber ?? null });
  }

  /** Re-derive the store roster after a membership change (mostly a no-op —
   *  the SFU roster drives the visible tiles; this keeps E2EE fresh). */
  private refreshRoster(): void {
    // Presence set drives the election on the next inbound KP; nothing to
    // push to the store here (the SFU peer-joined/left events own the tiles).
  }

  private broadcastKeyPackage(): void {
    if (this.joined || this.timedOut || this.pendingKeyPackage === null) return;
    this.session.sendSignalRelayToCall(this.pendingKeyPackage);
  }

  // -----------------------------------------------------------------
  // Store-participant helpers (ported from sfu-call-ui.ts)
  // -----------------------------------------------------------------

  private nameOf(
    peerId: string,
    pseudonym: string,
    peerTag: string | undefined,
  ): string {
    return (
      this.opts.resolveDisplayName?.(peerId, pseudonym, peerTag) ??
      (pseudonym.length > 0 ? pseudonym.slice(0, 8) : "Peer")
    );
  }

  private localParticipant(): Participant {
    return {
      id: "local",
      displayName: this.opts.localDisplayName ?? "You",
      isLocal: true,
      // See `sfu-call-ui.ts`: `media.audio` is always true on web, so a muted
      // start is carried in the store instead (docs/ux/calls.md section 3.2).
      micOn: callState.pendingMicOn(),
      camOn: this.opts.media.video,
      quality: "good",
      speaking: false,
    };
  }

  private remoteParticipant(displayName: string, peerId: string): Participant {
    return {
      id: `${PEER_PREFIX}${peerId}`,
      displayName,
      isLocal: false,
      micOn: true,
      camOn: false,
      quality: "good",
      speaking: false,
    };
  }
}

// ---- module helpers ------------------------------------------------

async function mintJwt(
  wasm: WasmModule,
  opts: MultiJoinerCallOptions,
  callIdHex: string,
): Promise<string> {
  const client = new CallJwtClient(wasm, opts.authBaseUrl);
  client.setSessionToken(opts.sessionToken);
  const minted = await client.mint(callIdHex, "participant");
  return minted.jwt;
}

/** Safety number is display-only — a derivation failure must never abort. */
function safeSafetyNumber(wasm: WasmModule, seed: Uint8Array): string | undefined {
  try {
    return wasm.callSafetyNumber(seed);
  } catch {
    return undefined;
  }
}

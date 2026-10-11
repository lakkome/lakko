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
 *     `new Group(identity, GroupKind.Group)`, derives the SFrame seed, joins the SFU room,
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
import { captureFallbackNotice, type CaptureFallback } from "./capture-fallback";
import {
  SfuCallSession,
  type SfuCallState,
  type SfuRemoteTrack,
  type VideoPublishState,
} from "./sfu-call-session";
import type { SfuPeerSnapshot } from "./sfu-signaling-client";
import { TurnCredentialClient } from "./turn-credential-client";
import {
  CallPresence,
  electActingCommitterLeaf,
  hexKey,
  type ElectionMember,
} from "./multi-joiner-election";
import type {
  Group as WasmGroup,
  Identity as WasmIdentity,
  ProcessedMessage as WasmProcessedMessage,
} from "../../wasm/lakko_ffi_wasm";
import { t } from "../l10n";
import { base64Encode } from "../auth/account-id";
import type { IncomingProfile } from "../profile/profile-store";
import { accountIdHexOfBytes } from "../profile/profile-store";
import { callTileName, EarlyRelays, profileClaimMatchesPeer } from "./tile-name";
import { ENDED_FOR_ALL } from "./end-for-everyone";
import { splitStreamId, withLightStream, withoutLightStream } from "./light-stream";
import type { VideoQuality } from "./video-wants";

/** A profile a member sealed into the call, as the profile store takes it. */
export type AnnouncedProfile = IncomingProfile;

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
/** A pending joiner re-sends its KeyPackage this often until admitted, so an
 *  admission is never lost to ordering: a committer that founds the group
 *  after the joiner's first broadcast, or that arrives later, still gets it. */
export const KEY_PACKAGE_REBROADCAST_MS = 3_000;
/** The owner, finding other peers already in the room, asks to be admitted
 *  for this long before concluding nobody there holds a group and founding
 *  one (the peers are then joiners waiting for exactly that). */
export const OWNER_REJOIN_PROBE_MS = 5_000;

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
  /** The user's own ADR-0048 profile envelope, or `null` when none is set.
   *  Sealed into the call's MLS group and relayed, so every other member — a
   *  guest with no contact list included — names this tile (§4.4). The SFU
   *  relays ciphertext only. */
  readonly ownProfileEnvelope?: () => Promise<Uint8Array | null>;
  /** `services/auth` base URL for the JWT mint (default `/api/auth`). */
  readonly authBaseUrl?: string;
  /** Extra ICE servers. A TURN row is minted from `sessionToken` on top of
   *  whatever this carries; the default base is public STUN. */
  readonly iceServers?: readonly RTCIceServer[];
  /** Resolve a peer to a person this device knows (ADR-0021 peer_tag
   *  lookup), or `null`. What names a tile before its member has announced
   *  itself. */
  readonly resolveDisplayName?: (
    peerId: string,
    accountPseudonym: string,
    peerTag: string | undefined,
  ) => string | null;
  /** A member announced its profile inside the call. `personKeyB64` is the
   *  MLS-authenticated sender, resolved to the account key at receipt (C-68),
   *  and the account id the profile claims is the one the SFU admitted the
   *  relaying peer as (`profileClaimMatchesPeer`). The host files it in the
   *  profile store, which is what verifies a claimed username. */
  readonly onMemberProfile?: (personKeyB64: string, profile: AnnouncedProfile) => void;
  /** What the one resolver calls `personKeyB64` (rename, profile name,
   *  verified `@username`), or `null` when it has only a fingerprint. */
  readonly resolvePersonName?: (personKeyB64: string) => string | null;
  /** The link's owner ended the call for everyone
   *  (`docs/ux/calls.md` section 4.1.2). `peerId` is the SFU peer that
   *  relayed it, for the name on the notice. The host ends the call. */
  readonly onEndedForEveryone?: (peerId: string) => void;
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
  private readonly setRemoteLightStreams: (
    fn: (prev: ReadonlyMap<string, MediaStream>) => ReadonlyMap<string, MediaStream>,
  ) => void;
  /** Remote peer id -> that peer's light companion stream
   *  (`light-stream.ts`): a second picture of a peer already in
   *  `remoteStreams`, never a participant, a tile or an audio sink. */
  readonly remoteLightStreams: Accessor<ReadonlyMap<string, MediaStream>>;
  private readonly setRemoteMiddleStreams: (
    fn: (prev: ReadonlyMap<string, MediaStream>) => ReadonlyMap<string, MediaStream>,
  ) => void;
  /** Remote peer id -> that peer's middle stream (`light-stream.ts`,
   *  ADR-0070 step 5): the picture a grid tile watches in place of the main
   *  one. Never a participant, a tile or an audio sink. */
  readonly remoteMiddleStreams: Accessor<ReadonlyMap<string, MediaStream>>;
  /** Remote peer id -> the picture the SFU says it gives this device of them
   *  (`video-given.ts`, ADR-0070 step 6). A peer absent from it is given
   *  what is asked for; empty with downlink control off. */
  readonly remoteVideoGiven: Accessor<ReadonlyMap<string, VideoQuality>>;
  private readonly setRemoteVideoGiven: (given: ReadonlyMap<string, VideoQuality>) => void;
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
  /** Names members announced over the call group (SFU peer id -> name). A
   *  name learnt this way outranks the contact-list resolver: it is what the
   *  person calls themself, sealed under a key only members hold. */
  private readonly peerNames = new Map<string, string>();
  /** SFU peer id -> the person (base64 account key) whose sealed profile
   *  that peer relayed. */
  private readonly peerPersons = new Map<string, string>();
  /** What each remote tile was named from, so it can be named again. */
  private readonly namingInputs = new Map<
    string,
    { pseudonym: string; peerTag: string | undefined }
  >();
  /** SFU peer id -> the username that peer's sealed profile claims. */
  private readonly peerUsernames = new Map<string, string>();
  /** Broadcasts that arrived before this client had a group to read them. */
  private readonly earlyRelays = new EarlyRelays<{ from: string; bytes: Uint8Array }>();
  /** What this device joined without (section 4.4), once capture ran. */
  private captureFallback: CaptureFallback | null = null;

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
  /** Periodic KeyPackage re-send while `pendingKeyPackage` is set. */
  private rebroadcastTimer: ReturnType<typeof setInterval> | null = null;
  /** The owner's rejoin probe (see `OWNER_REJOIN_PROBE_MS`). */
  private ownerProbeTimer: ReturnType<typeof setTimeout> | null = null;
  /** The exact-KeyPackage guard entry each admitted peer holds, so a
   *  departure can forget it along with the rest of the admission. */
  private readonly kpKeyByPeer = new Map<string, string>();
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
    const [remoteLightStreams, setRemoteLightStreams] = createSignal<
      ReadonlyMap<string, MediaStream>
    >(new Map());
    this.remoteLightStreams = remoteLightStreams;
    this.setRemoteLightStreams = (fn): void => {
      setRemoteLightStreams(fn);
    };
    const [remoteMiddleStreams, setRemoteMiddleStreams] = createSignal<
      ReadonlyMap<string, MediaStream>
    >(new Map());
    this.remoteMiddleStreams = remoteMiddleStreams;
    this.setRemoteMiddleStreams = (fn): void => {
      setRemoteMiddleStreams(fn);
    };
    const [remoteVideoGiven, setRemoteVideoGiven] = createSignal<
      ReadonlyMap<string, VideoQuality>
    >(new Map());
    this.remoteVideoGiven = remoteVideoGiven;
    this.setRemoteVideoGiven = (given): void => {
      setRemoteVideoGiven(given);
    };
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
        // Media waits for a seed on both sides: the joiner for its Welcome,
        // the owner for the SFU roster, which decides whether it founds the
        // group or comes back into one a successor is holding.
        deferPublish: true,
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
        // The SFU's `video-given` frames are read by the session this call
        // wraps; here they only reach the tiles.
        onVideoGiven: (given): void => this.setRemoteVideoGiven(given),
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
        onCaptureFallback: (fallback): void => this.onCaptureFallback(fallback),
      },
    );
    // A joiner behind a symmetric NAT has no server-reflexive candidate and
    // reaches the room only through the relay. Fail-soft: no credential
    // leaves the call on STUN.
    const turn = new TurnCredentialClient(opts.authBaseUrl);
    turn.setSessionToken(opts.sessionToken);
    this.session.setTurnProvider(() => turn.fetchOrNull());
  }

  /** Section 4.4: the device lacked what the call asked for. Turn the missing
   *  controls off on the own tile and say so once. */
  private onCaptureFallback(fallback: CaptureFallback): void {
    this.captureFallback = fallback;
    callActions.setCamera(false);
    if (fallback.missing === "camera-and-microphone") callActions.setMic(false);
    callActions.showNotice(captureFallbackNotice(fallback));
  }

  // -----------------------------------------------------------------
  // Factories
  // -----------------------------------------------------------------

  /** Owner: connect, then let the SFU roster decide. An empty room means the
   *  owner founds the call group (sole founder, no race) and starts admitting
   *  joiners. A room with people in it means the owner is coming BACK to a
   *  call a successor kept alive (the maintainer's rule: a joiner who arrived
   *  in the meantime is admin until the starter returns), so the owner asks
   *  to be admitted like anyone else and the election hands it the committer
   *  duty once it is in; if nobody admits it within `OWNER_REJOIN_PROBE_MS`,
   *  the peers are joiners waiting for a group and the owner founds one. */
  static async host(opts: MultiJoinerCallOptions): Promise<MultiJoinerCall> {
    const wasm = await loadWasm();
    const callIdHex = groupIdHex(opts.callId);
    const jwt = await mintJwt(wasm, opts, callIdHex);
    const ownerKeyHex = hexKey(opts.ownerSignatureKey);
    // Owner-presence rides `self` (ownerPeerTag = null on the owner's client).
    const call = new MultiJoinerCall(
      wasm,
      opts,
      "owner",
      jwt,
      ownerKeyHex,
      null,
      undefined,
      undefined,
    );
    await call.start();
    return call;
  }

  /** Owner: found the call group and go live (an empty room, or a probe
   *  that nobody answered). */
  private foundGroup(): void {
    if (this.group !== null || this.joined) return;
    let group: WasmGroup;
    let seed: Uint8Array;
    try {
      // ADR-0062 — a call-link group admits joiners, so it follows the
      // group rule; it stays out of the lists as before.
      group = new this.wasm.Group(this.opts.identity, this.wasm.GroupKind.Group);
      seed = this.deriveSeed(group);
    } catch (err) {
      this.opts.onError?.(err);
      return;
    }
    this.group = group;
    this.localLeaf = group.ownLeafIndex;
    this.joined = true;
    this.pendingKeyPackage = null;
    this.clearAdmissionTimer();
    this.clearRebroadcast();
    this.clearOwnerProbe();
    this.clearRebroadcast();
    this.clearOwnerProbe();
    this.safetyNumber = safeSafetyNumber(this.wasm, seed);
    this.opts.onJoined?.();
    void this.session.publish(seed, this.localLeaf).catch((err) => {
      this.opts.onError?.(err);
    });
    void this.broadcastOwnProfile();
  }

  /** Owner, in a room with people: ask to be admitted (the successor holds
   *  the group) and fall back to founding when nobody answers. */
  private beginOwnerRejoinProbe(): void {
    if (this.group !== null || this.joined || this.pendingKeyPackage !== null) return;
    try {
      const kp = this.opts.identity.createKeyPackage();
      this.pendingKeyPackage = this.wasm.wrapCallKeyPackage(kp);
    } catch (err) {
      this.opts.onError?.(err);
      return;
    }
    this.broadcastKeyPackage();
    this.startRebroadcast();
    this.ownerProbeTimer = setTimeout(() => {
      this.ownerProbeTimer = null;
      if (this.joined) return;
      this.pendingKeyPackage = null;
      this.clearRebroadcast();
      this.foundGroup();
    }, OWNER_REJOIN_PROBE_MS);
  }

  private startRebroadcast(): void {
    if (this.rebroadcastTimer !== null) return;
    this.rebroadcastTimer = setInterval(() => {
      if (this.pendingKeyPackage === null) {
        this.clearRebroadcast();
        return;
      }
      this.broadcastKeyPackage();
    }, KEY_PACKAGE_REBROADCAST_MS);
  }

  private clearRebroadcast(): void {
    if (this.rebroadcastTimer !== null) {
      clearInterval(this.rebroadcastTimer);
      this.rebroadcastTimer = null;
    }
  }

  private clearOwnerProbe(): void {
    if (this.ownerProbeTimer !== null) {
      clearTimeout(this.ownerProbeTimer);
      this.ownerProbeTimer = null;
    }
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

  /** Look up a roster participant id (`peer-<id>`) to its light companion
   *  stream, when its publisher sends one. */
  remoteLightStreamFor(participantId: string): MediaStream | undefined {
    const peerId = participantId.startsWith(PEER_PREFIX)
      ? participantId.slice(PEER_PREFIX.length)
      : participantId;
    return this.remoteLightStreams().get(peerId);
  }

  /** Look up a roster participant id (`peer-<id>`) to its middle stream,
   *  when its publisher sends one. */
  remoteMiddleStreamFor(participantId: string): MediaStream | undefined {
    const peerId = participantId.startsWith(PEER_PREFIX)
      ? participantId.slice(PEER_PREFIX.length)
      : participantId;
    return this.remoteMiddleStreams().get(peerId);
  }

  /** Look up a roster participant id (`peer-<id>`) to the picture the SFU
   *  says it gives of that person; `undefined` when it has not said. */
  remoteVideoGivenFor(participantId: string): VideoQuality | undefined {
    const peerId = participantId.startsWith(PEER_PREFIX)
      ? participantId.slice(PEER_PREFIX.length)
      : participantId;
    return this.remoteVideoGiven().get(peerId);
  }

  /** Look up a roster participant id (`peer-<id>`) to the receiver that
   *  decodes that person's audio, for the sound level the call screen reads
   *  on this device (`audio-level.ts`). */
  remoteAudioReceiverFor(participantId: string): RTCRtpReceiver | undefined {
    const peerId = participantId.startsWith(PEER_PREFIX)
      ? participantId.slice(PEER_PREFIX.length)
      : participantId;
    return this.session.audioReceiverFor(peerId);
  }

  /** A tile says which picture of a peer it shows, or `null` when it stops
   *  (`video-wants.ts`); the session asks the SFU accordingly. */
  reportTileVideoQuality(tile: object, peerId: string, quality: VideoQuality | null): void {
    this.session.reportTileVideoQuality(tile, peerId, quality);
  }

  /** Tear down the call. Idempotent. */
  hangUp(): void {
    this.clearAdmissionTimer();
    this.clearRebroadcast();
    this.clearOwnerProbe();
    this.lastKnownCameraOn.clear();
    this.session.hangUp();
    this.setRemoteStreams(() => new Map());
    this.setRemoteLightStreams(() => new Map());
    this.setRemoteMiddleStreams(() => new Map());
    this.setRemoteVideoGiven(new Map());
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
    this.clearRebroadcast();
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
    if (this.role === "owner" && this.group === null && !this.joined) {
      if (remotes.length === 0) {
        this.foundGroup();
      } else {
        this.beginOwnerRejoinProbe();
      }
      return;
    }
    this.broadcastKeyPackage();
    if (this.pendingKeyPackage !== null) this.startRebroadcast();
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
    const memberKeyHex = this.presence.memberKeyFor(peerId);
    this.presence.removePeer(peerId);
    const id = `${PEER_PREFIX}${peerId}`;
    // Forget the departed peer's sticky camera state so a later rejoin under
    // the same id starts from a clean slate (join-race guard).
    this.lastKnownCameraOn.delete(id);
    this.peerNames.delete(peerId);
    this.peerUsernames.delete(peerId);
    this.peerPersons.delete(peerId);
    this.namingInputs.delete(peerId);
    // The companion and the middle track go with their peer.
    this.setRemoteLightStreams((prev) => withoutLightStream(prev, peerId));
    this.setRemoteMiddleStreams((prev) => withoutLightStream(prev, peerId));
    callActions.participantLeft(id);
    // Someone who leaves can come back: forget their admission (else the
    // G-22 dedup re-serves a Welcome a fresh page cannot open and they wait
    // forever) and, if this client is now the acting committer, remove their
    // leaf so the key rotates away from them and the group holds only the
    // people in the room.
    this.forgetAdmission(peerId, memberKeyHex);
    if (memberKeyHex !== undefined) this.removeDepartedMember(memberKeyHex);
  }

  private forgetAdmission(peerId: string, memberKeyHex: string | undefined): void {
    this.admittedByPeer.delete(peerId);
    const kpKey = this.kpKeyByPeer.get(peerId);
    if (kpKey !== undefined) {
      this.admittedByKp.delete(kpKey);
      this.kpKeyByPeer.delete(peerId);
    }
    if (memberKeyHex !== undefined) this.admittedBySigKey.delete(memberKeyHex);
  }

  /** Acting committer: remove a departed member's leaf and rekey. */
  private removeDepartedMember(memberKeyHex: string): void {
    if (this.group === null || !this.joined) return;
    if (memberKeyHex === hexKey(this.opts.identity.signaturePublicKey)) return;
    const member = this.groupMembers().find((m) => m.signatureKeyHex === memberKeyHex);
    if (member === undefined) return;
    if (!this.isActingCommitter()) return;
    let commit: Uint8Array;
    try {
      commit = this.group.removeMember(this.opts.identity, member.leafIndex);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("multi-joiner: could not remove a departed member", err);
      return;
    }
    this.session.sendSignalRelayToCall(commit);
    this.rekeyToCurrentEpoch();
    this.refreshRoster();
  }

  private onRemoteTrack(rt: SfuRemoteTrack): void {
    // A light companion or a middle track (`light-stream.ts`) goes into its
    // own map and touches nothing else: no roster entry, no camera flag, no
    // entry in `remoteStreams`. Mirrors `sfu-call-ui.ts` `addCopyStream`.
    const id = splitStreamId(rt.streamId);
    if (id.role !== "main") {
      if (rt.track.kind !== "video") return;
      const set = id.role === "light" ? this.setRemoteLightStreams : this.setRemoteMiddleStreams;
      set((prev) => withLightStream(prev, id.peerId, rt.stream));
      rt.track.addEventListener("ended", () => {
        set((prev) => withoutLightStream(prev, id.peerId, rt.stream));
      });
      return;
    }
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
        this.onMaybeCommit(from, env.payload);
        break;
      case K.Profile:
        // ADR-0048 — a bare (unsealed) profile on the relay binds to nobody:
        // the SFU peer id is not an identity. Names travel sealed inside the
        // call group instead (`broadcastOwnProfile`), and surface through
        // `onMaybeCommit` as an MLS application message. Dropped here.
        break;
      case K.RegistrarSeed:
        // ADR-0057 - no client consumes a registrar seed yet. Never a
        // commit, never rendered: dropped.
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
    this.kpKeyByPeer.set(from, kpKey);
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
    // The newcomer decrypts only from this epoch on: tell it who we are.
    void this.broadcastOwnProfile();
  }

  /** Joiner side: open the committer's Welcome and go live. */
  private onIncomingWelcome(from: string, welcomeEnvelope: Uint8Array): void {
    // The returning owner is a joiner here too (see `host`).
    if (this.joined || this.pendingKeyPackage === null) return;
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
    void this.broadcastOwnProfile();
    this.replayEarlyRelays();
  }

  /** Process what arrived before the Welcome. A payload this epoch cannot
   *  open is dropped inside `onMaybeCommit`, as it always was. */
  private replayEarlyRelays(): void {
    for (const relay of this.earlyRelays.drain()) this.onMaybeCommit(relay.from, relay.bytes);
  }

  /** Existing member: apply a broadcast MLS message — a commit (epoch
   *  advance → rekey) or a member's sealed application message (today: the
   *  ADR-0048 profile that names its tile). */
  private onMaybeCommit(from: string, bytes: Uint8Array): void {
    if (this.group === null) {
      // Not readable yet: a member's profile can outrun the Welcome. Kept
      // and replayed once the group exists (`replayEarlyRelays`).
      this.earlyRelays.keep({ from, bytes });
      return;
    }
    let processed: WasmProcessedMessage;
    try {
      // The call group has no ADR-0022 role table — plain apply (the call
      // path caveat). A wrong-epoch / junk payload throws and is ignored.
      processed = this.group.processIncoming(this.opts.identity, bytes);
    } catch {
      return;
    }
    if (processed.kind === "Application") {
      // The MLS-authenticated sender leaf names the member behind this SFU
      // peer: bind them, so a later departure of that peer can be answered
      // with the removal of the right leaf whoever ends up committer.
      if (processed.senderLeaf !== undefined) {
        const sender = this.groupMembers().find((m) => m.leafIndex === processed.senderLeaf);
        if (sender !== undefined) this.presence.bindPeerToMember(from, sender.signatureKeyHex);
      }
      if (processed.applicationData !== undefined) {
        // The account key resolved at receipt names the PERSON (ADR-0050,
        // C-68); a leaf with no device binding is its own account key.
        const account = processed.senderAccount;
        const senderKeyB64 =
          account !== undefined && account.length === 32 ? base64Encode(account) : null;
        // Whether the MLS-authenticated sender is the link's owner: the key
        // the SIGNED capability names, matched the way the join check does.
        const fromOwner =
          processed.senderLeaf !== undefined &&
          this.groupMembers().some(
            (m) => m.leafIndex === processed.senderLeaf && m.signatureKeyHex === this.ownerKeyHex,
          );
        this.onMemberApplication(from, senderKeyB64, fromOwner, processed.applicationData);
      }
      return;
    }
    if (processed.kind !== "Commit") return;
    this.rekeyToCurrentEpoch();
    this.refreshRoster();
    // Someone was admitted: they decrypt only from this epoch on, so every
    // member re-announces its name for the newcomer.
    void this.broadcastOwnProfile();
  }

  /** A member's sealed application message. Only a profile is understood;
   *  anything else is ignored. The profile binds to the relaying SFU peer —
   *  the same binding the peer's media tracks already rest on — and to the
   *  person who sealed it, so the tile is named by the one resolver: a
   *  person who set no profile name still reads as their verified
   *  `@username`, and a contact as what the viewer calls them. */
  private onMemberApplication(
    from: string,
    senderKeyB64: string | null,
    fromOwner: boolean,
    plaintext: Uint8Array,
  ): void {
    if (this.isEndForEveryone(plaintext)) {
      // Acted on only from the link's owner; from anyone else it is ignored.
      if (fromOwner) this.opts.onEndedForEveryone?.(from);
      return;
    }
    let profile: AnnouncedProfile;
    try {
      const decoded = this.wasm.decodeProfileEnvelope(plaintext);
      profile = {
        displayName: decoded.displayName ?? null,
        username: decoded.username ?? null,
        updatedAtMs: decoded.updatedAtMs,
        accountId: decoded.accountId ?? null,
        usernameCertificate: decoded.usernameCertificate ?? null,
      };
    } catch {
      return;
    }
    const name = profile.displayName?.trim() ?? "";
    if (name.length > 0) this.peerNames.set(from, name);
    else this.peerNames.delete(from);
    const username = profile.username?.trim() ?? "";
    if (username.length > 0) this.peerUsernames.set(from, username);
    else this.peerUsernames.delete(from);
    // Filed in the profile store, and bound to a person, only when the
    // account the profile claims is the account the SFU admitted this peer
    // as. Otherwise the tile shows what was announced, as plain text.
    const claimed = profileClaimMatchesPeer(
      accountIdHexOfBytes(profile.accountId),
      this.namingInputs.get(from)?.peerTag,
      (accountIdHex) => this.wasm.sfuPeerTag(groupIdHex(this.opts.callId), accountIdHex),
    );
    if (senderKeyB64 !== null && claimed) {
      this.peerPersons.set(from, senderKeyB64);
      this.opts.onMemberProfile?.(senderKeyB64, profile);
    } else {
      this.peerPersons.delete(from);
    }
    this.renameTile(from);
  }

  /** Whether a sealed application message is "this call has been ended for
   *  everyone". States an intent only; the caller checks who sent it. */
  private isEndForEveryone(plaintext: Uint8Array): boolean {
    try {
      const parsed = this.wasm.parseCallSignaling(plaintext);
      try {
        return (
          parsed.kind === "end" &&
          parsed.endReason === ENDED_FOR_ALL &&
          groupIdHex(parsed.callId) === groupIdHex(this.opts.callId)
        );
      } finally {
        parsed.free();
      }
    } catch {
      return false;
    }
  }

  /**
   * End the call for every person in it. Only the link's owner may: every
   * other member checks the sender against the owner key of the signed
   * capability and ignores it from anyone else. Sealed into the call's own
   * group, so the SFU relays ciphertext. Best-effort: the caller hangs up
   * whether or not this went out.
   */
  endForEveryone(): void {
    if (this.role !== "owner" || this.group === null) return;
    try {
      const payload = this.wasm.composeCallEndSignaling(this.opts.callId, ENDED_FOR_ALL);
      const sealed = this.group.createApplicationMessage(this.opts.identity, payload);
      this.session.sendSignalRelayToCall(sealed);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("multi-joiner: the end for everyone was not sent", err);
    }
  }

  /** Re-run the naming rule for every remote tile. The host calls it when
   *  the profile store changes: a username verified, a contact renamed. */
  refreshNames(): void {
    for (const peerId of this.namingInputs.keys()) this.renameTile(peerId);
  }

  private renameTile(peerId: string): void {
    const inputs = this.namingInputs.get(peerId);
    if (inputs === undefined) return;
    callActions.setParticipantName(
      `${PEER_PREFIX}${peerId}`,
      this.nameOf(peerId, inputs.pseudonym, inputs.peerTag),
    );
  }

  /** Seal the own profile into the call group and relay it to every member.
   *  Best-effort: a missing profile or a failed send only leaves the tile on
   *  its fallback label. */
  private async broadcastOwnProfile(): Promise<void> {
    if (this.group === null || !this.joined) return;
    let envelope: Uint8Array | null;
    try {
      envelope = (await this.opts.ownProfileEnvelope?.()) ?? null;
    } catch {
      return;
    }
    if (envelope === null || this.group === null) return;
    try {
      const sealed = this.group.createApplicationMessage(this.opts.identity, envelope);
      this.session.sendSignalRelayToCall(sealed);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("multi-joiner: own profile not announced", err);
    }
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
    this.namingInputs.set(peerId, { pseudonym, peerTag });
    const person = this.peerPersons.get(peerId);
    // Never the SFU pseudonym (a piece of the account id). An empty name is
    // the call screen's unnamed state, for the moment before the person's
    // profile reaches this device (`tile-name.ts`).
    return (
      callTileName({
        resolved:
          (person === undefined ? null : (this.opts.resolvePersonName?.(person) ?? null)) ??
          this.opts.resolveDisplayName?.(peerId, pseudonym, peerTag) ??
          null,
        announcedName: this.peerNames.get(peerId) ?? null,
        claimedUsername: this.peerUsernames.get(peerId) ?? null,
      }) ?? ""
    );
  }

  private localParticipant(): Participant {
    return {
      id: "local",
      displayName: this.opts.localDisplayName ?? t("call.participant.you"),
      isLocal: true,
      // See `sfu-call-ui.ts`: `media.audio` is always true on web, so a muted
      // start is carried in the store instead (docs/ux/calls.md section 3.2).
      micOn:
        this.captureFallback?.missing === "camera-and-microphone"
          ? false
          : callState.pendingMicOn(),
      camOn: this.opts.media.video && this.captureFallback === null,
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

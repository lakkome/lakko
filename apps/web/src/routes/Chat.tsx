// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `/chat/:id` route — full timeline + composer for a 1×1 MLS
 * group (Phase 1.16e5).
 *
 * The `:id` URL segment is a **base64url-without-padding**
 * encoding of the MLS `group_id` bytes (matches the
 * `paths.chat(...)` helper). On mount we decode the segment,
 * look up the live `<ChatProvider>` registry, and render the
 * timeline. If the group isn't registered (typical case: the
 * page was opened directly via URL with no in-memory state —
 * Phase-1 doesn't persist MLS groups across reloads), we
 * surface a "group not found, return to /contacts" panel.
 *
 * Composer + timeline are largely unchanged from the inline
 * `ChatPane` that 1.16e4 shipped in `Contacts.tsx`. Two
 * additions:
 *
 *   * Full-page layout (the `AppShell` <main> takes care of
 *     scrolling). Timeline grows with the viewport rather than
 *     being clamped at 16rem.
 *   * **Ack indicators** on outbound bubbles — `pending` while
 *     the POST to the DS is in flight, `sent` once the DS has
 *     assigned an id (the publish call returns). Cross-peer
 *     "delivered" / "seen" markers need a Read-Ack ladder
 *     primitive — tracked as a follow-up.
 *
 * `EditChain` LWW for per-message drafts (the lakko-sync
 * surface) is also a follow-up — current composer is a plain
 * `<input>`, no edit history.
 */
import {
  createEffect,
  createMemo,
  createSignal,
  For,
  onMount,
  onCleanup,
  Show,
  type JSX,
} from "solid-js";
import { A, useNavigate, useParams } from "@solidjs/router";
import { useAuth } from "../auth/AuthContext";
import { useDeviceGroupSync } from "../devices/sync/DeviceGroupSyncContext";
import { groupIdHex as hexOf } from "../devices/sync/state";
import {
  CallSession,
  mintCallId,
  type WebCallState,
  type WebCallEndReason,
  type WebConnectionStats,
} from "../calls/call-session";
import {
  callActions,
  callState as callUi,
  qualityColour,
  type Participant,
  type QualityTier,
} from "../state/call-state";
import { createSfuCallUi, type SfuCallUiHandle } from "../calls/sfu-call-ui";
import { sfuBaseUrl } from "../calls/sfu-config";
import { CallJwtClient } from "../calls/call-jwt-client";
import Call from "./Call";
import { DsClient } from "../ds/client";
import { loadWasm as loadWasmForCalls } from "../wasm";
import {
  loadMessageLog,
  saveMessageLog,
  type MessageLogPayload,
  type PersistedAttachment,
  type PersistedGroup,
  type PersistedMessage,
} from "../chat/message-log-store";
import { getDeviceKey } from "../auth/key-store";
import { base64Encode, base64UrlDecodeNoPad } from "../auth/account-id";
import { ChatClient } from "../chat/client";
import { ContactsClient } from "../contacts/client";
import {
  loadAccepted,
  peerContactByMemberPubkeys,
} from "../contacts/local-store";
import type { IncomingChatEvent } from "../chat/client";
import {
  composeEnvelope,
  composeEditEnvelope,
  composeDeleteEnvelope,
  composeReactionEnvelope,
  downloadAndDecryptRef,
  encryptAndUploadFile,
  formatAttachmentSize,
  isInlinePreviewable,
  saveDecryptedBytes,
  type AttachmentRefBytes,
  AttachmentSizeError,
  AttachmentMissingError,
  MAX_ATTACHMENT_BYTES,
} from "../chat/attachments";
import { useChats, type ActiveGroup } from "../chat/ChatContext";
import {
  applyOp,
  editDeleteAuthorized,
  OWN_REACTION_SENTINEL,
  REACTION_EMOJI,
  type MessageOpState,
} from "../chat/message-ops";
import { paths } from "./paths";

/** A timeline entry — verified incoming events PLUS optimistic
 *  outbound bubbles that haven't been confirmed yet.
 *
 *  Phase 1.19c2: outgoing bubbles now carry an optional
 *  `attachments` field so the optimistic UI can show the chips
 *  immediately after Send (the bytes are already uploaded to
 *  storage by the time the bubble renders). */
type TimelineRow =
  | { kind: "incoming"; event: IncomingChatEvent }
  | {
      kind: "outgoing";
      /** Stable client-side id for `<For>` keying — distinct
       *  from any DS-assigned id. */
      tempId: number;
      plaintext: string;
      attachments: AttachmentRefBytes[];
      ack: "pending" | "sent";
      /** Filled when ack flips to "sent". */
      messageId?: number;
      createdAt?: string;
    };

/** Read the author leaf a displayed row should be gated on:
 *  incoming application messages carry the original author's
 *  `senderLeaf`; outgoing rows are authored by us. `undefined`
 *  ⇒ no author leaf is known for this row (a reloaded message,
 *  a control frame) — its edit/delete gate fails closed. */
function rowAuthorLeaf(
  row: TimelineRow,
  ownLeaf: number | undefined,
): number | undefined {
  if (row.kind === "outgoing") return ownLeaf;
  if (row.event.kind === "application") return row.event.senderLeaf;
  return undefined;
}

/** The DS `messageId` of a displayed row, or `undefined` when
 *  it doesn't have one yet (an un-acked outgoing bubble). Every
 *  edit/delete/react affordance is gated on this being present
 *  (the op references its target by id). */
function rowMessageId(row: TimelineRow): number | undefined {
  if (row.kind === "outgoing") {
    return row.ack === "sent" ? row.messageId : undefined;
  }
  if (row.event.kind === "application") return row.event.messageId;
  return undefined;
}

export default function Chat(): JSX.Element {
  const auth = useAuth();
  const chats = useChats();
  const params = useParams<{ id: string }>();
  const navigate = useNavigate();

  // Decode the URL slug into a groupId base64-standard (the
  // ChatProvider's registry key).
  //
  // 1.18e: the inline base64url-no-pad → standard-base64
  // conversion that lived here since 1.16e5 is now the shared
  // `base64UrlDecodeNoPad` helper (added in 1.18c next to its
  // encode-side sibling). Folding the duplicate keeps both
  // call sites — `/chat/:id` and `/i/:slug` — converging on
  // one tested decoder.
  const groupIdB64 = (): string | null => {
    try {
      const bytes = base64UrlDecodeNoPad(params.id);
      return base64Encode(bytes);
    } catch {
      return null;
    }
  };

  const entry = (): ActiveGroup | undefined => {
    const id = groupIdB64();
    if (id === null) return undefined;
    return chats.get(id);
  };

  // Identity gate — the chat needs a fresh export key in
  // memory to derive Identity for processIncoming /
  // createApplicationMessage.
  const requireIdentity = (): NonNullable<
    ReturnType<typeof auth.identitySecrets>
  > => {
    const s = auth.identitySecrets();
    if (s === null) throw new Error("identity unavailable");
    return s;
  };

  return (
    <article>
      <Show
        when={auth.identitySecrets() !== null}
        fallback={<NeedsReauth />}
      >
        <Show
          when={entry() !== undefined}
          fallback={<GroupNotFound />}
        >
          <ChatView
            entry={entry() as ActiveGroup}
            requireIdentity={requireIdentity}
            onLeave={() => navigate(paths.contacts)}
          />
        </Show>
      </Show>
    </article>
  );
}

// ---------------------------------------------------------------
// G3-3-conn-quality — in-call connection-quality display helpers
// ---------------------------------------------------------------

/**
 * Bucket a measured round-trip time into the shared
 * [`QualityTier`]. These are UX display bands (NOT measured
 * SLOs) matching the iOS / Android clients exactly: a `null`
 * RTT (pre-measurement) reads as `good` so a freshly-connected
 * call doesn't flash a red dot before the first stats sample.
 */
function qualityTierForRtt(rttMs: number | null): QualityTier {
  if (rttMs === null) return "good";
  if (rttMs < 100) return "excellent";
  if (rttMs < 200) return "good";
  if (rttMs < 350) return "fair";
  if (rttMs < 600) return "poor";
  return "bad";
}

/** Format a bits-per-second rate for the in-call readout.
 *  Mirrors the `formatBps` in the Call.tsx design route. */
function formatBps(bps: number | null): string {
  if (bps === null) return "—";
  if (bps >= 1_000_000) return `${(bps / 1_000_000).toFixed(1)} Mbps`;
  if (bps >= 1_000) return `${(bps / 1_000).toFixed(0)} kbps`;
  return `${bps} bps`;
}

/** Map a [`QualityTier`] to a concrete CSS colour for the dot. */
function qualityDotColour(tier: QualityTier): string {
  switch (qualityColour(tier)) {
    case "green":
      return "#22c55e";
    case "yellow":
      return "#eab308";
    case "red":
      return "#ef4444";
  }
}

// ---------------------------------------------------------------
// Main view
// ---------------------------------------------------------------

function ChatView(props: {
  entry: ActiveGroup;
  requireIdentity: () => NonNullable<
    ReturnType<ReturnType<typeof useAuth>["identitySecrets"]>
  >;
  onLeave: () => void;
}): JSX.Element {
  // 1.16e6c2: wire the canonical Identity + post-mutation
  // snapshot persist into ChatClient so `sendMessage` /
  // `pollMessages` flush MLS state to IDB on every successful
  // op. Closures read the auth + chat contexts lazily so they
  // always see the current session.
  const auth = useAuth();
  const chats = useChats();
  // Phase 2a3e2b-web — cross-device sync for per-thread state
  // (drafts now; flags / order / read cursors follow). Used
  // below to seed the composer from sibling-device drafts
  // and to broadcast composer edits back out.
  const sync = useDeviceGroupSync();
  // Raw bytes of the conversation group_id — fed straight to
  // the sync API and into the LWW resolver keying. `Group.groupId`
  // is a wasm-bindgen getter (no parens), distinct from
  // `DeviceGroup.groupId()` which is a method (the surfaces
  // diverged in the Rust source). `createMemo` so the
  // closure-capturing effects below see a stable reference.
  const groupIdBytes = createMemo(() => props.entry.group.groupId);
  const chat = new ChatClient(undefined, {
    identityProvider: () => chats.identity(),
    onPersist: async () => {
      const s = auth.state();
      if (s.kind !== "session") return;
      await chats.persistSnapshot(s.account.accountId);
    },
  });
  // 1.18g — ContactsClient is the home of the addMemberToGroup
  // flow (resolves @username via discovery + drives the MLS
  // addMember + ships Welcome). Same canonical-Identity +
  // persist plumbing as ChatClient — both end up calling
  // `chats.persistSnapshot` after every mutation.
  const contacts = new ContactsClient(undefined, undefined, {
    identityProvider: () => chats.identity(),
    onPersist: async () => {
      const s = auth.state();
      if (s.kind !== "session") return;
      await chats.persistSnapshot(s.account.accountId);
    },
  });

  const [rows, setRows] = createSignal<TimelineRow[]>([]);
  const [cursor, setCursor] = createSignal(0);

  // ADR-0023 — per-message op overlay (edit / delete / reaction).
  // Keyed on the target's DS `messageId`. In-memory + session-
  // scoped: multi-device convergence (the persisted-log replay
  // of ops) is deferred to Phase 2, so a reload starts the
  // overlay empty. The render path reads it by messageId and
  // folds it onto the bubble; the receive loop folds incoming
  // ops into it; the send handlers apply the own-op locally
  // (the sender never receives its own application message
  // back). A fresh Map replaces the signal on every mutation so
  // SolidJS re-renders the affected bubbles.
  const [msgOps, setMsgOps] = createSignal<Map<number, MessageOpState>>(
    new Map(),
  );
  // Inline-edit composer state — the messageId currently being
  // edited (or `null`), plus the working text.
  const [editingId, setEditingId] = createSignal<number | null>(null);
  const [editText, setEditText] = createSignal("");
  // The messageId whose reaction picker is open (or `null`).
  const [reactPickerId, setReactPickerId] = createSignal<number | null>(null);

  // Phase 2a3f1-web — persistent message log. The plaintext
  // timeline survives reloads under the device root key in
  // IDB; without it, every refresh emptied the visible
  // history (MLS forward secrecy wipes the chain keys, so
  // there's no way to re-derive past plaintexts from the wire
  // bytes). The two stores (MLS snapshot ⊥ message log) ride
  // side-by-side keyed on the same `accountIdB64`.
  const accountIdB64ForLog = (): string | null => {
    const s = auth.state();
    if (s.kind !== "session") return null;
    return s.account.accountId;
  };
  const groupIdHexForLog = (): string => hexOf(props.entry.group.groupId);
  let didHydrateFromLog = false;
  let pendingPersistTimer: ReturnType<typeof setTimeout> | null = null;
  const PERSIST_DEBOUNCE_MS = 500;
  const [input, setInput] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [notice, setNotice] = createSignal<string | null>(null);

  // Phase 3a8 — 1×1 call surface. Only one CallSession can
  // ride a given chat thread at a time; the field tracks
  // the active session (if any) + its observable state.
  // Audio is bound to a hidden `<audio>` element via the
  // ref below; the call overlay shows the duration / hang
  // up button.
  const [callState, setCallState] = createSignal<WebCallState>("idle");
  const [callEndReason, setCallEndReason] = createSignal<WebCallEndReason | null>(null);
  // G3-3-conn-quality — live connection-quality sample from the
  // session's getStats poll loop. null until the first tick lands.
  const [callConnStats, setCallConnStats] =
    createSignal<WebConnectionStats | null>(null);
  let activeCall: CallSession | null = null;
  const [remoteAudioStream, setRemoteAudioStream] = createSignal<MediaStream | null>(
    null,
  );
  const [audioRef, setAudioRef] = createSignal<HTMLAudioElement | null>(null);
  // SFU multi-party (group) call. Distinct from the 1×1 `activeCall`
  // above — a group chat (>2 members) routes through the Rust SFU with
  // SFrame, while a 1×1 chat stays P2P DTLS-SRTP. `sfuHandle` is held in
  // a signal so the remote-stream accessor reads reactively in JSX;
  // `sfuActive` gates the in-call overlay (the SFU path leaves the 1×1
  // `callState()` at `idle`).
  const [sfuHandle, setSfuHandle] = createSignal<SfuCallUiHandle | null>(null);
  const [sfuActive, setSfuActive] = createSignal(false);
  createEffect(() => {
    const el = audioRef();
    const stream = remoteAudioStream();
    if (el !== null && stream !== null) {
      el.srcObject = stream;
      void el.play().catch(() => {
        // Autoplay might be blocked until user gesture —
        // the Accept button click on the receiver counts.
      });
    }
  });

  // -----------------------------------------------------------------
  // G3-3-web-session-bind — drive the full Call.tsx in-call UI (the
  // shared `callActions` store) from the live `CallSession`. The web
  // analog of iOS/Android driveFrom + call-present: the rich UI is
  // presented as a full-screen overlay over the chat for live call
  // states, while `ringing` keeps the lightweight inline banner
  // (Accept / Decline).
  // -----------------------------------------------------------------

  /** True while the full in-call overlay should cover the chat.
   *  `ringing` is excluded — an incoming call shows the inline
   *  banner until accepted. */
  const callOverlayVisible = (): boolean => {
    if (sfuActive()) return true;
    const s = callState();
    return (
      s === "outgoing" ||
      s === "connecting" ||
      s === "connected" ||
      s === "ending"
    );
  };

  /** Synthesised local participant. Web 1×1 is audio-only, so
   *  `camOn` is false. `micOn` seeds true; the live mic state is
   *  driven through `callActions.setMic` ↔ `setMicrophoneEnabled`
   *  (see the effect below). Quality dot tracks the live RTT
   *  sample. */
  const localParticipant = (): Participant => ({
    id: "local",
    displayName: "You",
    isLocal: true,
    micOn: true,
    camOn: false,
    handRaised: false,
    quality: qualityTierForRtt(callConnStats()?.rttMs ?? null),
    speaking: false,
    screenSharing: false,
  });

  /** Synthesised sole remote participant for the 1×1 call. The web
   *  P2P path carries no roster, so the single remote IS this chat's
   *  contact. ADR-0021 follow-up: resolve the linked contact's
   *  user-chosen displayName (matched by this chat's group id), so a
   *  rename shows in the in-call roster; fall back to the chat label,
   *  then "Peer". Read at connect time, so a pre-call rename is
   *  reflected. */
  const remoteParticipant = (): Participant => ({
    id: "peer",
    displayName: resolvePeerName(),
    isLocal: false,
    micOn: true,
    camOn: false,
    handRaised: false,
    quality: "good",
    speaking: false,
    screenSharing: false,
  });

  /** Best contact name for this 1×1 chat: the linked contact's
   *  displayName (ADR-0021), else the chat label, else "Peer". */
  const resolvePeerName = (): string => {
    const accepted = loadAccepted();
    // Fast path: a chat created from an accepted request carries a stored
    // groupIdB64 link to its contact.
    const byId = accepted.find((c) => c.groupIdB64 === props.entry.groupIdB64);
    if (byId !== undefined && byId.displayName.length > 0) {
      return byId.displayName;
    }
    // Universal: match the 1×1 peer by the group's non-self member pubkey, so
    // an outbound or Welcome-joined chat (no stored link) still resolves.
    const own = chats.identity();
    const ownB64 = own === null ? null : base64Encode(own.signaturePublicKey);
    let memberB64s: string[] = [];
    try {
      memberB64s = (
        props.entry.group.members() as Array<{ signaturePublicKey: Uint8Array }>
      ).map((m) => base64Encode(m.signaturePublicKey));
    } catch {
      // members() can throw on a torn group; fall through to the label.
    }
    const byMember = peerContactByMemberPubkeys(memberB64s, ownB64, accepted);
    if (byMember !== undefined && byMember.displayName.length > 0) {
      return byMember.displayName;
    }
    return props.entry.label.length > 0 ? props.entry.label : "Peer";
  };

  /** Map the session's `WebCallState` onto the shared call-UI
   *  store (phase + 1×1 roster). Called from `onStateChange`. */
  const syncCallUiFromState = (s: WebCallState): void => {
    switch (s) {
      case "outgoing":
      case "connecting":
        callActions.setPhase({ kind: "connecting" });
        callActions.setParticipants([localParticipant()]);
        // 1×1 web media is P2P DTLS-SRTP, not SFrame — keep the
        // E2EE indicator inactive (and the text pill is hidden in
        // the embedded view regardless).
        callActions.setE2ee({ active: false, shortFingerprint: null });
        break;
      case "connected":
        callActions.setPhase({ kind: "connected" });
        callActions.setParticipants([localParticipant(), remoteParticipant()]);
        break;
      case "ending":
        // Transient — hold the connected visuals until `done`.
        break;
      case "idle":
      case "ringing":
      case "done":
        // `ringing` uses the inline banner (no overlay); `done` /
        // `idle` tear the overlay down.
        callActions.reset();
        break;
    }
  };

  // When the user hits "End call" inside the embedded Call.tsx, it
  // sets the store phase to `ended`; mirror that onto the real
  // session by hanging up (idempotent — a remote-initiated end
  // already drove the session to `done`).
  createEffect(() => {
    if (callUi.phase().kind !== "ended") return;
    if (activeCall !== null) {
      void activeCall.hangUp("hungUp");
    }
    const h = sfuHandle();
    if (h !== null) {
      h.hangUp();
      setSfuHandle(null);
      setSfuActive(false);
    }
  });

  // G3-3-web-call-controls — drive the real microphone from the
  // store's local-participant `micOn` flag. The Call.tsx mic button
  // toggles `callActions.setMic`; this mirrors it onto the live
  // session (idempotent — `track.enabled = on` is cheap, so the
  // redundant calls on each quality patch are harmless). Web 1×1 is
  // audio-only, so this is the only real in-call control; the camera
  // button stays UI-only until web video calls land.
  createEffect(() => {
    const local = callUi.participants().find((p) => p.isLocal);
    if (local === undefined) return;
    if (activeCall !== null) {
      activeCall.setMicrophoneEnabled(local.micOn);
    }
    const h = sfuHandle();
    if (h !== null) {
      h.setMicrophoneEnabled(local.micOn);
    }
  });

  /** Build outbound-signaling callback that PUTs raw
   *  bytes onto the chat group's DS journal as an MLS
   *  application message. */
  function publishSignalingFor(group: ActiveGroup): (bytes: Uint8Array) => void {
    return (bytes: Uint8Array): void => {
      void (async (): Promise<void> => {
        try {
          const id = chats.identity();
          if (id === null) {
            // eslint-disable-next-line no-console
            console.warn(
              "call signaling publish skipped: no canonical Identity in session",
            );
            return;
          }
          const out = group.group.createApplicationMessage(id, bytes);
          const ds = new DsClient();
          await ds.publishGroup(group.group.groupId, out);
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("call signaling publish failed", err);
        }
      })();
    };
  }

  function buildCallSession(callId: Uint8Array): CallSession {
    return new CallSession(callId, {
      onRemoteStream: (s) => setRemoteAudioStream(s),
      onOutboundSignaling: publishSignalingFor(props.entry),
      onStateChange: (s, reason) => {
        setCallState(s);
        if (reason !== undefined) setCallEndReason(reason);
        // G3-3-web-session-bind — drive the full Call.tsx UI store.
        syncCallUiFromState(s);
        if (s === "done") {
          activeCall = null;
          setRemoteAudioStream(null);
          // G3-3-conn-quality — drop the stale sample so the
          // next call doesn't briefly flash the previous one.
          setCallConnStats(null);
        }
      },
      // G3-3-conn-quality — live RTT / bitrate / relay sample,
      // pushed every 2 s while the call is connected. Also feeds
      // the full Call.tsx connection-detail popover + quality dot.
      onConnectionStats: (stats) => {
        setCallConnStats(stats);
        callActions.setConnectionDetail({
          rttMs: stats.rttMs,
          downBps: stats.downBps,
          upBps: stats.upBps,
          connectionType: stats.relayed
            ? "relay"
            : stats.rttMs !== null
              ? "direct"
              : "unknown",
        });
        callActions.setQuality("local", qualityTierForRtt(stats.rttMs));
      },
    });
  }

  async function onCallClicked(): Promise<void> {
    if (activeCall !== null) return;
    const session = buildCallSession(mintCallId());
    activeCall = session;
    try {
      await session.dial({ audio: true, video: false });
    } catch (err) {
      setError(
        `Could not start call: ${err instanceof Error ? err.message : String(err)}`,
      );
      activeCall = null;
    }
  }

  // SFU multi-party (group) call. Routed for group chats (>2 members);
  // 1×1 stays on `onCallClicked` (P2P DTLS-SRTP). Derives the per-call
  // SFrame seed + leaf from the MLS group, mints the call-admission JWT
  // bound to the live session token, and drives the shared call-UI
  // store through `createSfuCallUi`. Fail-closed throughout: any missing
  // prerequisite (SFU URL / session / seed) aborts before media.
  async function startGroupCall(): Promise<void> {
    if (sfuHandle() !== null || activeCall !== null) return;
    const base = sfuBaseUrl();
    if (base === null) {
      setError(
        "Group calls need an SFU endpoint — set VITE_LAKKO_SFU_URL and rebuild.",
      );
      return;
    }
    const id = chats.identity();
    const secrets = auth.identitySecrets();
    if (id === null || secrets === null) {
      setError("Re-authenticate to start a group call (no live session token).");
      return;
    }
    try {
      const wasm = await loadWasmForCalls();
      const callIdBytes = mintCallId();
      const callIdHex = hexOf(callIdBytes);
      // Per-call SFrame seed + this client's leaf. Deterministic per MLS
      // epoch (audit-pack C-1) so every participant derives identical
      // bytes. `ownLeafIndex` is the KID this client stamps into frames.
      const seed = props.entry.group.exportSecret(
        id,
        "lakko/call/v1",
        callIdBytes,
        32,
      );
      const localLeaf = props.entry.group.ownLeafIndex;
      // Canonical call safety number — the same string every participant
      // derives from the shared seed, shown in the E2EE pill for an
      // out-of-band man-in-the-middle check. Display-only + non-critical:
      // a derivation failure must never abort the call.
      let safetyNumber: string | undefined;
      try {
        safetyNumber = wasm.callSafetyNumber(seed);
      } catch {
        safetyNumber = undefined;
      }
      // Call-admission JWT (JWS Compact, EdDSA) bound to the session.
      const jwt = new CallJwtClient(wasm);
      jwt.setSessionToken(secrets.sessionToken);
      const minted = await jwt.mint(callIdHex, "participant");
      // ADR-0021 — precompute the per-call peer_tag -> contact name map
      // once, so the SFU roster resolves to real contact names instead of
      // a pseudonym. The tag is derived locally (the SFU never sends a
      // name); a peer with no matching contact keeps the pseudonym slice.
      const tagToName = new Map<string, string>();
      // ADR-0021: on a (vanishingly rare) tag collision between two
      // contacts, do NOT show one of their names — fall back to the
      // pseudonym, so a collision never surfaces a wrong contact name.
      const collidedTags = new Set<string>();
      for (const c of loadAccepted()) {
        if (c.accountIdHex !== undefined) {
          const tag = wasm.sfuPeerTag(callIdHex, c.accountIdHex);
          if (tagToName.has(tag)) {
            collidedTags.add(tag);
          } else {
            tagToName.set(tag, c.displayName);
          }
        }
      }
      const resolveDisplayName = (
        _peerId: string,
        pseudonym: string,
        peerTag: string | undefined,
      ): string => {
        if (peerTag !== undefined && !collidedTags.has(peerTag)) {
          const name = tagToName.get(peerTag);
          if (name !== undefined) return name;
        }
        return pseudonym.length > 0 ? pseudonym.slice(0, 8) : "Peer";
      };
      const handle = createSfuCallUi(
        {
          sfuBaseUrl: base,
          jwt: minted.jwt,
          seed,
          localLeaf,
          media: { audio: true, video: false },
        },
        { localDisplayName: "You", safetyNumber, resolveDisplayName },
      );
      setSfuHandle(handle);
      setSfuActive(true);
      await handle.connect();
    } catch (err) {
      setError(
        `Could not start group call: ${err instanceof Error ? err.message : String(err)}`,
      );
      sfuHandle()?.hangUp();
      setSfuHandle(null);
      setSfuActive(false);
    }
  }

  async function onHangUp(): Promise<void> {
    if (activeCall === null) return;
    await activeCall.hangUp("hungUp");
  }

  /** Exit the in-call overlay — tears down whichever session is live
   *  (SFU group call or 1×1 P2P). Wired to `Call`'s `onExit`. */
  function onCallExit(): void {
    const h = sfuHandle();
    if (h !== null) {
      h.hangUp();
      setSfuHandle(null);
      setSfuActive(false);
      return;
    }
    void onHangUp();
  }

  async function onAcceptCall(): Promise<void> {
    if (activeCall === null) return;
    await activeCall.accept();
  }

  async function onDeclineCall(): Promise<void> {
    if (activeCall === null) return;
    await activeCall.decline();
  }

  // Route every inbound callSignaling row to the active
  // session (or spin a fresh answerer session up for a
  // first-seen offer).
  createEffect(() => {
    const list = rows();
    for (const row of list) {
      if (row.kind !== "incoming") continue;
      const ev = row.event;
      if (ev.kind !== "callSignaling") continue;
      const bytes = ev.payloadBytes;
      void (async (): Promise<void> => {
        try {
          const wasm = await loadWasmForCalls();
          const parsed = wasm.parseCallSignaling(bytes);
          if (activeCall === null) {
            // Fresh inbound offer — spin up an answerer
            // session bound to the offer's call_id.
            if (parsed.kind !== "offer") return;
            const session = buildCallSession(parsed.callId);
            activeCall = session;
          }
          // Ignore stale signaling targeting a different
          // call_id.
          if (
            activeCall.callId.length === parsed.callId.length &&
            !uint8ArrayEquals(activeCall.callId, parsed.callId)
          ) {
            return;
          }
          await activeCall.applyIncomingSignaling(parsed);
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("inbound call signaling failed", err);
        }
      })();
    }
  });

  onCleanup(() => {
    if (activeCall !== null) {
      activeCall.dispose();
      activeCall = null;
    }
    const h = sfuHandle();
    if (h !== null) {
      h.hangUp();
      setSfuHandle(null);
      setSfuActive(false);
    }
  });

  // Phase 2a3e2b-web: cross-device draft synchronisation.
  //
  // Seed-side effect — on first mount + every time a fresh
  // Draft payload arrives from a sibling device, replace the
  // composer's text with the remote one IF the local composer
  // is currently empty. Choosing empty-only avoids stomping a
  // user mid-typing on this device when a stale sibling-device
  // update lands; the typing-side then broadcasts its own HLC
  // and wins by LWW.
  let seededDraft = false;
  createEffect(() => {
    const draft = sync.drafts()[hexOf(groupIdBytes())];
    if (draft === undefined) return;
    if (seededDraft) return;
    if (input() !== "") {
      // User is already typing on this device — don't stomp.
      // Mark seeded so we don't keep re-checking, and let the
      // local broadcast win the LWW race.
      seededDraft = true;
      return;
    }
    setInput(draft.text);
    seededDraft = true;
  });

  // Outbound broadcast — debounced. Every keystroke fires a
  // signal change; we batch ≤ 1 payload / DRAFT_DEBOUNCE_MS to
  // avoid drowning the DS journal in single-character commits.
  let draftBroadcastTimer: ReturnType<typeof setTimeout> | null = null;
  let lastBroadcastDraft: string | null = null;
  const DRAFT_DEBOUNCE_MS = 500;
  createEffect(() => {
    const text = input();
    if (text === lastBroadcastDraft) return;
    if (draftBroadcastTimer !== null) clearTimeout(draftBroadcastTimer);
    draftBroadcastTimer = setTimeout(() => {
      draftBroadcastTimer = null;
      lastBroadcastDraft = text;
      // Best-effort — a transient DS failure shouldn't crash
      // the composer; the next keystroke will re-broadcast.
      void sync
        .setDraft(groupIdBytes(), text)
        .catch((err: unknown) => {
          // eslint-disable-next-line no-console
          console.warn("setDraft failed", err);
        });
    }, DRAFT_DEBOUNCE_MS);
  });
  onCleanup(() => {
    if (draftBroadcastTimer !== null) {
      clearTimeout(draftBroadcastTimer);
      draftBroadcastTimer = null;
    }
  });
  // 1.19c2 — attachments staged in the composer. The user picks
  // file(s); each gets encrypt+upload immediately so the ref is
  // ready by the time they hit Send. Cleared on Send, on
  // explicit per-chip remove, and on navigation away.
  const [pendingAttachments, setPendingAttachments] = createSignal<
    AttachmentRefBytes[]
  >([]);
  const [uploading, setUploading] = createSignal(false);
  /** Per-file upload-progress snapshot — drives the
   *  "Uploading hello.png (2/5)…" indicator in the composer
   *  tray. `null` when no upload is in flight. The granularity
   *  is one entry per file × two stages (encrypt → upload);
   *  byte-level progress within a single PUT would need a
   *  fetch-stream rewrite (`fetch` doesn't expose upload
   *  progress; XHR does, but the trade-off in code complexity
   *  beats the marginal UX win for sub-MAX_ATTACHMENT_BYTES
   *  files). */
  const [uploadProgress, setUploadProgress] = createSignal<
    | {
        fileIndex: number;
        total: number;
        fileName: string;
        stage: "encrypting" | "uploading";
      }
    | null
  >(null);
  // Cached per-blob_id plaintext blobs for inline preview +
  // re-download. Keyed by hex(blob_id). Re-decrypt is cheap (no
  // network round-trip after the first hit) but caching keeps
  // a clicked image from re-fetching on every render.
  const [decryptedCache, setDecryptedCache] = createSignal<
    Map<string, { bytes: Uint8Array; objectUrl: string | null }>
  >(new Map());
  // 1.18g — Add-member panel state. Inline form expands below
  // the chat header when toggled; collapses after a successful
  // add or explicit Cancel.
  const [addMemberOpen, setAddMemberOpen] = createSignal(false);
  const [addMemberInput, setAddMemberInput] = createSignal("");
  // `memberCount` mirrors `props.entry.group.memberCount` into
  // a reactive signal so the header re-renders after
  // `addMember`. Without this the WASM-side property read in
  // JSX wouldn't re-evaluate on a render driven by a different
  // signal — Solid's fine-grained reactivity only tracks signal
  // accesses, not arbitrary object property reads.
  const [memberCount, setMemberCount] = createSignal(
    props.entry.group.memberCount,
  );
  // 1.18h — members panel state. `membersVersion` is a
  // monotonic counter bumped on every epoch advance (refresh
  // success, addMember success, removeMember success); the
  // `members` createMemo re-reads `group.members()` whenever
  // it changes. Same trick as `memberCount` — Solid only
  // tracks signal reads, not WASM-side property accesses.
  const [membersOpen, setMembersOpen] = createSignal(false);
  const [membersVersion, setMembersVersion] = createSignal(0);
  // 1.18i — accepted-contacts directory keyed by
  // base64(signature_pubkey). Lookup at member-render time
  // turns a hex fingerprint into a real displayName when the
  // member is somebody we've already accepted via the
  // ContactRequest handshake (1.16d3d). Re-read at the same
  // cadence as `members` so a fresh accept-flow happening in a
  // sibling tab on `/contacts` surfaces here without a hard
  // reload. The cost is one localStorage read per
  // membersVersion bump — sub-millisecond.
  const acceptedByPubkey = createMemo<Map<string, string>>(() => {
    membersVersion();
    const map = new Map<string, string>();
    for (const c of loadAccepted()) {
      map.set(c.signaturePubkeyB64, c.displayName);
    }
    return map;
  });
  const members = createMemo<readonly { leafIndex: number; sigPubkey: Uint8Array }[]>(
    () => {
      // Subscribe to the version signal so the memo re-runs.
      membersVersion();
      const arr = props.entry.group.members() as Array<{
        leafIndex: number;
        signaturePublicKey: Uint8Array;
      }>;
      return arr.map((m) => ({
        leafIndex: m.leafIndex,
        sigPubkey: m.signaturePublicKey,
      }));
    },
  );

  // ADR-0022 — the local member's role in this group ("owner" / "admin" /
  // "member"), or `null` for an open / legacy group with no role table. Drives
  // the add / remove affordance gating below. UX only — the receive-loop gate
  // (`processIncomingVerifyingRoles`) is the real boundary; this just hides
  // affordances a plain member's commit would be refused for anyway.
  const [ownRole, setOwnRole] = createSignal<"owner" | "admin" | "member" | null>(null);
  createEffect(() => {
    membersVersion(); // recompute when the roster / epoch changes
    const secrets = props.requireIdentity();
    void (async () => {
      try {
        const wasm = await loadWasmForCalls();
        const identity = wasm.Identity.fromSeed(secrets.accountIdentitySeed);
        try {
          const r = props.entry.group.ownRole(identity);
          if (r === wasm.Role.Owner) setOwnRole("owner");
          else if (r === wasm.Role.Admin) setOwnRole("admin");
          else if (r === wasm.Role.Member) setOwnRole("member");
          else setOwnRole(null);
        } finally {
          identity.free();
        }
      } catch {
        setOwnRole(null);
      }
    })();
  });
  // A plain member may not add / remove; everyone else (owner, admin, or an
  // open/legacy group where `ownRole` is `null`) may.
  const canManageMembers = (): boolean => ownRole() !== "member";
  // ADR-0022 — only the owner may promote / demote admins.
  const isOwner = (): boolean => ownRole() === "owner";

  // The current admin set as hex sig-pubkey strings (re-read per
  // `membersVersion`) — drives the per-member "Make/Remove admin" label.
  const adminPubkeys = createMemo<Set<string>>(() => {
    membersVersion();
    const set = new Set<string>();
    try {
      const flat = props.entry.group.roleTableAdmins() as Uint8Array;
      for (let i = 0; i + 32 <= flat.length; i += 32) {
        set.add(hexOf(flat.slice(i, i + 32)));
      }
    } catch {
      // open / legacy group with no role table — no admins.
    }
    return set;
  });
  // Owner sig-pubkey (hex), re-read per `membersVersion` — the owner row never
  // shows a promote/demote toggle (the owner is fixed).
  const ownerPubkeyHex = createMemo<string | null>(() => {
    membersVersion();
    try {
      const o = props.entry.group.roleTableOwner() as Uint8Array | undefined;
      return o ? hexOf(o) : null;
    } catch {
      return null;
    }
  });

  let nextTempId = 1;

  // ADR-0023 — our own MLS leaf in this group. Used to author-
  // stamp outgoing messages (for the edit/delete gate against an
  // incoming op) and to attribute our own reactions. Re-read on
  // every membersVersion bump (a Remove/Add can renumber leaves).
  const ownLeaf = createMemo<number | undefined>(() => {
    membersVersion();
    try {
      return props.entry.group.ownLeafIndex;
    } catch {
      return undefined;
    }
  });

  /** Look up the author leaf of the displayed message with this
   *  DS `messageId` — incoming app messages carry the original
   *  `senderLeaf`, outgoing rows are authored by us. `undefined`
   *  ⇒ the target is unknown or has no known author (a reloaded
   *  message), so the edit/delete gate fails closed. `extra`
   *  lets the receive loop also scan rows arriving in the same
   *  batch (which aren't in `rows()` yet). */
  function authorLeafForMessageId(
    messageId: number,
    extra: readonly TimelineRow[] = [],
  ): number | undefined {
    for (const row of rows()) {
      if (rowMessageId(row) !== messageId) continue;
      return rowAuthorLeaf(row, ownLeaf());
    }
    for (const row of extra) {
      if (rowMessageId(row) !== messageId) continue;
      return rowAuthorLeaf(row, ownLeaf());
    }
    return undefined;
  }

  /** Whether the target message is known at all (in the live
   *  timeline or this batch). An op against an unknown target is
   *  dropped (fail-closed; the ADR — we never materialise a
   *  bubble for a target we've never displayed). */
  function targetKnown(
    messageId: number,
    extra: readonly TimelineRow[],
  ): boolean {
    return (
      rows().some((r) => rowMessageId(r) === messageId) ||
      extra.some((r) => rowMessageId(r) === messageId)
    );
  }

  /** ADR-0023 — fold one incoming op event into the overlay map,
   *  enforcing the author-only gate for edit/delete. Returns the
   *  (possibly unchanged) map; `===` to the input when the op was
   *  dropped, so the caller can skip a needless signal write.
   *
   *  Gate (fail-closed, invariant #4):
   *   - target unknown                  → DROP
   *   - op.senderLeaf undefined          → DROP (control frame)
   *   - edit/delete + senderLeaf ≠ author → DROP
   *   - reaction                          → accept from any member */
  function applyIncomingOp(
    e: Extract<IncomingChatEvent, { kind: "op" }>,
    extra: readonly TimelineRow[],
    prev: Map<number, MessageOpState>,
  ): Map<number, MessageOpState> {
    if (!targetKnown(e.target, extra)) return prev;
    if (e.senderLeaf === undefined) return prev;
    if (e.op === "reaction") {
      return applyOp(prev, {
        op: "reaction",
        target: e.target,
        emoji: e.emoji ?? "",
        reactorLeaf: e.senderLeaf,
      });
    }
    // edit / delete — author-only (fail-closed, invariant #4).
    const author = authorLeafForMessageId(e.target, extra);
    if (!editDeleteAuthorized(author, e.senderLeaf)) return prev;
    if (e.op === "edit") {
      return applyOp(prev, { op: "edit", target: e.target, text: e.text });
    }
    return applyOp(prev, { op: "delete", target: e.target });
  }

  async function refresh(): Promise<void> {
    setError(null);
    setBusy(true);
    try {
      const result = await chat.pollMessages(
        props.requireIdentity(),
        props.entry.group,
        cursor(),
      );
      // De-dup: the same DS message id can come back across
      // refreshes after a reset; outgoing rows we sent in
      // this session also surface here on a second refresh
      // through a different path (we already filter the echo
      // in ChatClient via ownPublishedIds, so duplicates here
      // are rare).
      const incomingIds = new Set(
        rows().flatMap((r) =>
          r.kind === "incoming" ? [r.event.messageId] : [],
        ),
      );
      // ADR-0023 — split the batch: chat / system / callSignaling
      // events become timeline rows as before; edit / delete /
      // reaction OPS fold into the per-message overlay instead of
      // rendering as standalone bubbles. Each op is applied once
      // (the poll is incremental, so a given op id arrives once).
      const fresh: TimelineRow[] = [];
      let ops = msgOps();
      let opsChanged = false;
      for (const e of result.events) {
        if (e.kind === "op") {
          // Fold the op into the overlay first so a chat row + an
          // op that targets it that arrive in the same batch both
          // land (the chat row appears in `fresh` below; the
          // author-leaf lookup also scans `fresh`).
          const next = applyIncomingOp(e, fresh, ops);
          if (next !== ops) {
            ops = next;
            opsChanged = true;
          }
          continue;
        }
        if (incomingIds.has(e.messageId)) continue;
        fresh.push({ kind: "incoming", event: e });
      }
      if (fresh.length > 0) {
        setRows([...rows(), ...fresh]);
      }
      if (opsChanged) {
        setMsgOps(ops);
      }
      setCursor(result.cursor);
      // 1.18g — mirror the post-poll member count so the
      // header re-renders after an incoming commit advances
      // the local group state (someone else added / removed a
      // member; we processed their commit in pollMessages).
      // No-op when no commits came in: setSignal short-circuits
      // on equal values.
      setMemberCount(props.entry.group.memberCount);
      // 1.18h — bump membersVersion so the members panel's
      // createMemo re-reads the post-poll leaf set.
      setMembersVersion(membersVersion() + 1);
    } catch (err) {
      setError(formatError(err));
    } finally {
      setBusy(false);
    }
  }

  async function onSend(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    const text = input();
    const attached = pendingAttachments();
    // 1.19c2: attachment-only messages are valid (empty text +
    // ≥1 attachment); pre-1.19 text-only messages still valid
    // (≥1 char text + zero attachments). The only invalid
    // input is the all-empty composer.
    if (text.length === 0 && attached.length === 0) return;
    setError(null);

    // Optimistic outbound bubble.
    const tempId = nextTempId++;
    setRows([
      ...rows(),
      {
        kind: "outgoing",
        tempId,
        plaintext: text,
        attachments: attached,
        ack: "pending",
      },
    ]);
    setInput("");
    setPendingAttachments([]);

    try {
      // 1.19c2: compose the JSON envelope from (text, refs).
      // Empty refs ⇒ pure-text envelope; non-empty ⇒ multi-
      // attachment envelope via the wasm EnvelopeBuilder.
      const envelopeBytes = await composeEnvelope(text, attached);
      const result = await chat.sendMessage(
        props.requireIdentity(),
        props.entry.group,
        envelopeBytes,
      );
      // Flip ack → sent + record DS id.
      setRows(
        rows().map((r) =>
          r.kind === "outgoing" && r.tempId === tempId
            ? {
                ...r,
                ack: "sent",
                messageId: result.messageId,
                createdAt: result.createdAt,
              }
            : r,
        ),
      );
    } catch (err) {
      // Roll back the optimistic bubble + restore the
      // attachments to the composer so the user can retry
      // without re-picking the same files.
      setRows(
        rows().filter((r) => !(r.kind === "outgoing" && r.tempId === tempId)),
      );
      setPendingAttachments(attached);
      setError(formatError(err));
    }
  }

  // ----- ADR-0023 edit / delete / reaction send handlers -------
  //
  // The sender never receives its own application message back
  // (the echo is filtered in ChatClient), so each handler BOTH
  // publishes the op AND applies it locally. The op carries a DS
  // `messageId` target; the local apply uses the same overlay
  // map the receive loop folds into. Failures roll the local
  // apply back (or never apply, for delete/react where we apply
  // only after the publish resolves).

  /** Save an inline edit of our own message. Author-gated by the
   *  UI affordance (offered on own messages only) + by the
   *  receive-side gate every peer runs. */
  async function onSaveEdit(messageId: number): Promise<void> {
    const newText = editText();
    // An empty edit is a no-op rather than a silent delete — the
    // user must use Delete explicitly.
    if (newText.length === 0) {
      setEditingId(null);
      return;
    }
    setEditingId(null);
    setError(null);
    try {
      const envelopeBytes = await composeEditEnvelope(messageId, newText);
      await chat.sendMessage(
        props.requireIdentity(),
        props.entry.group,
        envelopeBytes,
      );
      // Apply locally (no echo). Author == us, so the gate the
      // receive loop runs would accept it; we mirror that here.
      setMsgOps(applyOp(msgOps(), { op: "edit", target: messageId, text: newText }));
    } catch (err) {
      setError(formatError(err));
    }
  }

  /** Delete our own message — publish a delete op + tombstone it
   *  locally. */
  async function onDeleteMessage(messageId: number): Promise<void> {
    setError(null);
    try {
      const envelopeBytes = await composeDeleteEnvelope(messageId);
      await chat.sendMessage(
        props.requireIdentity(),
        props.entry.group,
        envelopeBytes,
      );
      setMsgOps(applyOp(msgOps(), { op: "delete", target: messageId }));
    } catch (err) {
      setError(formatError(err));
    }
  }

  /** React to any message with `emoji` — publish a reaction op +
   *  toggle it locally under our own leaf. Re-reacting with the
   *  same emoji toggles it off (the op + the local apply are both
   *  toggles, so they stay in sync). */
  async function onReact(messageId: number, emoji: string): Promise<void> {
    setReactPickerId(null);
    setError(null);
    try {
      const envelopeBytes = await composeReactionEnvelope(messageId, emoji);
      await chat.sendMessage(
        props.requireIdentity(),
        props.entry.group,
        envelopeBytes,
      );
      const reactorLeaf = ownLeaf() ?? OWN_REACTION_SENTINEL;
      setMsgOps(
        applyOp(msgOps(), {
          op: "reaction",
          target: messageId,
          emoji,
          reactorLeaf,
        }),
      );
    } catch (err) {
      setError(formatError(err));
    }
  }

  /**
   * Encrypt + upload an iterable of files sequentially (so a
   * slow user-stream doesn't fan out N parallel storage PUTs),
   * appending the resulting refs to the pending list. Failed
   * uploads surface a per-file error and stop the loop.
   *
   * Shared between the `<input type="file">` Change handler and
   * the drag-drop handler — both surface a `FileList`-ish
   * iterable; this helper is the one place we own the upload
   * loop.
   */
  async function processFiles(files: ArrayLike<File>): Promise<void> {
    if (files.length === 0) return;
    setError(null);
    setUploading(true);
    const list = Array.from(files);
    try {
      for (let i = 0; i < list.length; i += 1) {
        const file = list[i];
        if (file === undefined) continue;
        try {
          // Polish 1.x: drive `uploadProgress` so the composer
          // tray can render "Uploading <file> (i/n)…" with a
          // stage indicator. The encrypt-vs-upload split lets
          // the user see the slow phase (network) separately
          // from the fast one (in-memory AEAD).
          const ref = await encryptAndUploadFile(
            file,
            undefined,
            (stage) => {
              setUploadProgress({
                fileIndex: i,
                total: list.length,
                fileName: file.name,
                stage,
              });
            },
          );
          setPendingAttachments([...pendingAttachments(), ref]);
        } catch (err) {
          if (err instanceof AttachmentSizeError) {
            setError(
              `"${file.name}" is ${formatAttachmentSize(err.got)} — over the ${formatAttachmentSize(err.max)} limit.`,
            );
          } else {
            setError(`upload of "${file.name}" failed: ${formatError(err)}`);
          }
          // Stop the loop on the first failure — a network
          // outage isn't going to be fixed by trying the next
          // file. The user picks again after surfacing.
          break;
        }
      }
    } finally {
      setUploading(false);
      setUploadProgress(null);
    }
  }

  /**
   * Handle the composer's `<input type="file">` change event.
   * Delegates to [`processFiles`] then resets the input so the
   * same file can be re-picked (browsers suppress the change
   * event when the value is identical).
   */
  async function onFilesPicked(e: Event): Promise<void> {
    const target = e.currentTarget as HTMLInputElement;
    const files = target.files;
    if (files === null) return;
    try {
      await processFiles(files);
    } finally {
      target.value = "";
    }
  }

  // Drag-drop polish (1.x): a drop anywhere on the chat surface
  // queues the dropped files into the pending-attachments list
  // — same flow as clicking the 📎 button. Tracks `isDragOver`
  // so the surface can render a "drop here" hint while a drag
  // hovers; `dragCounter` handles the dragenter/dragleave race
  // (children fire leave events when the drag crosses them).
  const [isDragOver, setIsDragOver] = createSignal(false);
  // eslint-disable-next-line solid/reactivity
  let dragCounter = 0;

  function onDragEnter(e: DragEvent): void {
    if (!hasFilesType(e.dataTransfer)) return;
    e.preventDefault();
    dragCounter += 1;
    setIsDragOver(true);
  }

  function onDragOver(e: DragEvent): void {
    if (!hasFilesType(e.dataTransfer)) return;
    e.preventDefault();
    // `move` shows the cursor that matches "this drop will
    // produce something" instead of the default "no" cursor on
    // top of a non-droppable surface.
    if (e.dataTransfer !== null) {
      e.dataTransfer.dropEffect = "copy";
    }
  }

  function onDragLeave(e: DragEvent): void {
    if (!hasFilesType(e.dataTransfer)) return;
    e.preventDefault();
    dragCounter = Math.max(0, dragCounter - 1);
    if (dragCounter === 0) {
      setIsDragOver(false);
    }
  }

  async function onDrop(e: DragEvent): Promise<void> {
    if (!hasFilesType(e.dataTransfer)) return;
    e.preventDefault();
    dragCounter = 0;
    setIsDragOver(false);
    const files = e.dataTransfer?.files;
    if (files === undefined || files.length === 0) return;
    await processFiles(files);
  }

  function removePendingAttachment(index: number): void {
    setPendingAttachments(
      pendingAttachments().filter((_, i) => i !== index),
    );
  }

  /**
   * Download + decrypt a received attachment + offer a Save
   * dialog. Used by the attachment-chip Save button on incoming
   * bubbles.
   */
  async function onDownloadAttachment(
    ref: AttachmentRefBytes,
  ): Promise<void> {
    setError(null);
    try {
      const bytes = await downloadAndDecryptRef(ref);
      saveDecryptedBytes(
        bytes,
        ref.name ?? `attachment-${blobIdHex(ref.blobId).slice(0, 8)}`,
        ref.mime,
      );
    } catch (err) {
      if (err instanceof AttachmentMissingError) {
        setError(
          "The attachment is no longer available on the server.",
        );
      } else {
        setError(`download failed: ${formatError(err)}`);
      }
    }
  }

  /**
   * Inline-preview an image attachment. Fetches once + caches
   * the resulting object URL in `decryptedCache`; the
   * [`AttachmentChips`] component reads the cache reactively and
   * renders the `<img>` inside the bubble once the URL is
   * available. Re-click is a no-op (the cache already has the
   * entry); the user toggles the inline preview off via the
   * Hide button on the chip.
   *
   * Polish 1.x — inline image previews live in the bubble
   * itself rather than blowing open a new tab, matching iOS /
   * Android attachment UX and Signal / Telegram patterns. The
   * SVG exclusion in [`isInlinePreviewable`] keeps the trust
   * boundary tight: no `<img>` src from an unverified SVG, only
   * raster formats decoded by the browser's image pipeline.
   */
  async function onPreviewAttachment(
    ref: AttachmentRefBytes,
  ): Promise<void> {
    const key = blobIdHex(ref.blobId);
    const cached = decryptedCache().get(key);
    if (cached !== undefined && cached.objectUrl !== null) {
      // Already cached — the bubble already renders inline.
      // Nothing to do; the Hide affordance on the chip clears
      // the URL.
      return;
    }
    setError(null);
    try {
      const bytes = await downloadAndDecryptRef(ref);
      const blob = new Blob([bytes.slice()], {
        type: ref.mime ?? "application/octet-stream",
      });
      const url = URL.createObjectURL(blob);
      const next = new Map(decryptedCache());
      next.set(key, { bytes, objectUrl: url });
      setDecryptedCache(next);
      // No window.open — the JSX picks up the cached URL via
      // the `getCachedUrl` getter passed to AttachmentChips.
    } catch (err) {
      if (err instanceof AttachmentMissingError) {
        setError("The attachment is no longer available on the server.");
      } else {
        setError(`preview failed: ${formatError(err)}`);
      }
    }
  }

  /**
   * Drop a cached inline preview without re-decrypting on the
   * next Open. Revokes the object URL so the browser releases
   * the underlying Blob; the `bytes` ref is kept in the cache
   * for re-download but the URL is cleared so the bubble
   * re-renders without the inline image.
   */
  function onHideAttachmentPreview(ref: AttachmentRefBytes): void {
    const key = blobIdHex(ref.blobId);
    const cached = decryptedCache().get(key);
    if (cached === undefined || cached.objectUrl === null) return;
    URL.revokeObjectURL(cached.objectUrl);
    const next = new Map(decryptedCache());
    next.set(key, { bytes: cached.bytes, objectUrl: null });
    setDecryptedCache(next);
  }

  /** Cache accessor passed to [`AttachmentChips`] — returns the
   *  cached object URL when the attachment has been previewed
   *  in this session, `null` otherwise. The chip reads this on
   *  every render so the inline image appears reactively once
   *  the cache lands. */
  function getCachedAttachmentUrl(ref: AttachmentRefBytes): string | null {
    return decryptedCache().get(blobIdHex(ref.blobId))?.objectUrl ?? null;
  }

  // 1.18g — "Add member" submission handler. Resolves the
  // typed @username via discovery, runs the full MLS add
  // (fetch KeyPackage → group.addMember → publish commit to DS
  // → ship Welcome to stranger inbox), then registers the
  // commit's DS id with `ChatClient.markOwnPublishedCommit` so
  // the very next pollMessages drops our own echo and doesn't
  // hit `processIncoming` on a commit we just applied locally.
  // Final step is a `refresh()` — pulls the (now-suppressed)
  // commit off the wire + re-renders the header (memberCount
  // signal updates from `group.memberCount` post-add).
  async function onAddMember(): Promise<void> {
    const username = addMemberInput().trim();
    if (username.length === 0) return;
    setError(null);
    setNotice(null);
    setBusy(true);
    try {
      const result = await contacts.addMemberToGroupByUsername(
        props.requireIdentity(),
        props.entry.group,
        username,
      );
      // Suppress the echo so the sender's next pollMessages
      // doesn't try to processIncoming on its own commit at
      // an epoch we've already advanced past.
      chat.markOwnPublishedCommit(result.commitMessageId);
      // Mirror the post-add member count into the reactive
      // signal so the header re-renders.
      setMemberCount(props.entry.group.memberCount);
      // 1.18h — bump membersVersion so the members panel
      // (when open) re-reads the updated leaf set.
      setMembersVersion(membersVersion() + 1);
      setNotice(
        `@${result.recipientUsername} added (commit #${result.commitMessageId}, welcome #${result.welcomeMessageId}).`,
      );
      setAddMemberInput("");
      setAddMemberOpen(false);
      // Pull anything that arrived during the add (no-op when
      // there's nothing new but cheap, and keeps the cursor
      // moving past the suppressed commit row).
      void refresh();
    } catch (err) {
      setError(formatError(err));
    } finally {
      setBusy(false);
    }
  }

  // 1.18h — Remove-member handler. Symmetric to onAddMember
  // minus the discovery/Welcome dance: caller picks the leaf
  // index from the members list, we drive the MLS remove,
  // broadcast the commit, suppress the echo. The removed
  // member's leaf is gone from `group.members()` immediately;
  // their own client lands the commit on next poll and learns
  // they're out.
  async function onRemoveMember(leafIndex: number): Promise<void> {
    if (!window.confirm(`Remove this member (leaf #${leafIndex})?`)) {
      return;
    }
    setError(null);
    setNotice(null);
    setBusy(true);
    try {
      const result = await contacts.removeMemberFromGroup(
        props.requireIdentity(),
        props.entry.group,
        leafIndex,
      );
      chat.markOwnPublishedCommit(result.commitMessageId);
      setMemberCount(props.entry.group.memberCount);
      setMembersVersion(membersVersion() + 1);
      setNotice(`Member at leaf #${leafIndex} removed (commit #${result.commitMessageId}).`);
      void refresh();
    } catch (err) {
      setError(formatError(err));
    } finally {
      setBusy(false);
    }
  }

  // ADR-0022 — promote / demote a member as admin (owner only). Publishes the
  // setGroupRoles commit + suppresses the echo, same shape as add / remove.
  async function onToggleAdmin(
    member: { leafIndex: number; sigPubkey: Uint8Array },
    makeAdmin: boolean,
  ): Promise<void> {
    setError(null);
    setNotice(null);
    setBusy(true);
    try {
      const result = await contacts.setMemberAdmin(
        props.requireIdentity(),
        props.entry.group,
        member.sigPubkey,
        makeAdmin,
      );
      chat.markOwnPublishedCommit(result.commitMessageId);
      setMembersVersion(membersVersion() + 1);
      setNotice(
        makeAdmin
          ? `Promoted to admin (commit #${result.commitMessageId}).`
          : `Removed as admin (commit #${result.commitMessageId}).`,
      );
      void refresh();
    } catch (err) {
      setError(formatError(err));
    } finally {
      setBusy(false);
    }
  }

  // Pull the current backlog on mount so a fresh `/chat/:id`
  // visit shows whatever the peer has sent so far.
  //
  // Phase 2a3f1-web — also seed `rows` from the persistent
  // message log BEFORE the first refresh. This way reloads
  // show prior history immediately + the DS poll just
  // catches up any rows that arrived while the tab was
  // closed (the persisted `cursor` lets the next refresh
  // start from the right slot).
  onMount(() => {
    void (async (): Promise<void> => {
      await hydrateFromLog();
      await refresh();
    })();
  });

  // Phase 2a3e2c-web — read cursor sync. Every time the
  // timeline grows past the previously-broadcast value, mint
  // a fresh `ReadCursor` payload pointing at the newest
  // visible message. The sibling-device chat list folds the
  // cursor into its unread-count tracker on its next mount.
  //
  // Debounced to 1500 ms so a noisy poll batch doesn't fan
  // out one DeviceGroup write per row; the user's "thread
  // open + scrolling" intent is captured at the end of the
  // burst.
  let lastBroadcastReadId: bigint | null = null;
  let readCursorTimer: ReturnType<typeof setTimeout> | null = null;
  const READ_CURSOR_DEBOUNCE_MS = 1500;
  createEffect(() => {
    const list = rows();
    if (list.length === 0) return;
    // Find the highest server-assigned messageId — outgoing
    // rows with `ack === "pending"` carry no messageId yet so
    // they're skipped.
    let lastId: bigint | null = null;
    for (const r of list) {
      let id: number | undefined;
      if (r.kind === "outgoing") {
        if (r.ack === "sent" && r.messageId !== undefined) id = r.messageId;
      } else {
        id = r.event.messageId;
      }
      if (id !== undefined) {
        const b = BigInt(id);
        if (lastId === null || b > lastId) lastId = b;
      }
    }
    if (lastId === null) return;
    if (lastBroadcastReadId !== null && lastBroadcastReadId >= lastId) return;
    const target = lastId;
    if (readCursorTimer !== null) clearTimeout(readCursorTimer);
    readCursorTimer = setTimeout(() => {
      readCursorTimer = null;
      lastBroadcastReadId = target;
      const groupId = groupIdBytes();
      // Encode the messageId as 8 big-endian bytes so the
      // wire shape stays stable regardless of how JS strings
      // serialise BigInt. Receivers compare the raw bytes
      // against their own per-group highest-id.
      const idBytes = new Uint8Array(8);
      let v = target;
      for (let i = 7; i >= 0; i -= 1) {
        idBytes[i] = Number(v & 0xffn);
        v >>= 8n;
      }
      void sync
        .setReadCursor(groupId, idBytes, BigInt(Date.now()))
        .catch((err: unknown) => {
          // eslint-disable-next-line no-console
          console.warn("setReadCursor failed", err);
        });
    }, READ_CURSOR_DEBOUNCE_MS);
  });

  onCleanup(() => {
    if (readCursorTimer !== null) {
      clearTimeout(readCursorTimer);
      readCursorTimer = null;
    }
  });

  // Debounced persist whenever the timeline mutates. We
  // re-read `rows()` inside the effect so every transitive
  // signal change schedules a write; the 500 ms debounce
  // collapses bursts (poll batch, send + ack flip) into one
  // write.
  createEffect(() => {
    // touch the signals
    void rows();
    void cursor();
    if (!didHydrateFromLog) return;
    if (pendingPersistTimer !== null) clearTimeout(pendingPersistTimer);
    pendingPersistTimer = setTimeout(() => {
      pendingPersistTimer = null;
      void persistToLogBestEffort();
    }, PERSIST_DEBOUNCE_MS);
  });

  onCleanup(() => {
    if (pendingPersistTimer !== null) {
      clearTimeout(pendingPersistTimer);
      pendingPersistTimer = null;
    }
  });

  async function hydrateFromLog(): Promise<void> {
    try {
      const accountIdB64 = accountIdB64ForLog();
      if (accountIdB64 === null) {
        didHydrateFromLog = true;
        return;
      }
      const deviceKey = await getDeviceKey(accountIdB64);
      if (deviceKey === null) {
        didHydrateFromLog = true;
        return;
      }
      const log = await loadMessageLog(accountIdB64, deviceKey);
      didHydrateFromLog = true;
      if (log === null) return;
      const slot = log.groups[groupIdHexForLog()];
      if (slot === undefined) return;
      const seeded = slot.messages.map(persistedToTimelineRow);
      // Only seed when the in-memory list is empty — a refresh
      // running concurrently with hydration shouldn't get
      // stomped by the disk load.
      if (rows().length === 0) {
        setRows(seeded);
      }
      const cur = Number(slot.cursor);
      if (Number.isFinite(cur) && cur > cursor()) {
        setCursor(cur);
      }
    } catch (err) {
      // Best-effort — corrupt blob or transient IDB failure
      // shouldn't block the chat surface. The DS poll backs
      // up the timeline anyway.
      // eslint-disable-next-line no-console
      console.warn("hydrateFromLog failed", err);
      didHydrateFromLog = true;
    }
  }

  async function persistToLogBestEffort(): Promise<void> {
    try {
      const accountIdB64 = accountIdB64ForLog();
      if (accountIdB64 === null) return;
      const deviceKey = await getDeviceKey(accountIdB64);
      if (deviceKey === null) return;
      // Read current log (other groups may have rows that
      // belong to the same account — we don't want to wipe
      // them when writing back). Per-tab serialisation is the
      // common case (one Chat route mounted at a time), so a
      // read-modify-write race against another tab is rare
      // enough to ignore for MVP.
      const existing =
        (await loadMessageLog(accountIdB64, deviceKey)) ??
        ({ version: 1, groups: {} } as MessageLogPayload);
      const next: MessageLogPayload = {
        version: existing.version,
        groups: {
          ...existing.groups,
          [groupIdHexForLog()]: {
            cursor: String(cursor()),
            messages: rows()
              .map(timelineRowToPersisted)
              .filter((m): m is PersistedMessage => m !== null),
          } satisfies PersistedGroup,
        },
      };
      await saveMessageLog(accountIdB64, deviceKey, next);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("persistToLogBestEffort failed", err);
    }
  }

  return (
    <div
      onDragEnter={onDragEnter}
      onDragOver={onDragOver}
      onDragLeave={onDragLeave}
      onDrop={(e) => {
        void onDrop(e);
      }}
      style={{
        display: "flex",
        "flex-direction": "column",
        gap: "1rem",
        position: "relative",
        // Subtle outline + accent tint while a file drag hovers
        // anywhere over the chat body. The inner content stays
        // interactive; the overlay below the header is purely
        // a visual cue.
        outline: isDragOver()
          ? "2px dashed var(--accent)"
          : "2px dashed transparent",
        "outline-offset": "-4px",
        "border-radius": "var(--radius-md)",
        transition: "outline-color 0.1s",
      }}
    >
      <Show when={isDragOver()}>
        <div
          style={{
            position: "absolute",
            inset: "0",
            background: "rgba(0, 122, 255, 0.08)",
            "border-radius": "var(--radius-md)",
            display: "flex",
            "align-items": "center",
            "justify-content": "center",
            "pointer-events": "none",
            "z-index": 10,
          }}
        >
          <span
            style={{
              padding: "0.5rem 1rem",
              background: "var(--surface-elevated)",
              border: "1px solid var(--accent)",
              "border-radius": "var(--radius-md)",
              "font-weight": "600",
            }}
          >
            Drop files to attach
          </span>
        </div>
      </Show>
      <header
        style={{
          display: "flex",
          "align-items": "center",
          "justify-content": "space-between",
          gap: "0.75rem",
        }}
      >
        <div>
          {/* ADR-0021 follow-up: show the linked contact's user-chosen
              name in the chat header (falls back to the chat label). */}
          <h1 style={{ margin: 0 }}>{resolvePeerName()}</h1>
          <div
            style={{
              color: "var(--surface-muted)",
              "font-size": "0.9rem",
              "margin-top": "0.25rem",
            }}
          >
            {memberCount()} member
            {memberCount() === 1 ? "" : "s"} · seat #
            {props.entry.group.ownLeafIndex}
          </div>
        </div>
        <div style={{ display: "flex", gap: "0.5rem", "flex-wrap": "wrap" }}>
          <button
            type="button"
            onClick={() => {
              setMembersOpen(!membersOpen());
              setNotice(null);
              setError(null);
              // Bump version so the panel reads fresh data
              // when it opens (covers the case where new
              // commits arrived between the last refresh and
              // this click).
              setMembersVersion(membersVersion() + 1);
            }}
            disabled={busy()}
          >
            {membersOpen() ? "Hide members" : "Members"}
          </button>
          {/* ADR-0022 — only owner/admin (or an open/legacy group) sees the
              add affordance; a plain member's add would be refused by peers. */}
          <Show when={canManageMembers()}>
            <button
              type="button"
              onClick={() => {
                setAddMemberOpen(!addMemberOpen());
                setNotice(null);
                setError(null);
              }}
              disabled={busy()}
            >
              {addMemberOpen() ? "Cancel add" : "Add member"}
            </button>
          </Show>
          <button
            type="button"
            onClick={() => {
              void refresh();
            }}
            disabled={busy()}
          >
            {busy() ? "Refreshing…" : "Refresh"}
          </button>
          {/* Phase 3a8 — call button. Visible whenever no
              call is active in this thread. */}
          <Show
            when={
              (callState() === "idle" || callState() === "done") && !sfuActive()
            }
          >
            <button
              type="button"
              onClick={() => {
                // Group chat (>2 members) → SFU + SFrame; 1×1 → P2P.
                if (memberCount() > 2) {
                  void startGroupCall();
                } else {
                  void onCallClicked();
                }
              }}
              disabled={busy()}
            >
              📞 Call
            </button>
          </Show>
          <button type="button" onClick={props.onLeave}>
            Back to Contacts
          </button>
        </div>
      </header>

      {/* G3-3-web-session-bind — full in-call UI presented over the
          chat for live call states (the web analog of iOS/Android
          call-present). `ringing` keeps the lightweight inline banner
          below (Accept / Decline) until the call is accepted. */}
      <Show when={callOverlayVisible()}>
        <Call
          embedded={true}
          onExit={(): void => {
            onCallExit();
          }}
          remoteStreamFor={(participantId): MediaStream | undefined => {
            // Roster id is `peer-<peerId>`; the SFU sets the downstream
            // MSID stream id to that same peer id, so strip the prefix
            // to look the stream up. 1×1 path has no SFU handle → undefined.
            const peerId = participantId.replace(/^peer-/, "");
            return sfuHandle()?.remoteStreams().get(peerId);
          }}
        />
      </Show>

      {/* Hidden audio sink for the remote stream. Rendered for the
          whole call lifetime (ringing → ending) in a stable slot so
          it does not unmount when the inline banner gives way to the
          full overlay — otherwise remote audio would cut out. */}
      <Show when={callState() !== "idle" && callState() !== "done"}>
        <audio
          ref={(el) => setAudioRef(el)}
          autoplay
          style={{ display: "none" }}
        />
      </Show>

      {/* SFU group-call remote audio — one hidden sink per remote
          MediaStream, keyed by the stream object identity (a stable
          stream keeps its element, so audio does not cut on roster
          churn). Remote VIDEO renders per-tile from the store roster:
          the `remoteStreamFor` prop above resolves a tile's stream via
          the SFU MSID-stream-id-to-peer mapping (`peer-<peerId>`). */}
      <Show when={sfuActive()}>
        <For
          each={[
            ...(
              sfuHandle()?.remoteStreams() ?? new Map<string, MediaStream>()
            ).values(),
          ]}
        >
          {(stream) => (
            <audio
              autoplay
              style={{ display: "none" }}
              ref={(el) => {
                el.srcObject = stream;
              }}
            />
          )}
        </For>
      </Show>

      {/* Phase 3a8 — inline incoming-call banner. Now shown ONLY
          while `ringing` (Accept / Decline); live states use the
          full overlay above. */}
      <Show when={callState() === "ringing"}>
        <div
          style={{
            position: "sticky",
            top: "0",
            "z-index": 10,
            margin: "0.5rem 0",
            padding: "0.75rem 1rem",
            background: "var(--accent)",
            color: "var(--accent-fg)",
            "border-radius": "var(--radius-md)",
            display: "flex",
            "align-items": "center",
            "justify-content": "space-between",
            gap: "0.75rem",
            "box-shadow": "0 2px 8px rgba(0, 0, 0, 0.15)",
          }}
        >
          <div>
            <strong>
              {(() => {
                switch (callState()) {
                  case "outgoing":
                    return "📞 Calling…";
                  case "ringing":
                    return "📞 Incoming call";
                  case "connecting":
                    return "📞 Connecting…";
                  case "connected":
                    return "📞 In call";
                  case "ending":
                    return "📞 Ending…";
                  default:
                    return "📞";
                }
              })()}
            </strong>
            <div style={{ "font-size": "0.85rem", "margin-top": "0.2rem" }}>
              Audio · P2P (DTLS-SRTP). SFU + SFrame land in Phase 3b.
            </div>
            {/* G3-3-conn-quality — live RTT / bitrate / path readout
                from the session's getStats poll. Shown only while
                connected (the sample is meaningless before ICE
                settles). */}
            <Show when={callState() === "connected"}>
              <div
                style={{
                  "font-size": "0.8rem",
                  "margin-top": "0.3rem",
                  display: "flex",
                  "align-items": "center",
                  gap: "0.4rem",
                }}
              >
                <span
                  aria-hidden="true"
                  style={{
                    width: "0.55rem",
                    height: "0.55rem",
                    "border-radius": "50%",
                    "background-color": qualityDotColour(
                      qualityTierForRtt(callConnStats()?.rttMs ?? null),
                    ),
                    display: "inline-block",
                    "flex-shrink": 0,
                  }}
                />
                <span>
                  {(() => {
                    const s = callConnStats();
                    const rtt =
                      s?.rttMs != null ? `${s.rttMs} ms` : "measuring…";
                    const path =
                      s?.relayed === true ? "relay" : s != null ? "direct" : "—";
                    return `${rtt} · ${path} · ↓ ${formatBps(
                      s?.downBps ?? null,
                    )} · ↑ ${formatBps(s?.upBps ?? null)}`;
                  })()}
                </span>
              </div>
            </Show>
          </div>
          <div style={{ display: "flex", gap: "0.4rem" }}>
            <Show when={callState() === "ringing"}>
              <button
                type="button"
                onClick={() => {
                  void onAcceptCall();
                }}
                style={{
                  background: "var(--surface)",
                  color: "var(--surface-fg)",
                  border: "none",
                  padding: "0.5rem 0.9rem",
                  "border-radius": "var(--radius-sm)",
                  "font-weight": "600",
                  cursor: "pointer",
                }}
              >
                Accept
              </button>
              <button
                type="button"
                onClick={() => {
                  void onDeclineCall();
                }}
                style={{
                  background: "transparent",
                  color: "var(--accent-fg)",
                  border: "1px solid var(--accent-fg)",
                  padding: "0.5rem 0.9rem",
                  "border-radius": "var(--radius-sm)",
                  cursor: "pointer",
                }}
              >
                Decline
              </button>
            </Show>
            <Show when={callState() !== "ringing"}>
              <button
                type="button"
                onClick={() => {
                  void onHangUp();
                }}
                style={{
                  background: "transparent",
                  color: "var(--accent-fg)",
                  border: "1px solid var(--accent-fg)",
                  padding: "0.5rem 0.9rem",
                  "border-radius": "var(--radius-sm)",
                  cursor: "pointer",
                }}
              >
                Hang up
              </button>
            </Show>
          </div>
        </div>
      </Show>

      <Show when={callState() === "done" && callEndReason() !== null}>
        <div
          style={{
            margin: "0.5rem 0",
            padding: "0.5rem 0.75rem",
            background: "var(--surface-elevated)",
            "border-radius": "var(--radius-sm)",
            "font-size": "0.85rem",
            color: "var(--surface-muted)",
          }}
        >
          Last call ended ({callEndReason()}).
        </div>
      </Show>

      {/* 1.18g — inline "Add member" form. Toggled via the
         button above. Submit drives
         ContactsClient.addMemberToGroupByUsername, which fetches
         the recipient's KeyPackage, runs Group.addMember,
         broadcasts the commit to existing members via DS, and
         ships the Welcome to the new member's stranger inbox. */}
      <Show when={addMemberOpen()}>
        <form
          onSubmit={(e) => {
            e.preventDefault();
            void onAddMember();
          }}
          style={{
            display: "flex",
            gap: "0.5rem",
            padding: "0.75rem",
            border: "1px solid var(--surface-border)",
            "border-radius": "var(--radius-md)",
            background: "var(--surface-elevated)",
          }}
        >
          <input
            type="text"
            placeholder="@username"
            autocomplete="off"
            value={addMemberInput()}
            onInput={(e) => setAddMemberInput(e.currentTarget.value)}
            style={{ flex: 1 }}
            disabled={busy()}
          />
          <button
            class="primary"
            type="submit"
            disabled={busy() || addMemberInput().trim().length === 0}
          >
            {busy() ? "Adding…" : "Add"}
          </button>
        </form>
      </Show>

      {/* 1.18h — members panel. Lists every leaf at the
          current epoch with a fingerprint-short label + Remove
          button. The own-leaf row gets a "(you)" badge instead
          of a Remove button (can't kick yourself). */}
      <Show when={membersOpen()}>
        <section
          aria-label="Members"
          style={{
            padding: "0.5rem 0.75rem",
            border: "1px solid var(--surface-border)",
            "border-radius": "var(--radius-md)",
            background: "var(--surface-elevated)",
          }}
        >
          <h2
            style={{
              "font-size": "0.85rem",
              "text-transform": "uppercase",
              "letter-spacing": "0.05em",
              color: "var(--surface-muted)",
              margin: "0 0 0.5rem 0",
            }}
          >
            Members ({members().length})
          </h2>
          <ul
            style={{
              "list-style": "none",
              padding: 0,
              margin: 0,
              display: "flex",
              "flex-direction": "column",
              gap: "0.25rem",
            }}
          >
            <For each={members()}>
              {(m) => {
                const isSelf = m.leafIndex === props.entry.group.ownLeafIndex;
                // 1.18i — render the accepted-contact's
                // displayName when this leaf's pubkey is in our
                // local cache. Falls back to a hex fingerprint
                // (a recipient we've not formally accepted yet,
                // or somebody else who joined a group we're in).
                // Lookup key matches `local-store.ts`'s
                // `signaturePubkeyB64` shape (standard base64
                // of the 32-byte Ed25519 verifying key).
                const displayName = acceptedByPubkey().get(
                  base64Encode(m.sigPubkey),
                );
                return (
                  <li
                    style={{
                      display: "flex",
                      "align-items": "center",
                      gap: "0.5rem",
                      padding: "0.35rem 0.5rem",
                      "border-radius": "var(--radius-sm)",
                      background: "var(--surface-bg)",
                    }}
                  >
                    <code style={{ "font-size": "0.85rem" }}>
                      #{m.leafIndex} ·{" "}
                      {displayName !== undefined
                        ? displayName
                        : shortFingerprint(m.sigPubkey)}
                    </code>
                    <span style={{ flex: 1 }} />
                    <Show
                      when={!isSelf}
                      fallback={
                        <span
                          style={{
                            "font-size": "0.8rem",
                            color: "var(--surface-muted)",
                          }}
                        >
                          (you)
                        </span>
                      }
                    >
                      {/* ADR-0022 — only the owner promotes / demotes admins;
                          never the owner's own row. */}
                      <Show when={isOwner() && ownerPubkeyHex() !== hexOf(m.sigPubkey)}>
                        <button
                          type="button"
                          onClick={() => {
                            void onToggleAdmin(
                              m,
                              !adminPubkeys().has(hexOf(m.sigPubkey)),
                            );
                          }}
                          disabled={busy()}
                          style={{ "font-size": "0.8rem" }}
                        >
                          {adminPubkeys().has(hexOf(m.sigPubkey))
                            ? "Remove admin"
                            : "Make admin"}
                        </button>
                      </Show>
                      {/* ADR-0022 — only owner/admin (or an open/legacy
                          group) sees Remove; a member's removal is refused. */}
                      <Show when={canManageMembers()}>
                        <button
                          type="button"
                          onClick={() => {
                            void onRemoveMember(m.leafIndex);
                          }}
                          disabled={busy()}
                          style={{
                            "font-size": "0.8rem",
                            color: "var(--error)",
                          }}
                        >
                          Remove
                        </button>
                      </Show>
                    </Show>
                  </li>
                );
              }}
            </For>
          </ul>
        </section>
      </Show>

      {error() !== null && <div class="form-error">{error()}</div>}
      {notice() !== null && <div class="form-notice">{notice()}</div>}

      <Show
        when={rows().length > 0}
        fallback={
          <p
            style={{
              color: "var(--surface-muted)",
              padding: "1rem 0",
            }}
          >
            No messages yet. Send the first one below.
          </p>
        }
      >
        <ol
          style={{
            "list-style": "none",
            padding: 0,
            margin: 0,
            display: "flex",
            "flex-direction": "column",
            gap: "0.4rem",
          }}
        >
          <For each={rows()}>
            {(row) => (
              <TimelineBubble
                row={row}
                onDownload={onDownloadAttachment}
                onPreview={onPreviewAttachment}
                onHidePreview={onHideAttachmentPreview}
                getCachedUrl={getCachedAttachmentUrl}
                ownLeaf={ownLeaf()}
                opState={(id) => msgOps().get(id)}
                editingId={editingId()}
                editText={editText()}
                setEditText={setEditText}
                onStartEdit={(id, current) => {
                  setEditText(current);
                  setEditingId(id);
                  setReactPickerId(null);
                }}
                onCancelEdit={() => setEditingId(null)}
                onSaveEdit={onSaveEdit}
                onDeleteMessage={onDeleteMessage}
                reactPickerId={reactPickerId()}
                onOpenReactPicker={(id) => {
                  setReactPickerId(id);
                  setEditingId(null);
                }}
                onCloseReactPicker={() => setReactPickerId(null)}
                onReact={onReact}
              />
            )}
          </For>
        </ol>
      </Show>

      {/* 1.19c2 — pending-attachments tray. Renders above the
         text composer when ≥1 file is staged; each chip shows
         the filename + size with a × to drop it. The chips
         carry no preview because the local bytes have been
         GC'd after encrypt — re-decrypt would round-trip
         through storage, which is overkill for a composer
         scratch row. */}
      <Show when={pendingAttachments().length > 0 || uploading()}>
        <div
          style={{
            display: "flex",
            "flex-wrap": "wrap",
            gap: "0.4rem",
            padding: "0.5rem",
            border: "1px solid var(--surface-border)",
            "border-radius": "var(--radius-md)",
            background: "var(--surface-elevated)",
          }}
        >
          <For each={pendingAttachments()}>
            {(ref, i) => (
              <span
                style={{
                  display: "inline-flex",
                  "align-items": "center",
                  gap: "0.35rem",
                  padding: "0.2rem 0.5rem",
                  background: "var(--surface-bg)",
                  border: "1px solid var(--surface-border)",
                  "border-radius": "var(--radius-sm)",
                  "font-size": "0.8rem",
                }}
              >
                <span>{ref.name ?? "(unnamed)"}</span>
                <span style={{ color: "var(--surface-muted)" }}>
                  · {formatAttachmentSize(ref.size)}
                </span>
                <button
                  type="button"
                  onClick={() => removePendingAttachment(i())}
                  disabled={busy()}
                  aria-label="Remove attachment"
                  style={{
                    border: "none",
                    background: "transparent",
                    cursor: "pointer",
                    "font-size": "1rem",
                    "line-height": 1,
                    padding: 0,
                  }}
                >
                  ×
                </button>
              </span>
            )}
          </For>
          <Show when={uploading()}>
            <span
              style={{
                color: "var(--surface-muted)",
                "font-size": "0.8rem",
                "align-self": "center",
              }}
            >
              {(() => {
                const p = uploadProgress();
                if (p === null) return "Encrypting + uploading…";
                const verb = p.stage === "encrypting"
                  ? "Encrypting"
                  : "Uploading";
                // 1-based index in the user-facing copy — humans
                // count from 1. Total = 1 collapses the "(1/1)"
                // ornament since it's noise.
                const tail =
                  p.total > 1 ? ` (${p.fileIndex + 1}/${p.total})` : "";
                return `${verb} "${p.fileName}"${tail}…`;
              })()}
            </span>
          </Show>
        </div>
      </Show>

      <form
        onSubmit={onSend}
        style={{
          display: "flex",
          gap: "0.5rem",
          "padding-top": "0.5rem",
          "border-top": "1px solid var(--surface-border)",
        }}
      >
        {/* 1.19c2 — Attach button. The hidden `<input
            type="file" multiple>` is keyed by uploading() so
            the same file can be re-picked after a failed
            upload, and the label drives the click for a tidy
            chrome-less control. */}
        <label
          style={{
            display: "inline-flex",
            "align-items": "center",
            "justify-content": "center",
            padding: "0 0.6rem",
            border: "1px solid var(--surface-border)",
            "border-radius": "var(--radius-md)",
            background: "var(--surface-elevated)",
            cursor: uploading() ? "wait" : "pointer",
            opacity: uploading() ? 0.6 : 1,
          }}
          aria-label="Attach files"
          title={`Attach files (up to ${formatAttachmentSize(MAX_ATTACHMENT_BYTES)} each)`}
        >
          📎
          <input
            type="file"
            multiple
            disabled={uploading()}
            onChange={(e) => {
              void onFilesPicked(e);
            }}
            style={{ display: "none" }}
          />
        </label>
        <input
          type="text"
          placeholder="Message"
          autocomplete="off"
          value={input()}
          onInput={(e) => setInput(e.currentTarget.value)}
          style={{ flex: 1 }}
        />
        <button
          class="primary"
          type="submit"
          disabled={
            uploading() ||
            (input().length === 0 && pendingAttachments().length === 0)
          }
        >
          Send
        </button>
      </form>
    </div>
  );
}

// ---------------------------------------------------------------
// Subcomponents
// ---------------------------------------------------------------

function TimelineBubble(props: {
  row: TimelineRow;
  onDownload: (ref: AttachmentRefBytes) => void | Promise<void>;
  onPreview: (ref: AttachmentRefBytes) => void | Promise<void>;
  onHidePreview: (ref: AttachmentRefBytes) => void;
  getCachedUrl: (ref: AttachmentRefBytes) => string | null;
  /** ADR-0023 — our own MLS leaf (for own-reaction highlight). */
  ownLeaf: number | undefined;
  /** Reactive overlay accessor for a given target messageId. */
  opState: (messageId: number) => MessageOpState | undefined;
  editingId: number | null;
  editText: string;
  setEditText: (v: string) => void;
  onStartEdit: (messageId: number, current: string) => void;
  onCancelEdit: () => void;
  onSaveEdit: (messageId: number) => void | Promise<void>;
  onDeleteMessage: (messageId: number) => void | Promise<void>;
  reactPickerId: number | null;
  onOpenReactPicker: (messageId: number) => void;
  onCloseReactPicker: () => void;
  onReact: (messageId: number, emoji: string) => void | Promise<void>;
}): JSX.Element {
  // System rows + call signaling never carry an op overlay.
  if (props.row.kind === "incoming") {
    const ev = props.row.event;
    if (ev.kind === "system") {
      return (
        <li
          style={{
            "font-size": "0.8rem",
            color: "var(--surface-muted)",
            "font-style": "italic",
            "text-align": "center",
          }}
        >
          · {ev.systemKind} #{ev.messageId} ·
        </li>
      );
    }
    if (ev.kind === "callSignaling") {
      // Phase 3a8 — call signaling rides the same MLS group
      // as chat messages, but it isn't part of the visible
      // timeline. The CallContext upstream already routed
      // it into the state machine; nothing renders here.
      return null;
    }
    if (ev.kind === "op") {
      // ADR-0023 — ops are folded into the target message's
      // overlay by the receive loop, never rendered as a
      // standalone bubble. (Defensive: ops don't enter `rows()`,
      // but keep the timeline render total.)
      return null;
    }
    // Incoming application message — never our own → Edit/Delete
    // are not offered (the author-only gate would reject our op
    // anyway); React is.
    return (
      <ChatBubbleBody
        messageId={ev.messageId}
        isOwn={false}
        ack="sent"
        originalText={ev.text}
        attachments={ev.attachments}
        meta={`peer · #${ev.messageId} · ${ev.createdAt}`}
        tone="incoming"
        {...props}
      />
    );
  }
  // Outgoing — our own message. Edit/Delete offered once acked
  // (a pending bubble has no messageId yet).
  const r = props.row;
  return (
    <ChatBubbleBody
      messageId={r.ack === "sent" ? r.messageId : undefined}
      isOwn={true}
      ack={r.ack}
      originalText={r.plaintext}
      attachments={r.attachments}
      meta={
        r.ack === "pending"
          ? "you · sending…"
          : `you · sent · #${r.messageId ?? "?"} · ${r.createdAt ?? ""}`
      }
      tone="outgoing"
      {...props}
    />
  );
}

/**
 * ADR-0023 — the shared bubble body for both incoming + outgoing
 * messages, carrying the op overlay (edited text / deleted
 * tombstone / reaction chips) and the per-message action menu.
 *
 * Reactivity: `opState(messageId)` is read inside the render so a
 * later op fold (or a local own-op apply) re-renders just this
 * bubble. The action affordances are gated on `messageId !==
 * undefined` (an un-acked outgoing bubble has no DS id to
 * reference yet).
 */
function ChatBubbleBody(props: {
  messageId: number | undefined;
  isOwn: boolean;
  ack: "pending" | "sent";
  originalText: string;
  attachments: AttachmentRefBytes[];
  meta: string;
  tone: "incoming" | "outgoing";
  onDownload: (ref: AttachmentRefBytes) => void | Promise<void>;
  onPreview: (ref: AttachmentRefBytes) => void | Promise<void>;
  onHidePreview: (ref: AttachmentRefBytes) => void;
  getCachedUrl: (ref: AttachmentRefBytes) => string | null;
  ownLeaf: number | undefined;
  opState: (messageId: number) => MessageOpState | undefined;
  editingId: number | null;
  editText: string;
  setEditText: (v: string) => void;
  onStartEdit: (messageId: number, current: string) => void;
  onCancelEdit: () => void;
  onSaveEdit: (messageId: number) => void | Promise<void>;
  onDeleteMessage: (messageId: number) => void | Promise<void>;
  reactPickerId: number | null;
  onOpenReactPicker: (messageId: number) => void;
  onCloseReactPicker: () => void;
  onReact: (messageId: number, emoji: string) => void | Promise<void>;
}): JSX.Element {
  const isIncoming = props.tone === "incoming";
  // The live overlay for this message (reactive). `undefined`
  // until an op lands.
  const state = (): MessageOpState | undefined =>
    props.messageId === undefined ? undefined : props.opState(props.messageId);
  const isDeleted = (): boolean => state()?.deleted === true;
  const editedText = (): string | undefined => state()?.editedText;
  const displayText = (): string => editedText() ?? props.originalText;
  const isEdited = (): boolean => editedText() !== undefined;
  const isEditing = (): boolean =>
    props.messageId !== undefined && props.editingId === props.messageId;
  const pickerOpen = (): boolean =>
    props.messageId !== undefined && props.reactPickerId === props.messageId;
  // Reactions sorted for a stable chip order.
  const reactionEntries = (): Array<{
    emoji: string;
    count: number;
    mine: boolean;
  }> => {
    const m = state()?.reactions;
    if (m === undefined) return [];
    const own = props.ownLeaf ?? OWN_REACTION_SENTINEL;
    return [...m.entries()]
      .map(([emoji, reactors]) => ({
        emoji,
        count: reactors.size,
        mine: reactors.has(own),
      }))
      .filter((e) => e.count > 0)
      .sort((a, b) => a.emoji.localeCompare(b.emoji));
  };

  const bubbleStyle = isIncoming
    ? {
        padding: "0.5rem 0.75rem",
        background: "var(--surface-elevated)",
        border: "1px solid var(--surface-border)",
        "border-radius": "var(--radius-md)",
        "align-self": "flex-start",
        "max-width": "75%",
      }
    : {
        padding: "0.5rem 0.75rem",
        background: "var(--accent)",
        color: "var(--accent-fg)",
        "border-radius": "var(--radius-md)",
        "align-self": "flex-end",
        "max-width": "75%",
        opacity: props.ack === "pending" ? 0.7 : 1,
      };
  const metaColour = isIncoming ? "var(--surface-muted)" : undefined;

  return (
    <li style={{ ...bubbleStyle, position: "relative" }}>
      <Show
        when={!isDeleted()}
        fallback={
          <div
            style={{
              "font-style": "italic",
              opacity: 0.7,
              "white-space": "pre-wrap",
            }}
          >
            message deleted
          </div>
        }
      >
        <Show
          when={isEditing()}
          fallback={
            <>
              <Show when={displayText().length > 0}>
                <div style={{ "white-space": "pre-wrap" }}>
                  {displayText()}
                  <Show when={isEdited()}>
                    <span
                      style={{
                        "font-size": "0.7rem",
                        opacity: 0.6,
                        "margin-left": "0.35rem",
                      }}
                    >
                      (edited)
                    </span>
                  </Show>
                </div>
              </Show>
              <Show when={props.attachments.length > 0}>
                <AttachmentChips
                  attachments={props.attachments}
                  onDownload={props.onDownload}
                  onPreview={props.onPreview}
                  onHidePreview={props.onHidePreview}
                  getCachedUrl={props.getCachedUrl}
                  tone={props.tone}
                />
              </Show>
            </>
          }
        >
          {/* Inline edit composer — pre-filled with the current
              text; Save publishes the edit op, Cancel discards. */}
          <div style={{ display: "flex", "flex-direction": "column", gap: "0.35rem" }}>
            <textarea
              value={props.editText}
              onInput={(e) => props.setEditText(e.currentTarget.value)}
              rows={2}
              style={{
                width: "100%",
                "box-sizing": "border-box",
                "font-family": "inherit",
                "font-size": "inherit",
                padding: "0.3rem 0.4rem",
                "border-radius": "var(--radius-sm)",
                border: "1px solid var(--surface-border)",
                color: "var(--surface-fg)",
                background: "var(--surface-bg)",
                resize: "vertical",
              }}
            />
            <div style={{ display: "flex", gap: "0.4rem" }}>
              <button
                type="button"
                onClick={() => {
                  if (props.messageId !== undefined) {
                    void props.onSaveEdit(props.messageId);
                  }
                }}
                style={bubbleActionBtnStyle}
              >
                Save
              </button>
              <button
                type="button"
                onClick={() => props.onCancelEdit()}
                style={bubbleActionBtnStyle}
              >
                Cancel
              </button>
            </div>
          </div>
        </Show>
      </Show>

      {/* Reaction chips — rendered under the body (incl. on a
          deleted message: a reaction can outlive the text). */}
      <Show when={reactionEntries().length > 0}>
        <div
          style={{
            display: "flex",
            "flex-wrap": "wrap",
            gap: "0.3rem",
            "margin-top": "0.35rem",
          }}
        >
          <For each={reactionEntries()}>
            {(re) => (
              <button
                type="button"
                title={re.mine ? "Remove your reaction" : "React"}
                onClick={() => {
                  if (props.messageId !== undefined) {
                    void props.onReact(props.messageId, re.emoji);
                  }
                }}
                style={{
                  display: "inline-flex",
                  "align-items": "center",
                  gap: "0.2rem",
                  padding: "0.05rem 0.4rem",
                  "font-size": "0.8rem",
                  cursor: "pointer",
                  "border-radius": "999px",
                  border: re.mine
                    ? "1px solid var(--accent)"
                    : "1px solid var(--surface-border)",
                  background: re.mine
                    ? "var(--accent-soft, rgba(99,102,241,0.18))"
                    : isIncoming
                      ? "var(--surface-bg)"
                      : "rgba(255,255,255,0.15)",
                  color: "inherit",
                }}
              >
                <span>{re.emoji}</span>
                <span style={{ "font-size": "0.72rem", opacity: 0.85 }}>
                  {re.count}
                </span>
              </button>
            )}
          </For>
        </div>
      </Show>

      <div
        style={{
          color: metaColour,
          "font-size": "0.75rem",
          "margin-top": "0.2rem",
        }}
      >
        {props.meta}
      </div>

      {/* Per-message action menu — gated on the message having a
          DS messageId (the op references its target by id). The
          hover-revealed strip carries React on any message, plus
          Edit + Delete on our own. Inline-edit mode hides it. */}
      <Show when={props.messageId !== undefined && !isEditing() && !isDeleted()}>
        <div
          class="lakko-msg-actions"
          style={{
            position: "absolute",
            top: "-0.6rem",
            [isIncoming ? "right" : "left"]: "0.4rem",
            display: "flex",
            gap: "0.2rem",
            padding: "0.1rem",
            "border-radius": "var(--radius-sm)",
            background: "var(--surface-bg)",
            border: "1px solid var(--surface-border)",
            "box-shadow": "0 1px 3px rgba(0,0,0,0.18)",
          }}
        >
          <button
            type="button"
            title="React"
            onClick={() =>
              pickerOpen()
                ? props.onCloseReactPicker()
                : props.onOpenReactPicker(props.messageId as number)
            }
            style={bubbleMiniBtnStyle}
          >
            🙂
          </button>
          <Show when={props.isOwn}>
            <button
              type="button"
              title="Edit"
              onClick={() =>
                props.onStartEdit(props.messageId as number, displayText())
              }
              style={bubbleMiniBtnStyle}
            >
              ✎
            </button>
            <button
              type="button"
              title="Delete"
              onClick={() => {
                if (props.messageId !== undefined) {
                  void props.onDeleteMessage(props.messageId);
                }
              }}
              style={bubbleMiniBtnStyle}
            >
              🗑
            </button>
          </Show>
        </div>
      </Show>

      {/* Emoji picker popover. */}
      <Show when={pickerOpen()}>
        <div
          style={{
            position: "absolute",
            top: "1.2rem",
            [isIncoming ? "right" : "left"]: "0.4rem",
            "z-index": 5,
            display: "flex",
            gap: "0.15rem",
            padding: "0.25rem",
            "border-radius": "var(--radius-md)",
            background: "var(--surface-bg)",
            border: "1px solid var(--surface-border)",
            "box-shadow": "0 2px 6px rgba(0,0,0,0.22)",
          }}
        >
          <For each={REACTION_EMOJI}>
            {(emoji) => (
              <button
                type="button"
                onClick={() => {
                  if (props.messageId !== undefined) {
                    void props.onReact(props.messageId, emoji);
                  }
                }}
                style={{
                  border: "none",
                  background: "transparent",
                  "font-size": "1.1rem",
                  cursor: "pointer",
                  padding: "0.1rem 0.2rem",
                  "line-height": 1,
                }}
              >
                {emoji}
              </button>
            )}
          </For>
        </div>
      </Show>
    </li>
  );
}

/** Shared style for the Save / Cancel buttons in the inline
 *  edit composer. */
const bubbleActionBtnStyle = {
  padding: "0.2rem 0.6rem",
  "font-size": "0.78rem",
  cursor: "pointer",
  "border-radius": "var(--radius-sm)",
  border: "1px solid var(--surface-border)",
  background: "var(--surface-bg)",
  color: "var(--surface-fg)",
} as const;

/** Shared style for the tiny hover-strip action buttons. */
const bubbleMiniBtnStyle = {
  border: "none",
  background: "transparent",
  "font-size": "0.85rem",
  cursor: "pointer",
  padding: "0.1rem 0.25rem",
  "line-height": 1,
} as const;

/**
 * Render the attachment-chip strip below a bubble's text body.
 * Each chip shows `name · size` plus action buttons:
 *
 *   * **Open** — inline-preview when the MIME is a known image
 *     format (`isInlinePreviewable`); fetches + decrypts the
 *     bytes lazily, caches the object URL.
 *   * **Save** — download + decrypt + trigger a save dialog.
 *
 * `tone` shifts the chip colour so they stay legible inside
 * both the incoming-grey and outgoing-accent bubble bodies.
 */
function AttachmentChips(props: {
  attachments: AttachmentRefBytes[];
  onDownload: (ref: AttachmentRefBytes) => void | Promise<void>;
  onPreview: (ref: AttachmentRefBytes) => void | Promise<void>;
  onHidePreview: (ref: AttachmentRefBytes) => void;
  /** Returns the cached object URL for an attachment, or `null`
   *  if it hasn't been decrypted in this session. The bubble
   *  renders an inline `<img>` whenever this is non-null. */
  getCachedUrl: (ref: AttachmentRefBytes) => string | null;
  tone: "incoming" | "outgoing";
}): JSX.Element {
  const chipBg = props.tone === "incoming" ? "var(--surface-bg)" : "rgba(255,255,255,0.15)";
  const chipText = props.tone === "incoming" ? undefined : "inherit";
  return (
    <div
      style={{
        display: "flex",
        "flex-direction": "column",
        gap: "0.35rem",
        "margin-top": "0.4rem",
      }}
    >
      <For each={props.attachments}>
        {(ref) => {
          const cachedUrl = (): string | null => props.getCachedUrl(ref);
          const previewable = isInlinePreviewable(ref);
          return (
            <div style={{ display: "flex", "flex-direction": "column", gap: "0.25rem" }}>
              <Show when={previewable && cachedUrl() !== null}>
                {/* Inline image preview — renders the decrypted
                    blob URL directly in the bubble. Polish 1.x:
                    matches the Signal / Telegram pattern of
                    showing the picture next to its filename,
                    rather than blowing open a new tab. */}
                <img
                  src={cachedUrl() ?? ""}
                  alt={ref.name ?? "image attachment"}
                  style={{
                    display: "block",
                    "max-width": "100%",
                    "max-height": "320px",
                    width: "auto",
                    height: "auto",
                    "border-radius": "var(--radius-sm)",
                    "object-fit": "contain",
                  }}
                />
              </Show>
              <span
                style={{
                  display: "inline-flex",
                  "align-items": "center",
                  gap: "0.35rem",
                  padding: "0.25rem 0.5rem",
                  background: chipBg,
                  color: chipText,
                  "border-radius": "var(--radius-sm)",
                  "font-size": "0.8rem",
                  "max-width": "100%",
                }}
              >
                <span
                  style={{
                    "white-space": "nowrap",
                    overflow: "hidden",
                    "text-overflow": "ellipsis",
                    "max-width": "20ch",
                  }}
                  title={ref.name ?? "(unnamed)"}
                >
                  {ref.name ?? "(unnamed)"}
                </span>
                <span style={{ opacity: 0.7 }}>
                  · {formatAttachmentSize(ref.size)}
                </span>
                <Show when={previewable}>
                  <Show
                    when={cachedUrl() !== null}
                    fallback={
                      <button
                        type="button"
                        onClick={() => {
                          void props.onPreview(ref);
                        }}
                        style={{
                          border: "none",
                          background: "transparent",
                          color: "inherit",
                          cursor: "pointer",
                          "text-decoration": "underline",
                          padding: 0,
                          "font-size": "0.8rem",
                        }}
                      >
                        Open
                      </button>
                    }
                  >
                    <button
                      type="button"
                      onClick={() => props.onHidePreview(ref)}
                      style={{
                        border: "none",
                        background: "transparent",
                        color: "inherit",
                        cursor: "pointer",
                        "text-decoration": "underline",
                        padding: 0,
                        "font-size": "0.8rem",
                      }}
                    >
                      Hide
                    </button>
                  </Show>
                </Show>
                <button
                  type="button"
                  onClick={() => {
                    void props.onDownload(ref);
                  }}
                  style={{
                    border: "none",
                    background: "transparent",
                    color: "inherit",
                    cursor: "pointer",
                    "text-decoration": "underline",
                    padding: 0,
                    "font-size": "0.8rem",
                  }}
                >
                  Save
                </button>
              </span>
            </div>
          );
        }}
      </For>
    </div>
  );
}

function GroupNotFound(): JSX.Element {
  return (
    <section style={panelStyle()}>
      <h1 style={{ "margin-top": 0 }}>Group not found</h1>
      <p>
        Either the URL is wrong, or this group's MLS state was dropped
        on the most recent page reload — Phase-1 doesn't persist
        in-memory MLS groups (tracked as ROADMAP 1.16e6).
      </p>
      <p>
        <A href={paths.contacts}>Back to Contacts</A>
      </p>
    </section>
  );
}

function NeedsReauth(): JSX.Element {
  return (
    <section style={panelStyle()}>
      <h1 style={{ "margin-top": 0 }}>Re-enter your password</h1>
      <p>
        This page reload restored your session from the local device
        key, but the secrets needed to decrypt and sign messages
        aren't in memory. Please re-enter your password.
      </p>
      <p>
        <A href={paths.login}>Re-enter password</A>
      </p>
    </section>
  );
}

// ---------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------

function panelStyle(): JSX.CSSProperties {
  return {
    background: "var(--surface-elevated)",
    border: "1px solid var(--surface-border)",
    "border-radius": "var(--radius-md)",
    padding: "1rem",
  };
}

/** 12-hex-char prefix of an Ed25519 verifying key — enough to
 *  visually tell members apart in the leaf list without crowding
 *  the row. Mirror of `Invite.tsx`'s helper; if a third call
 *  site appears, fold both into `contacts/local-store.ts`'s
 *  existing `fingerprintShort` (which works in base64 — JS
 *  string typing wants the hex variant here). */
function shortFingerprint(pubkey: Uint8Array): string {
  let hex = "";
  for (let i = 0; i < pubkey.length && hex.length < 12; i++) {
    hex += (pubkey[i] ?? 0).toString(16).padStart(2, "0");
  }
  return hex.slice(0, 12);
}

function formatError(err: unknown): string {
  if (err instanceof Error) return err.message;
  return String(err);
}

/** Lowercase hex of a small byte array, for short
 *  filename-fallback when an attachment ref carries no name. */
function blobIdHex(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) {
    out += b.toString(16).padStart(2, "0");
  }
  return out;
}

/** `true` when the drag event carries an actual file payload
 *  (not just selected text or a link) — the
 *  [`DataTransfer.types`] list contains `"Files"` for OS-side
 *  file drags from Finder / Explorer / a download manager.
 *
 *  Guard exists so a stray text-selection drag doesn't trigger
 *  the drop-zone overlay; the file-only filter matches how the
 *  attach-button path treats the user input (a `<input
 *  type="file">` only fires for actual files). */
function hasFilesType(dt: DataTransfer | null): boolean {
  if (dt === null) return false;
  // `DataTransfer.types` is a DOMStringList — `includes` is
  // available on modern browsers; the `Array.from` fallback
  // keeps the check working for older list-only implementations.
  const types = Array.from(dt.types);
  return types.includes("Files");
}

// ---------------------------------------------------------------
// Phase 2a3f1-web — TimelineRow ↔ PersistedMessage conversion
// ---------------------------------------------------------------

/** Convert a runtime [`TimelineRow`] into the on-disk
 *  [`PersistedMessage`] shape. Returns `null` for outgoing
 *  rows that haven't been acked yet — those carry no
 *  server-assigned `messageId` and would collide on reload
 *  with whatever the DS eventually hands back. The next ack
 *  flips the row to `kind: "outgoing"` with a populated
 *  `messageId` and the next persist sweep includes it. */
function timelineRowToPersisted(row: TimelineRow): PersistedMessage | null {
  // Phase 3a8 — call signaling events never persist into
  // the chat log; they're an out-of-band side channel that
  // the CallContext consumes.
  if (row.kind === "incoming" && row.event.kind === "callSignaling") {
    return null;
  }
  if (row.kind === "outgoing") {
    if (
      row.ack !== "sent" ||
      row.messageId === undefined ||
      row.createdAt === undefined
    ) {
      return null;
    }
    return {
      messageId: String(row.messageId),
      direction: "outgoing",
      text: row.plaintext,
      attachments: row.attachments.map(persistedFromAttachmentRef),
      createdAt: row.createdAt,
    };
  }
  // Incoming: split application vs system.
  if (row.event.kind === "application") {
    return {
      messageId: String(row.event.messageId),
      direction: "incoming",
      text: row.event.text,
      attachments: row.event.attachments.map(persistedFromAttachmentRef),
      createdAt: row.event.createdAt,
    };
  }
  if (row.event.kind === "callSignaling") {
    // Already filtered out at the top of this function.
    return null;
  }
  if (row.event.kind === "op") {
    // ADR-0023 — op events are folded into the target message's
    // overlay by the receive loop, never materialised as timeline
    // rows, so this branch is unreachable. Never persist them.
    return null;
  }
  // system
  return {
    messageId: String(row.event.messageId),
    direction: "system",
    text: `[mls ${row.event.systemKind}]`,
    attachments: [],
    createdAt: row.event.createdAt,
  };
}

function persistedFromAttachmentRef(a: AttachmentRefBytes): PersistedAttachment {
  return {
    blobIdB64: b64UrlEncode(a.blobId),
    keyB64: b64UrlEncode(a.key),
    nonceB64: b64UrlEncode(a.nonce),
    mime: a.mime ?? null,
    sizeStr: String(a.size),
    name: a.name ?? null,
  };
}

/** Inverse — reconstruct a [`TimelineRow`] from the on-disk
 *  shape. Outgoing rows come back as acked
 *  (`ack: "sent"`, no `tempId`); the UI's `<For>` key uses
 *  `messageId` for outgoing+sent so no key conflict with the
 *  fresh-send tempIds.
 *
 *  Caveat: `system` rows survive the round trip with their
 *  text already baked in; the systemKind discriminator isn't
 *  recoverable but the rendered string is the same. */
function persistedToTimelineRow(m: PersistedMessage): TimelineRow {
  const attachments = m.attachments.map(attachmentRefFromPersisted);
  if (m.direction === "outgoing") {
    return {
      kind: "outgoing",
      tempId: -1,
      plaintext: m.text,
      attachments,
      ack: "sent",
      messageId: Number(m.messageId),
      createdAt: m.createdAt,
    };
  }
  if (m.direction === "incoming") {
    return {
      kind: "incoming",
      event: {
        kind: "application",
        messageId: Number(m.messageId),
        text: m.text,
        plaintext: m.text,
        attachments,
        // ADR-0023 — the author leaf is not persisted (the
        // op overlay is session-scoped, multi-device replay
        // deferred to Phase 2), so a reloaded message's
        // edit/delete gate fails closed. Acceptable: a peer
        // editing across the reload boundary just won't apply
        // until the next live message re-establishes authorship.
        senderLeaf: undefined,
        createdAt: m.createdAt,
      },
    };
  }
  // system
  return {
    kind: "incoming",
    event: {
      kind: "system",
      messageId: Number(m.messageId),
      systemKind: "commit",
      createdAt: m.createdAt,
    },
  };
}

function attachmentRefFromPersisted(a: PersistedAttachment): AttachmentRefBytes {
  return {
    blobId: b64UrlDecode(a.blobIdB64),
    key: b64UrlDecode(a.keyB64),
    nonce: b64UrlDecode(a.nonceB64),
    mime: a.mime ?? undefined,
    size: Number(a.sizeStr),
    name: a.name ?? undefined,
  };
}

function b64UrlEncode(bytes: Uint8Array): string {
  let binary = "";
  for (let i = 0; i < bytes.length; i += 1) {
    binary += String.fromCharCode(bytes[i] as number);
  }
  return btoa(binary)
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=+$/, "");
}

function uint8ArrayEquals(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i += 1) {
    if (a[i] !== b[i]) return false;
  }
  return true;
}

function b64UrlDecode(s: string): Uint8Array {
  const padded = s + "===".slice((s.length + 3) % 4);
  const std = padded.replace(/-/g, "+").replace(/_/g, "/");
  const binary = atob(std);
  const out = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) {
    out[i] = binary.charCodeAt(i);
  }
  return out;
}

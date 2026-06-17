// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Contacts route — search by `@username`, send a
 * ContactRequest, poll the inbox, accept/decline incoming
 * requests, list accepted contacts.
 *
 * Three sections, top-down:
 *
 *   1. **Search & send** — type a handle, optional intro,
 *      submit. Drives `ContactsClient.sendContactRequest`.
 *   2. **Pending** — lives in component state only (no
 *      persistence; intro plaintext stays out of localStorage).
 *      Refreshed on mount + on the explicit "Refresh"
 *      button. Each row has Accept / Decline buttons.
 *   3. **Accepted** — local address book (public-key shaped),
 *      persisted in `contacts/local-store.ts`.
 *
 * Identity availability: every flow that needs the user's
 * Identity reads `useAuth().identitySecrets()`. When that's
 * `null` the UI shows a "please re-enter your password" panel
 * up front rather than letting the user click into a flow that
 * will fail at the WASM boundary.
 */
import { createMemo, createSignal, For, onCleanup, onMount, Show, type JSX } from "solid-js";
import { A } from "@solidjs/router";
import { useAuth } from "../auth/AuthContext";
import {
  base64Decode,
  base64Encode,
  base64UrlDecodeNoPad,
  base64UrlEncodeNoPad,
} from "../auth/account-id";
import {
  ContactsClient,
  ContactsError,
  SupersessionRefused,
} from "../contacts/client";
import type { IncomingContactRequest, SendResult } from "../contacts/client";
import { supersessionRefusedMessage } from "../contacts/supersession-message";
import { useChats, type ActiveGroup } from "../chat/ChatContext";
import { useDeviceGroupSync } from "../devices/sync/DeviceGroupSyncContext";
import { groupIdHex as syncGroupIdHex } from "../devices/sync/state";
import { loadMessageLog } from "../chat/message-log-store";
import { subscribeMessageLogChanged } from "../chat/message-log-events";
import { getDeviceKey } from "../auth/key-store";
import { loadWasm } from "../wasm";
import { paths } from "./paths";
import {
  composeInlineInviteUrl,
  composePointerInviteUrl,
  expiryPresetToMs,
  INVITE_LABEL_MAX_LEN,
  maxUsesPresetToValue,
  type ExpiryPreset,
  type MaxUsesPreset,
} from "../contacts/invite";
import { DiscoveryClient, type ActiveInvite } from "../discovery/client";
import {
  fingerprintShort,
  loadAccepted,
  loadCursor,
  peerContactByMemberPubkeys,
  removeAccepted,
  saveCursor,
  upsertAccepted,
  type AcceptedContact,
} from "../contacts/local-store";
import { computeSafetyNumber } from "../contacts/safety";

export default function Contacts(): JSX.Element {
  const auth = useAuth();
  const chats = useChats();
  // 1.16e6c2: hand the chat-context wires (canonical Identity +
  // post-mutation snapshot persist) to ContactsClient so every
  // MLS state mutation in `startGroupWith` / `joinGroupFromWelcome`
  // flushes to IDB before the call returns. The closures capture
  // `chats` + `auth` lazily, so they always see the current
  // session.
  const client = new ContactsClient(undefined, undefined, {
    identityProvider: () => chats.identity(),
    onPersist: async () => {
      const s = auth.state();
      if (s.kind !== "session") return;
      await chats.persistSnapshot(s.account.accountId);
    },
  });

  // Memoised so consumers re-run when the AuthContext signal
  // changes (e.g. after a fresh login on the same page session).
  const identity = createMemo(() => auth.identitySecrets());
  const accountUsername = createMemo(() => {
    const s = auth.state();
    return s.kind === "session" ? s.account.username : null;
  });

  return (
    <article>
      <h1>Contacts</h1>
      <Show
        when={identity() !== null}
        fallback={<NeedsReauth />}
      >
        <ConnectedContacts
          client={client}
          username={accountUsername() ?? ""}
        />
      </Show>
    </article>
  );
}

// ----------------------------------------------------------------------
// "Connected" panel — identity secrets are available
// ----------------------------------------------------------------------

interface ConnectedContactsProps {
  readonly client: ContactsClient;
  readonly username: string;
}

function ConnectedContacts(props: ConnectedContactsProps): JSX.Element {
  const auth = useAuth();
  const chats = useChats();
  // The Show wrapper above guarantees identitySecrets() is
  // non-null on every read inside this subtree, but TS doesn't
  // narrow across component boundaries — a tiny accessor that
  // throws if it does see null keeps the inner code clean.
  const requireIdentity = (): NonNullable<
    ReturnType<typeof auth.identitySecrets>
  > => {
    const s = auth.identitySecrets();
    if (s === null) {
      throw new Error("identity secrets unavailable");
    }
    return s;
  };

  // ---- send -----------------------------------------------------
  const [searchInput, setSearchInput] = createSignal("");
  const [introInput, setIntroInput] = createSignal("");
  const [sendBusy, setSendBusy] = createSignal(false);
  const [sendError, setSendError] = createSignal<string | null>(null);
  const [sendNotice, setSendNotice] = createSignal<string | null>(null);

  async function onSend(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    setSendError(null);
    setSendNotice(null);
    setSendBusy(true);
    try {
      const introBytes = new TextEncoder().encode(introInput());
      const result: SendResult = await props.client.sendContactRequest(
        requireIdentity(),
        searchInput(),
        introBytes,
      );
      setSendNotice(
        `Request sent to @${result.recipientUsername} ` +
          `(DS id ${result.messageId}).`,
      );
      setSearchInput("");
      setIntroInput("");
    } catch (err) {
      setSendError(formatError(err, atLabel(searchInput())));
    } finally {
      setSendBusy(false);
    }
  }

  // ---- start MLS group (1.16e3) ---------------------------------
  // Sender-driven flow: the user types a handle, we resolve it,
  // fetch the recipient's KeyPackage, spin up a Group, and ship
  // the Welcome to the recipient's mailbox. The natural
  // Accept-driven path is blocked by a missing wire-format /
  // reverse-lookup primitive (1.16e3b).
  async function onStartGroup(): Promise<void> {
    setSendError(null);
    setSendNotice(null);
    setSendBusy(true);
    try {
      const result = await props.client.startGroupWithUsername(
        requireIdentity(),
        searchInput(),
      );
      chats.add({
        label: `Group with @${result.recipientUsername}`,
        group: result.group,
      });
      setSendNotice(
        `Group started with @${result.recipientUsername} ` +
          `(Welcome shipped, DS id ${result.welcomeMessageId}).`,
      );
      setSearchInput("");
      setIntroInput("");
    } catch (err) {
      setSendError(formatError(err, atLabel(searchInput())));
    } finally {
      setSendBusy(false);
    }
  }

  // ---- pending --------------------------------------------------
  const [pending, setPending] = createSignal<IncomingContactRequest[]>([]);
  const [cursor, setCursor] = createSignal<number>(loadCursor());
  const [pollBusy, setPollBusy] = createSignal(false);
  const [pollError, setPollError] = createSignal<string | null>(null);

  async function refresh(): Promise<void> {
    setPollError(null);
    setPollBusy(true);
    try {
      const result = await props.client.pollMailbox(
        requireIdentity(),
        cursor(),
      );
      // Dispatch on event kind. ContactRequests pile up in the
      // pending list (de-duped by messageId); Welcomes turn
      // straight into active groups via joinGroupFromWelcome.
      const seenContactRequests = new Set(
        pending().map((r) => r.messageId),
      );
      const freshRequests: IncomingContactRequest[] = [];
      for (const ev of result.events) {
        if (ev.kind === "contact-request") {
          if (!seenContactRequests.has(ev.request.messageId)) {
            freshRequests.push(ev.request);
          }
        } else if (ev.kind === "welcome") {
          // welcome — try to join. On success append to active
          // groups; on failure log and skip.
          try {
            const group = await props.client.joinGroupFromWelcome(
              requireIdentity(),
              ev.welcome.welcomeBytes,
            );
            chats.add({
              label: `Joined group (Welcome ${ev.welcome.messageId})`,
              group,
            });
          } catch (err) {
            // eslint-disable-next-line no-console
            console.warn("joinGroupFromWelcome failed", err);
          }
        } else {
          // 1.9b1: contact-ack — the original recipient of our
          // ContactRequest pressed Accept. Upsert their pubkeys
          // into the accepted list so the local address book
          // flips from "pending" to "accepted" and we have the
          // accepter's X25519 pubkey handy for future seals.
          // Cross-platform parity (Apple-Watch-style) note: the
          // pending-sent table that pairs ack ↔ original request
          // by `ackToPseudonym` lives on this client; Phase-1
          // collapses that into a best-effort upsert (the
          // signature is verified, so even a "stale" ack is a
          // legitimate accept by the holder of `accepterPubkey`).
          const accepterContact: AcceptedContact = {
            signaturePubkeyB64: base64Encode(ev.ack.accepterPubkey),
            x25519PubkeyB64: base64Encode(ev.ack.accepterX25519Pubkey),
            displayName: `@${fingerprintShort(ev.ack.accepterPubkey)}`,
            acceptedAtMs: Date.now(),
            // ADR-0021 — persist the peer's account_id (hex) so the SFU
            // group-call roster can resolve to this contact's name.
            accountIdHex: syncGroupIdHex(ev.ack.accepterAccountId),
          };
          setAccepted(upsertAccepted(accepterContact));
        }
      }
      if (freshRequests.length > 0) {
        setPending([...pending(), ...freshRequests]);
      }
      setCursor(result.cursor);
      saveCursor(result.cursor);
    } catch (err) {
      setPollError(formatError(err));
    } finally {
      setPollBusy(false);
    }
  }

  // ---- active MLS groups ---------------------------------------
  // Lifted into the global `<ChatProvider>` (1.16e5) so the
  // `/chat/:id` route can resolve the URL slug to a live
  // group. In-memory only — Phase-1 doesn't persist MLS state.

  async function onAccept(req: IncomingContactRequest): Promise<void> {
    // Always upsert into the accepted list first — the user has
    // explicitly trusted this peer; the group-creation step
    // below is best-effort and a network blip shouldn't undo
    // the acceptance.
    const contact: AcceptedContact = {
      signaturePubkeyB64: base64Encode(req.fromPubkey),
      displayName: `@${fingerprintShort(req.fromPubkey)}`,
      acceptedAtMs: Date.now(),
      // ADR-0021 — persist the peer's account_id (hex) when the request
      // carried it, so the SFU group-call roster can resolve names.
      ...(req.fromAccountId
        ? { accountIdHex: syncGroupIdHex(req.fromAccountId) }
        : {}),
    };
    setAccepted(upsertAccepted(contact));
    setPending(pending().filter((r) => r.messageId !== req.messageId));

    // 1.9b1: ship a ContactAck back to the original sender's
    // mailbox so their UI can flip from "pending" to
    // "accepted" without polling the (post-startGroupWith)
    // Welcome arrival. Best-effort — a failure here doesn't
    // unwind the local accept (the user already trusted the
    // peer); we log and move on. The signed ack is independent
    // of MLS state, so the sender can light up their address
    // book even if our group-creation step below stalls.
    try {
      await props.client.sendContactAck(requireIdentity(), req);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("sendContactAck failed", err);
    }

    // 1.16e3b: Accept-driven group creation. Requires v2 wire
    // format — surface a clean error for legacy senders.
    if (req.fromAccountId === null) {
      setPollError(
        "this is a legacy v1 contact request — group creation " +
          "needs the sender to upgrade to a 1.16e3b+ client",
      );
      return;
    }
    try {
      const result = await props.client.startGroupWith(
        requireIdentity(),
        req.fromAccountId,
        req.fromPubkey,
      );
      const active = chats.add({
        label: `Group with @${fingerprintShort(req.fromPubkey)}`,
        group: result.group,
      });
      // ADR-0021 follow-up: link the just-created 1×1 chat back to this
      // contact (by its base64 group id) so the in-call roster + chat
      // label resolve to the user-chosen displayName. Re-upsert by the
      // same signature pubkey, preserving the fields set above.
      setAccepted(
        upsertAccepted({ ...contact, groupIdB64: active.groupIdB64 }),
      );
    } catch (err) {
      // Don't unwind the acceptance; just surface the error. The peer
      // here is identified by their key fingerprint (no @handle on the
      // Accept path) — pass it as the label for any reset-aware copy.
      setPollError(formatError(err, `@${fingerprintShort(req.fromPubkey)}`));
    }
  }

  function onDecline(req: IncomingContactRequest): void {
    setPending(pending().filter((r) => r.messageId !== req.messageId));
  }

  // ---- accepted -------------------------------------------------
  const [accepted, setAccepted] = createSignal<AcceptedContact[]>(
    loadAccepted(),
  );
  function onRemoveAccepted(contact: AcceptedContact): void {
    setAccepted(removeAccepted(contact.signaturePubkeyB64));
  }
  /** ADR-0021 follow-up: set the contact's user-chosen display name. A
   *  blank value resets it to the `@<fingerprint>` accept-time default.
   *  Re-upserts by the same signature pubkey, so the link + account_id
   *  + group id are preserved. */
  function onRenameAccepted(contact: AcceptedContact, newName: string): void {
    const trimmed = newName.trim();
    const displayName =
      trimmed.length > 0
        ? trimmed
        : `@${fingerprintShort(base64Decode(contact.signaturePubkeyB64))}`;
    setAccepted(upsertAccepted({ ...contact, displayName }));
  }

  return (
    <>
      <p style={{ color: "var(--surface-muted)", "margin-top": 0 }}>
        Signed in as <strong>@{props.username}</strong>. Requests
        you send and receive are end-to-end encrypted; the
        server only sees ciphertext + routing pseudonyms.
      </p>

      <SendPanel
        searchInput={searchInput}
        setSearchInput={setSearchInput}
        introInput={introInput}
        setIntroInput={setIntroInput}
        busy={sendBusy}
        error={sendError}
        notice={sendNotice}
        onSubmit={onSend}
        onStartGroup={() => {
          void onStartGroup();
        }}
      />

      <InvitePanel />

      <ActiveInvitesPanel />

      <PendingPanel
        pending={pending}
        busy={pollBusy}
        error={pollError}
        onRefresh={() => {
          void refresh();
        }}
        onAccept={onAccept}
        onDecline={onDecline}
      />

      <ActiveGroupsPanel groups={chats.groups} accepted={accepted} />

      <AcceptedPanel
        accepted={accepted}
        myPubkey={() => {
          const id = chats.identity();
          return id?.signaturePublicKey ?? null;
        }}
        onRemove={onRemoveAccepted}
        onRename={onRenameAccepted}
      />
    </>
  );
}

// ----------------------------------------------------------------------
// Sub-panels
// ----------------------------------------------------------------------

interface SendPanelProps {
  readonly searchInput: () => string;
  readonly setSearchInput: (v: string) => void;
  readonly introInput: () => string;
  readonly setIntroInput: (v: string) => void;
  readonly busy: () => boolean;
  readonly error: () => string | null;
  readonly notice: () => string | null;
  readonly onSubmit: (e: SubmitEvent) => void;
  readonly onStartGroup: () => void;
}

function SendPanel(props: SendPanelProps): JSX.Element {
  return (
    <section style={panelStyle()}>
      <h2 style={{ "margin-top": 0 }}>Find someone</h2>
      <form onSubmit={props.onSubmit}>
        <label>
          Username
          <input
            type="text"
            placeholder="@alice"
            autocomplete="off"
            spellcheck={false}
            required
            value={props.searchInput()}
            onInput={(e) => props.setSearchInput(e.currentTarget.value)}
            disabled={props.busy()}
          />
        </label>
        <label>
          Intro message (optional)
          <textarea
            rows={2}
            placeholder="Hi, this is alice from the conference."
            value={props.introInput()}
            onInput={(e) => props.setIntroInput(e.currentTarget.value)}
            disabled={props.busy()}
          />
        </label>
        {props.error() !== null && (
          <div class="form-error">{props.error()}</div>
        )}
        {props.notice() !== null && (
          <div style={{ color: "var(--accent)", "font-size": "0.9rem" }}>
            {props.notice()}
          </div>
        )}
        <div style={{ display: "flex", gap: "0.5rem", "flex-wrap": "wrap" }}>
          <button class="primary" type="submit" disabled={props.busy()}>
            {props.busy() ? "Sending…" : "Send contact request"}
          </button>
          <button
            type="button"
            disabled={props.busy()}
            onClick={props.onStartGroup}
            title="Fetch the recipient's KeyPackage, build a 1×1 MLS group, ship the Welcome over DS."
          >
            Start MLS group
          </button>
        </div>
      </form>
    </section>
  );
}

function InvitePanel(): JSX.Element {
  // Form drives `wasm.InviteCapability.compose(...)` and produces a
  // `https://<host>/i/<slug>` URL. The default path (1.18d1b) uploads
  // the composed capability to the discovery service and emits a short
  // pointer URL; it falls back to the self-contained inline-bytes form
  // (1.18c) when the service is unreachable.
  //
  // We pull the canonical Identity from <ChatProvider> rather
  // than re-deriving from `auth.identitySecrets` so the
  // Ed25519 seed + X25519 pubkey we stamp into the capability
  // are byte-identical to what every other contact-flow uses
  // on this device (1.16e6c2 — identity coherence). The
  // account_id still lives in IdentitySecrets — the canonical
  // Identity doesn't carry it on the wasm-bindgen surface.
  const chats = useChats();
  const auth = useAuth();
  // Phase 1.18d1b — uploads compose'd capabilities to
  // /discovery/invite and gets a content-addressed blob_id back
  // for the short pointer URL. Falls back to the inline-bytes
  // form when the discovery service is unreachable (e.g. local
  // dev without the service running, or a censored region path
  // where /discovery/* is blocked but the link is still
  // shareable).
  // Phase 1.5b: thread the bearer token through. `setSessionToken`
  // accepts `null` so an Anon-state render (Show guard above
  // wouldn't get us here but be defensive) just emits 401s on
  // the protected paths.
  const discovery = new DiscoveryClient();
  discovery.setSessionToken(auth.identitySecrets()?.sessionToken ?? null);

  const [label, setLabel] = createSignal("");
  const [expiry, setExpiry] = createSignal<ExpiryPreset>("7d");
  const [maxUses, setMaxUses] = createSignal<MaxUsesPreset>("single");
  const [bootstrapReferral, setBootstrapReferral] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [generatedUrl, setGeneratedUrl] = createSignal<string | null>(null);
  const [generatedKind, setGeneratedKind] = createSignal<
    "pointer" | "inline-fallback" | null
  >(null);
  // 1.18d2b-web — track the blob_id of the just-uploaded
  // capability so the same-session "Revoke this invite" button
  // can post the signed DELETE without forcing the user to
  // paste the URL back. `null` on the inline-fallback path
  // (no server row) and after `reset()`.
  const [generatedBlobId, setGeneratedBlobId] = createSignal<Uint8Array | null>(
    null,
  );
  const [copied, setCopied] = createSignal(false);

  async function onGenerate(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    setError(null);
    setGeneratedUrl(null);
    setGeneratedKind(null);
    setCopied(false);
    setBusy(true);
    try {
      const id = chats.identity();
      if (id === null) {
        // Same gate as the rest of the contacts surface — the
        // outer Show keeps us from rendering when
        // identitySecrets is null, but defensive belt
        // and suspenders.
        throw new Error("re-enter your password to generate invites");
      }
      // wasm-bindgen surface uses bigint for u64; preset
      // helpers return that shape directly.
      const expiryMs = expiryPresetToMs(expiry(), Date.now());
      const maxUsesValue = maxUsesPresetToValue(maxUses());
      const wasm = await loadWasm();
      // Owner account_id sits in IdentitySecrets — the canonical
      // Identity doesn't carry it on the JS surface. We read
      // both fields lazily inside the handler so a session
      // reset between mount and click surfaces as the same
      // re-auth prompt the rest of the contacts surface uses.
      const secrets = auth.identitySecrets();
      if (secrets === null) {
        throw new Error("re-enter your password to generate invites");
      }
      const bytes = wasm.InviteCapability.compose(
        id.ed25519Seed,
        secrets.accountId,
        id.x25519PublicKey,
        expiryMs,
        maxUsesValue,
        bootstrapReferral(),
        label(),
      );

      // 1.18d1b — try the pointer flow first (short URL,
      // server-side existence + future max_uses enforcement).
      // Fall back to the inline-bytes form on transport
      // failure so a censored / temporarily-down discovery
      // service doesn't take the feature offline. We do NOT
      // fall back on PoW / decode_and_verify failures — those
      // mean the server actively rejected the bytes, and
      // smuggling the same bytes inline would still be a bug
      // (the joiner would re-verify locally and reject). We
      // distinguish via DiscoveryNetworkError shape; everything
      // else surfaces as a real error.
      try {
        const blobId = await discovery.uploadInvite(bytes);
        setGeneratedUrl(composePointerInviteUrl(blobId));
        setGeneratedKind("pointer");
        setGeneratedBlobId(blobId);
      } catch (uploadErr) {
        const isNetwork =
          uploadErr instanceof Error && uploadErr.name === "DiscoveryNetworkError";
        if (!isNetwork) throw uploadErr;
        // eslint-disable-next-line no-console
        console.warn(
          "invite upload network-failed; falling back to inline-bytes URL",
          uploadErr,
        );
        setGeneratedUrl(composeInlineInviteUrl(bytes));
        setGeneratedKind("inline-fallback");
        setGeneratedBlobId(null);
      }
    } catch (err) {
      setError(formatError(err));
    } finally {
      setBusy(false);
    }
  }

  async function onCopy(): Promise<void> {
    const url = generatedUrl();
    if (url === null) return;
    try {
      await navigator.clipboard.writeText(url);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 2000);
    } catch (err) {
      setError(formatError(err));
    }
  }

  function reset(): void {
    setGeneratedUrl(null);
    setGeneratedKind(null);
    setGeneratedBlobId(null);
    setError(null);
    setCopied(false);
    setLabel("");
    setBootstrapReferral(false);
  }

  // 1.18d2b-web: same-session revocation. Calls
  // `discovery.revokeInvite(blobId, ed25519Seed)`; on success
  // wipes the URL panel and surfaces a notice via the existing
  // `error` channel (re-purposed as a generic message slot —
  // the form is in a terminal "URL was here" state at this
  // point and a real banner UI is overkill).
  async function onRevoke(): Promise<void> {
    const blobId = generatedBlobId();
    if (blobId === null) return;
    setError(null);
    setBusy(true);
    try {
      const id = chats.identity();
      if (id === null) {
        throw new Error("re-enter your password to revoke invites");
      }
      const result = await discovery.revokeInvite(blobId, id.ed25519Seed);
      if (result.kind === "revoked") {
        // Wipe the panel back to the form. Surface a one-liner
        // through the `error` channel (no dedicated success
        // banner — this is a terminal action; the form is
        // about to re-render).
        reset();
        setError("Invite revoked.");
      } else {
        // 404 — already gone or someone else's blob. Either
        // way, wipe the panel.
        reset();
        setError("Invite no longer exists.");
      }
    } catch (err) {
      setError(formatError(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section style={panelStyle()}>
      <h2 style={{ "margin-top": 0 }}>Generate invite link</h2>
      <p style={{ color: "var(--surface-muted)", "margin-top": 0 }}>
        Anyone holding the link can send you a ContactRequest.
        Short pointer URLs by default; falls back to inline-bytes
        if the discovery service is unreachable.
      </p>

      <Show
        when={generatedUrl() === null}
        fallback={
          <GeneratedUrlPanel
            url={generatedUrl() ?? ""}
            kind={generatedKind() ?? "pointer"}
            copied={copied}
            busy={busy}
            canRevoke={generatedBlobId() !== null}
            onCopy={() => {
              void onCopy();
            }}
            onRevoke={() => {
              void onRevoke();
            }}
            onReset={reset}
          />
        }
      >
        <form onSubmit={onGenerate}>
          <label style={{ display: "block", "margin-bottom": "0.6rem" }}>
            <span>Label</span>
            <input
              type="text"
              value={label()}
              onInput={(e) => setLabel(e.currentTarget.value)}
              placeholder="alice@home"
              maxlength={INVITE_LABEL_MAX_LEN}
              style={{ width: "100%", "margin-top": "0.25rem" }}
              disabled={busy()}
            />
            <span
              style={{
                color: "var(--surface-muted)",
                "font-size": "0.8rem",
                display: "block",
                "margin-top": "0.2rem",
              }}
            >
              Optional, ≤ {INVITE_LABEL_MAX_LEN} chars. Shown to the
              joiner before they accept.
            </span>
          </label>

          <label style={{ display: "block", "margin-bottom": "0.6rem" }}>
            <span>Expiry</span>
            <select
              value={expiry()}
              onChange={(e) => setExpiry(e.currentTarget.value as ExpiryPreset)}
              style={{ width: "100%", "margin-top": "0.25rem" }}
              disabled={busy()}
            >
              <option value="1d">1 day</option>
              <option value="7d">7 days (default)</option>
              <option value="30d">30 days</option>
              <option value="never">Never (stalking risk — see ADR-0005)</option>
            </select>
          </label>

          <label style={{ display: "block", "margin-bottom": "0.6rem" }}>
            <span>Max uses</span>
            <select
              value={maxUses()}
              onChange={(e) =>
                setMaxUses(e.currentTarget.value as MaxUsesPreset)
              }
              style={{ width: "100%", "margin-top": "0.25rem" }}
              disabled={busy()}
            >
              <option value="single">Single-use (default)</option>
              <option value="ten">Up to 10 uses</option>
              <option value="unlimited">Unlimited</option>
            </select>
            <span
              style={{
                color: "var(--surface-muted)",
                "font-size": "0.8rem",
                display: "block",
                "margin-top": "0.2rem",
              }}
            >
              Enforced server-side on the pointer path (1.18d2a);
              a UI hint only on the inline-fallback path.
            </span>
          </label>

          <label
            style={{
              display: "flex",
              gap: "0.5rem",
              "align-items": "center",
              "margin-bottom": "0.6rem",
            }}
          >
            <input
              type="checkbox"
              checked={bootstrapReferral()}
              onChange={(e) => setBootstrapReferral(e.currentTarget.checked)}
              disabled={busy()}
            />
            <span>Referral link (for someone without a Lakko account yet)</span>
          </label>

          {error() !== null && <div class="form-error">{error()}</div>}

          <button class="primary" type="submit" disabled={busy()}>
            {busy() ? "Generating…" : "Generate"}
          </button>
        </form>
      </Show>
    </section>
  );
}

/**
 * Phase 1.18d2b-list — "Active invites" panel. Lists every live
 * invite the current owner has uploaded across all their
 * devices, with a per-row Revoke button.
 *
 * Sits below [`InvitePanel`] in the Contacts route. The list is
 * **not auto-loaded** — the user clicks Refresh to pull it. The
 * round-trip is cheap (one signed SELECT) but doing it on every
 * page mount would be wasted work for users who never use
 * invites; explicit gesture also keeps the timing-correlation
 * side-channel ("Alice's discovery server sees an owner-list
 * query") under user control rather than firing on every
 * navigation.
 *
 * The Revoke flow reuses the existing
 * [`DiscoveryClient.revokeInvite`] surface from 1.18d2b-web; the
 * only new wire-format here is the list query itself.
 */
function ActiveInvitesPanel(): JSX.Element {
  const auth = useAuth();
  const chats = useChats();
  // Same wiring pattern as InvitePanel — bind the session token
  // so the protected list endpoint sees the bearer.
  const discovery = new DiscoveryClient();
  discovery.setSessionToken(auth.identitySecrets()?.sessionToken ?? null);

  const [invites, setInvites] = createSignal<ActiveInvite[]>([]);
  // `null` = never loaded yet; `"loaded"` = at least one
  // successful refresh has populated `invites`. Distinguishes
  // "user hasn't clicked Refresh yet" from "Refresh ran and
  // returned empty list".
  const [loadStatus, setLoadStatus] = createSignal<null | "loaded">(null);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [revokeBusySlug, setRevokeBusySlug] = createSignal<string | null>(null);

  async function onRefresh(): Promise<void> {
    setError(null);
    setBusy(true);
    try {
      const secrets = auth.identitySecrets();
      const id = chats.identity();
      if (secrets === null || id === null) {
        throw new Error("re-enter your password to list invites");
      }
      const result = await discovery.listInvitesByOwner(
        secrets.accountId,
        id.ed25519Seed,
      );
      if (result.kind === "not-found-or-not-owner") {
        // No identity bundle for this account_id, or signature
        // mismatch. Either way, present as "no invites" — the
        // most common path here is "owner has never uploaded an
        // invite, so there's no row to match anyway."
        setInvites([]);
        setLoadStatus("loaded");
        return;
      }
      setInvites(result.invites);
      setLoadStatus("loaded");
    } catch (err) {
      setError(formatError(err));
    } finally {
      setBusy(false);
    }
  }

  async function onRevokeRow(row: ActiveInvite): Promise<void> {
    setError(null);
    setRevokeBusySlug(row.blobIdSlug);
    try {
      const id = chats.identity();
      if (id === null) {
        throw new Error("re-enter your password to revoke invites");
      }
      const blobId = base64UrlDecodeNoPad(row.blobIdSlug);
      const result = await discovery.revokeInvite(blobId, id.ed25519Seed);
      // Drop the row locally regardless of `revoked` vs
      // `not-found-or-not-owner` — both terminal states mean
      // the row is gone server-side.
      setInvites(invites().filter((r) => r.blobIdSlug !== row.blobIdSlug));
      if (result.kind === "not-found-or-not-owner") {
        setError(
          `"${row.label || "(unlabelled)"}" was already gone server-side.`,
        );
      }
    } catch (err) {
      setError(formatError(err));
    } finally {
      setRevokeBusySlug(null);
    }
  }

  return (
    <section style={panelStyle()}>
      <h2 style={{ "margin-top": 0 }}>
        Active invites
        <Show when={loadStatus() === "loaded" && invites().length > 0}>
          <span
            style={{
              "margin-left": "0.5rem",
              color: "var(--surface-muted)",
              "font-weight": "normal",
              "font-size": "0.9rem",
            }}
          >
            ({invites().length})
          </span>
        </Show>
      </h2>
      <p style={{ color: "var(--surface-muted)", "margin-top": 0 }}>
        Lists every live invite-link this account has issued — across
        all your devices. Tap Refresh to pull the current state.
      </p>

      <div style={{ display: "flex", gap: "0.5rem", "margin-bottom": "0.75rem" }}>
        <button
          type="button"
          disabled={busy()}
          onClick={() => {
            void onRefresh();
          }}
        >
          {busy() ? "Loading…" : "Refresh"}
        </button>
      </div>

      <Show when={error() !== null}>
        <div class="form-error">{error()}</div>
      </Show>

      <Show
        when={loadStatus() === "loaded"}
        fallback={
          <p style={{ color: "var(--surface-muted)" }}>
            Click Refresh to load your live invites.
          </p>
        }
      >
        <Show
          when={invites().length > 0}
          fallback={
            <p style={{ color: "var(--surface-muted)" }}>
              No live invites — every link has been revoked or
              expired.
            </p>
          }
        >
          <ul style={listStyle()}>
            <For each={invites()}>
              {(row) => (
                <li style={rowStyle()}>
                  <div style={{ flex: 1, "min-width": 0 }}>
                    <strong>{row.label || "(unlabelled)"}</strong>
                    <div
                      style={{
                        color: "var(--surface-muted)",
                        "font-size": "0.85rem",
                        "margin-top": "0.2rem",
                      }}
                    >
                      <code>{row.blobIdSlug.slice(0, 16)}…</code>
                      {" · "}
                      {describeExpiryUnixMs(row.expiryMs)}
                      {" · "}
                      {describeUsage(row.usedCount, row.maxUses)}
                      <Show when={row.bootstrapReferral}>
                        {" · referral"}
                      </Show>
                    </div>
                  </div>
                  <button
                    type="button"
                    disabled={revokeBusySlug() !== null}
                    onClick={() => {
                      void onRevokeRow(row);
                    }}
                  >
                    {revokeBusySlug() === row.blobIdSlug
                      ? "Revoking…"
                      : "Revoke"}
                  </button>
                </li>
              )}
            </For>
          </ul>
        </Show>
      </Show>
    </section>
  );
}

/** Human-readable rendering for `expiry_ms` straight off the
 *  wire. `0` is the `NEVER_EXPIRES` sentinel; anything in the
 *  past is rendered as "expired" (defensive — the server's
 *  filter shouldn't return them, but a near-instant expiry
 *  could land in the response and lose the race). */
function describeExpiryUnixMs(expiryMs: number): string {
  if (expiryMs === 0) return "never expires";
  const nowMs = Date.now();
  if (expiryMs <= nowMs) return "expired";
  const deltaMs = expiryMs - nowMs;
  const days = Math.floor(deltaMs / (24 * 60 * 60 * 1000));
  if (days >= 1) return `expires in ${days}d`;
  const hours = Math.floor(deltaMs / (60 * 60 * 1000));
  if (hours >= 1) return `expires in ${hours}h`;
  const minutes = Math.max(1, Math.floor(deltaMs / (60 * 1000)));
  return `expires in ${minutes}m`;
}

/** Human-readable rendering for the `(used_count, max_uses)`
 *  pair using the same sentinel mapping the server enforces:
 *  `0` ⇒ single-use, `0xFFFFFFFF` ⇒ unlimited. */
function describeUsage(used: number, max: number): string {
  if (max === 0) return used >= 1 ? "used 1/1" : "0/1 used";
  if (max === 0xffff_ffff) return `${used} use${used === 1 ? "" : "s"}`;
  return `${used}/${max} used`;
}

function GeneratedUrlPanel(props: {
  url: string;
  kind: "pointer" | "inline-fallback";
  copied: () => boolean;
  busy: () => boolean;
  /** `true` only on the pointer-form path. Inline-bytes URLs
   *  have no associated server row, so revoke is unavailable
   *  by construction. */
  canRevoke: boolean;
  onCopy: () => void;
  onRevoke: () => void;
  onReset: () => void;
}): JSX.Element {
  return (
    <div>
      <p style={{ margin: "0 0 0.5rem 0" }}>
        <strong>Share this URL out-of-band.</strong>{" "}
        <Show
          when={props.kind === "pointer"}
          fallback={
            <span>
              Discovery service was unreachable — falling back to an
              <strong> inline-bytes URL</strong>. The capability sits
              in the URL itself; anyone with the link can send you a
              ContactRequest until it expires.
            </span>
          }
        >
          <span>
            Short pointer URL — the capability lives on the
            discovery service. Anyone with the link can send you a
            ContactRequest until it expires.
          </span>
        </Show>
      </p>
      <textarea
        readonly
        value={props.url}
        rows={3}
        style={{
          width: "100%",
          "font-family": "monospace",
          "font-size": "0.8rem",
        }}
        onClick={(e) => e.currentTarget.select()}
      />
      <div style={{ display: "flex", gap: "0.5rem", "margin-top": "0.5rem", "flex-wrap": "wrap" }}>
        <button
          class="primary"
          type="button"
          onClick={props.onCopy}
          disabled={props.busy()}
        >
          {props.copied() ? "Copied" : "Copy to clipboard"}
        </button>
        <button type="button" onClick={props.onReset} disabled={props.busy()}>
          Generate another
        </button>
        <Show when={props.canRevoke}>
          <button
            type="button"
            onClick={props.onRevoke}
            disabled={props.busy()}
            style={{
              "margin-left": "auto",
              color: "var(--danger-fg, currentColor)",
            }}
          >
            {props.busy() ? "Revoking…" : "Revoke this invite"}
          </button>
        </Show>
      </div>
      <Show when={!props.canRevoke}>
        <p
          style={{
            color: "var(--surface-muted)",
            "font-size": "0.8rem",
            "margin-top": "0.5rem",
          }}
        >
          (Revoke unavailable on the inline-bytes fallback — no server
          row to delete.)
        </p>
      </Show>
    </div>
  );
}

function ActiveGroupsPanel(props: {
  groups: () => readonly ActiveGroup[];
  /** Reactive accepted-contacts list (ADR-0021 follow-up). A chat
   *  linked to a contact (by group id) renders the contact's
   *  user-chosen displayName instead of the static creation-time
   *  label, so a rename propagates here too. */
  accepted: () => AcceptedContact[];
}): JSX.Element {
  const chats = useChats();
  /** Best label for a chat row: the linked contact's user-chosen
   *  displayName, else the chat's own label. Resolves the contact by the
   *  stored groupIdB64 fast-path (accept-created chats) and, universally,
   *  by the group's non-self member pubkey (outbound + Welcome-joined
   *  chats that carry no stored link) — ADR-0021. */
  const chatLabel = (g: ActiveGroup): string => {
    const accepted = props.accepted();
    const byId = accepted.find((c) => c.groupIdB64 === g.groupIdB64);
    if (byId !== undefined && byId.displayName.length > 0) {
      return byId.displayName;
    }
    const own = chats.identity();
    const ownB64 = own === null ? null : base64Encode(own.signaturePublicKey);
    let memberB64s: string[] = [];
    try {
      memberB64s = (
        g.group.members() as Array<{ signaturePublicKey: Uint8Array }>
      ).map((m) => base64Encode(m.signaturePublicKey));
    } catch {
      // members() can throw on a torn group; fall through to the label.
    }
    const byMember = peerContactByMemberPubkeys(memberB64s, ownB64, accepted);
    if (byMember !== undefined && byMember.displayName.length > 0) {
      return byMember.displayName;
    }
    return g.label;
  };
  // Phase 2a3e2c-web — reactive flags from the cross-device
  // sync runtime. Reads here drive (a) the pin / mute /
  // archive indicators next to each row, (b) the sort
  // ordering (pinned top, archived bottom).
  const sync = useDeviceGroupSync();
  const auth = useAuth();
  // Phase 2a3e2c-followup-web — per-thread highest message-id
  // map, sourced from the persistent message log. Unread
  // count = highestMessageId - readCursor for the same
  // group. We load the log once on mount and refresh
  // whenever the auth account_id changes; live changes
  // happen on Chat.tsx mutations which run in another tab /
  // route, so the next visit picks them up on its own mount.
  // Per-group sorted list of incoming/system messageIds —
  // unread count is `idsAboveCursor.length`, so we keep the
  // full vector instead of just the max. Web's message-log
  // store carries the full timeline so this is cheap; the
  // sort lets `unreadFor` early-exit at the first id ≤ cursor.
  const [incomingIds, setIncomingIds] = createSignal<
    { readonly [groupIdHex: string]: readonly bigint[] }
  >({});

  async function reloadIncomingIds(): Promise<void> {
    const s = auth.state();
    if (s.kind !== "session") return;
    const accountIdB64 = s.account.accountId;
    const deviceKey = await getDeviceKey(accountIdB64);
    if (deviceKey === null) return;
    const log = await loadMessageLog(accountIdB64, deviceKey);
    if (log === null) return;
    const map: { [hex: string]: bigint[] } = {};
    for (const [hex, slot] of Object.entries(log.groups)) {
      const ids: bigint[] = [];
      for (const m of slot.messages) {
        if (m.direction === "outgoing") continue;
        try {
          ids.push(BigInt(m.messageId));
        } catch {
          // Non-numeric message id — skip.
        }
      }
      if (ids.length > 0) {
        ids.sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
        map[hex] = ids;
      }
    }
    setIncomingIds(map);
  }

  onMount(() => {
    void reloadIncomingIds();
    // G-8: the badge used to refresh only on its next mount. Subscribe to
    // message-log changes (a message written from another tab / the Chat
    // route) so it reloads live. Same-account filter so an unrelated
    // account's write (multi-account on one browser) is ignored.
    const unsubscribe = subscribeMessageLogChanged((account) => {
      const s = auth.state();
      if (s.kind === "session" && s.account.accountId === account) {
        void reloadIncomingIds();
      }
    });
    onCleanup(unsubscribe);
  });

  function unreadFor(groupIdBytes: Uint8Array): number {
    const hex = syncGroupIdHex(groupIdBytes);
    const ids = incomingIds()[hex];
    if (ids === undefined || ids.length === 0) return 0;
    const cursor = sync.readCursors()[hex];
    if (cursor === undefined || cursor.upToMessageId.length < 8) {
      // No cursor yet — treat as "all read" so we don't blast
      // the badge during cold start; the chat-screen mount
      // broadcasts a cursor within 1.5 s.
      return 0;
    }
    let cursorId: bigint = 0n;
    const bytes = cursor.upToMessageId;
    for (let i = 0; i < 8; i += 1) {
      cursorId = (cursorId << 8n) | BigInt(bytes[i] as number);
    }
    // Count from the high end. Sorted ascending, so the
    // first id ≤ cursor short-circuits.
    let unread = 0;
    for (let i = ids.length - 1; i >= 0; i -= 1) {
      const id = ids[i] as bigint;
      if (id > cursorId) {
        unread += 1;
        if (unread > 99) return 100;
      } else {
        break;
      }
    }
    return unread;
  }

  /** Pinned → top, archived → bottom, otherwise stable on
   *  creation order. */
  const sortedGroups = (): readonly ActiveGroup[] => {
    const flagsMap = sync.flags();
    const items = [...props.groups()];
    items.sort((a, b) => {
      const fa = flagsMap[syncGroupIdHex(a.group.groupId)];
      const fb = flagsMap[syncGroupIdHex(b.group.groupId)];
      const aPinned = fa?.pinned === true;
      const bPinned = fb?.pinned === true;
      if (aPinned !== bPinned) return aPinned ? -1 : 1;
      const aArchived = fa?.archived === true;
      const bArchived = fb?.archived === true;
      if (aArchived !== bArchived) return aArchived ? 1 : -1;
      return 0;
    });
    return items;
  };

  async function toggleFlag(
    g: ActiveGroup,
    field: "pinned" | "muted" | "archived",
  ): Promise<void> {
    const groupId = g.group.groupId;
    const key = syncGroupIdHex(groupId);
    const cur = sync.flags()[key];
    const next = {
      pinned: cur?.pinned ?? false,
      muted: cur?.muted ?? false,
      archived: cur?.archived ?? false,
      unreadOverride: cur?.unreadOverride ?? false,
    };
    next[field] = !next[field];
    try {
      await sync.setFlags(groupId, next);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn(`setFlags(${field}) failed`, err);
    }
  }

  return (
    <section style={panelStyle()}>
      <h2 style={{ "margin-top": 0 }}>
        Active MLS groups
        <Show when={props.groups().length > 0}>
          <span
            style={{
              "margin-left": "0.5rem",
              color: "var(--surface-muted)",
              "font-weight": "normal",
              "font-size": "0.9rem",
            }}
          >
            ({props.groups().length})
          </span>
        </Show>
      </h2>
      <Show
        when={props.groups().length > 0}
        fallback={
          <p style={{ color: "var(--surface-muted)" }}>
            No active groups. Start one above with "Start MLS group", or
            click Refresh in <em>Pending</em> — incoming Welcomes turn
            into active groups automatically.
          </p>
        }
      >
        <ul style={listStyle()}>
          <For each={sortedGroups()}>
            {(g) => {
              const key = syncGroupIdHex(g.group.groupId);
              const flags = (): {
                pinned: boolean;
                muted: boolean;
                archived: boolean;
              } => {
                const f = sync.flags()[key];
                return {
                  pinned: f?.pinned === true,
                  muted: f?.muted === true,
                  archived: f?.archived === true,
                };
              };
              return (
                <li
                  style={{
                    ...rowStyle(),
                    "align-items": "center",
                    opacity: flags().archived ? "0.6" : "1",
                  }}
                >
                  <div style={{ flex: 1 }}>
                    <strong>
                      <Show when={flags().pinned}>📌 </Show>
                      {chatLabel(g)}
                      <Show when={flags().muted}>
                        <span
                          style={{
                            "margin-left": "0.4rem",
                            color: "var(--surface-muted)",
                            "font-size": "0.85rem",
                          }}
                          title="Muted on this account"
                        >
                          🔇
                        </span>
                      </Show>
                      <Show when={flags().archived}>
                        <span
                          style={{
                            "margin-left": "0.4rem",
                            color: "var(--surface-muted)",
                            "font-size": "0.85rem",
                          }}
                          title="Archived on this account"
                        >
                          📦
                        </span>
                      </Show>
                    </strong>
                    <div
                      style={{
                        color: "var(--surface-muted)",
                        "font-size": "0.85rem",
                      }}
                    >
                      {g.group.memberCount} member
                      {g.group.memberCount === 1 ? "" : "s"} · seat #
                      {g.group.ownLeafIndex}
                    </div>
                  </div>
                  <div
                    style={{
                      display: "flex",
                      gap: "0.3rem",
                      "align-items": "center",
                    }}
                  >
                    <Show when={unreadFor(g.group.groupId) > 0}>
                      <span
                        title={`${unreadFor(g.group.groupId)} unread`}
                        style={{
                          "min-width": "1.5rem",
                          "text-align": "center",
                          padding: "0.1rem 0.4rem",
                          "border-radius": "999px",
                          background: flags().muted
                            ? "var(--surface-muted)"
                            : "var(--accent)",
                          color: flags().muted
                            ? "var(--surface)"
                            : "var(--accent-fg)",
                          "font-size": "0.75rem",
                          "font-weight": "600",
                        }}
                      >
                        {unreadFor(g.group.groupId) >= 100
                          ? "99+"
                          : unreadFor(g.group.groupId)}
                      </span>
                    </Show>
                    <button
                      type="button"
                      title={flags().pinned ? "Unpin" : "Pin"}
                      onClick={() => {
                        void toggleFlag(g, "pinned");
                      }}
                      style={iconBtnStyle(flags().pinned)}
                    >
                      📌
                    </button>
                    <button
                      type="button"
                      title={flags().muted ? "Unmute" : "Mute"}
                      onClick={() => {
                        void toggleFlag(g, "muted");
                      }}
                      style={iconBtnStyle(flags().muted)}
                    >
                      🔇
                    </button>
                    <button
                      type="button"
                      title={flags().archived ? "Unarchive" : "Archive"}
                      onClick={() => {
                        void toggleFlag(g, "archived");
                      }}
                      style={iconBtnStyle(flags().archived)}
                    >
                      📦
                    </button>
                    <A
                      href={paths.chat(
                        base64UrlEncodeNoPad(g.group.groupId),
                      )}
                      style={{
                        padding: "0.4rem 0.8rem",
                        background: "var(--accent)",
                        color: "var(--accent-fg)",
                        "border-radius": "var(--radius-sm)",
                        "text-decoration": "none",
                      }}
                    >
                      Open chat
                    </A>
                  </div>
                </li>
              );
            }}
          </For>
        </ul>
      </Show>
    </section>
  );
}

/** Round inline toggle button. `active` = filled background
 *  for the on-state; off-state shows a muted outline. The
 *  state across devices comes via DeviceGroupSync flags map. */
function iconBtnStyle(active: boolean): JSX.CSSProperties {
  return {
    width: "1.8rem",
    height: "1.8rem",
    padding: "0",
    "border-radius": "var(--radius-sm)",
    border: active
      ? "1px solid var(--accent)"
      : "1px solid var(--surface-muted)",
    background: active ? "var(--accent)" : "transparent",
    color: active ? "var(--accent-fg)" : "var(--surface-muted)",
    cursor: "pointer",
    "font-size": "0.9rem",
    "line-height": "1",
  };
}

interface PendingPanelProps {
  readonly pending: () => IncomingContactRequest[];
  readonly busy: () => boolean;
  readonly error: () => string | null;
  readonly onRefresh: () => void;
  readonly onAccept: (r: IncomingContactRequest) => void;
  readonly onDecline: (r: IncomingContactRequest) => void;
}

function PendingPanel(props: PendingPanelProps): JSX.Element {
  return (
    <section style={panelStyle()}>
      <header
        style={{
          display: "flex",
          "align-items": "center",
          "justify-content": "space-between",
          gap: "0.5rem",
        }}
      >
        <h2 style={{ margin: 0 }}>
          Pending
          <Show when={props.pending().length > 0}>
            <span
              style={{
                "margin-left": "0.5rem",
                color: "var(--surface-muted)",
                "font-weight": "normal",
                "font-size": "0.9rem",
              }}
            >
              ({props.pending().length})
            </span>
          </Show>
        </h2>
        <button
          type="button"
          onClick={props.onRefresh}
          disabled={props.busy()}
        >
          {props.busy() ? "Refreshing…" : "Refresh"}
        </button>
      </header>
      {props.error() !== null && (
        <div class="form-error">{props.error()}</div>
      )}
      <Show
        when={props.pending().length > 0}
        fallback={
          <p style={{ color: "var(--surface-muted)" }}>
            No incoming contact requests.
          </p>
        }
      >
        <ul style={listStyle()}>
          <For each={props.pending()}>
            {(req) => (
              <li style={rowStyle()}>
                <div style={{ flex: 1, "min-width": 0 }}>
                  <div style={{ "font-family": "ui-monospace, monospace" }}>
                    From <strong>@{fingerprintShort(req.fromPubkey)}</strong>
                  </div>
                  <Show when={req.intro !== null && req.intro !== ""}>
                    <div
                      style={{
                        "margin-top": "0.25rem",
                        "white-space": "pre-wrap",
                      }}
                    >
                      {req.intro}
                    </div>
                  </Show>
                  <div
                    style={{
                      color: "var(--surface-muted)",
                      "font-size": "0.85rem",
                      "margin-top": "0.25rem",
                    }}
                  >
                    {req.createdAt}
                  </div>
                </div>
                <div style={{ display: "flex", gap: "0.5rem" }}>
                  <button
                    class="primary"
                    type="button"
                    onClick={() => {
                      void props.onAccept(req);
                    }}
                  >
                    Accept
                  </button>
                  <button
                    type="button"
                    onClick={() => props.onDecline(req)}
                  >
                    Decline
                  </button>
                </div>
              </li>
            )}
          </For>
        </ul>
      </Show>
    </section>
  );
}

interface AcceptedPanelProps {
  readonly accepted: () => AcceptedContact[];
  /** Local user's Ed25519 verifying key (32 bytes) — required to
   *  compute safety numbers against accepted contacts. Read from
   *  the chat provider's `identity()`; `null` only while that
   *  identity signal has not resolved yet (the AcceptedRow falls
   *  back to "safety number unavailable" for that window). */
  readonly myPubkey: () => Uint8Array | null;
  readonly onRemove: (c: AcceptedContact) => void;
  /** Set this contact's user-chosen display name (ADR-0021 follow-up).
   *  A blank name resets it to the key-fingerprint default. */
  readonly onRename: (c: AcceptedContact, newName: string) => void;
}

function AcceptedPanel(props: AcceptedPanelProps): JSX.Element {
  return (
    <section style={panelStyle()}>
      <h2 style={{ "margin-top": 0 }}>
        Contacts
        <Show when={props.accepted().length > 0}>
          <span
            style={{
              "margin-left": "0.5rem",
              color: "var(--surface-muted)",
              "font-weight": "normal",
              "font-size": "0.9rem",
            }}
          >
            ({props.accepted().length})
          </span>
        </Show>
      </h2>
      <Show
        when={props.accepted().length > 0}
        fallback={
          <p style={{ color: "var(--surface-muted)" }}>
            No accepted contacts yet.
          </p>
        }
      >
        <ul style={listStyle()}>
          <For each={props.accepted()}>
            {(c) => (
              <AcceptedRow
                contact={c}
                myPubkey={props.myPubkey}
                onRemove={() => props.onRemove(c)}
                onRename={(name) => props.onRename(c, name)}
              />
            )}
          </For>
        </ul>
      </Show>
    </section>
  );
}

/** Single accepted-contact row with a "Show safety number"
 *  toggle (Phase 1.9b3 — ADR-0005 §13.5). The safety number is
 *  computed lazily on first toggle: BLAKE3 is fast but `loadWasm`
 *  has a one-time module-instantiate cost we don't want to pay
 *  on every row mount. */
interface AcceptedRowProps {
  readonly contact: AcceptedContact;
  readonly myPubkey: () => Uint8Array | null;
  readonly onRemove: () => void;
  /** Set this contact's display name; a blank value resets to the
   *  key-fingerprint default (ADR-0021 follow-up). */
  readonly onRename: (newName: string) => void;
}

function AcceptedRow(props: AcceptedRowProps): JSX.Element {
  const [open, setOpen] = createSignal(false);
  const [number, setNumber] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);
  const [err, setErr] = createSignal<string | null>(null);
  // ADR-0021 follow-up: inline display-name editor.
  const [editing, setEditing] = createSignal(false);
  const [draft, setDraft] = createSignal("");

  function startEdit(): void {
    setDraft(props.contact.displayName);
    setEditing(true);
  }
  function saveEdit(): void {
    props.onRename(draft());
    setEditing(false);
  }

  async function ensureNumber(): Promise<void> {
    if (number() !== null) return;
    const my = props.myPubkey();
    if (my === null) {
      setErr("safety number unavailable — sign in again to compute");
      return;
    }
    setBusy(true);
    setErr(null);
    try {
      const their = base64Decode(props.contact.signaturePubkeyB64);
      const n = await computeSafetyNumber(my, their);
      setNumber(n);
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  async function toggle(): Promise<void> {
    const next = !open();
    setOpen(next);
    if (next) {
      await ensureNumber();
    }
  }

  return (
    <li style={rowStyle()}>
      <div style={{ flex: 1 }}>
        <Show when={editing()} fallback={<strong>{props.contact.displayName}</strong>}>
          <div style={{ display: "flex", gap: "0.25rem", "align-items": "center" }}>
            <input
              type="text"
              value={draft()}
              placeholder="Display name"
              onInput={(e) => setDraft(e.currentTarget.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") saveEdit();
                if (e.key === "Escape") setEditing(false);
              }}
              style={{ flex: 1, "font-size": "0.9rem" }}
            />
            <button type="button" onClick={saveEdit}>
              Save
            </button>
            <button type="button" onClick={() => setEditing(false)}>
              Cancel
            </button>
          </div>
        </Show>
        <div
          style={{
            color: "var(--surface-muted)",
            "font-size": "0.85rem",
            "font-family": "ui-monospace, monospace",
          }}
        >
          {props.contact.signaturePubkeyB64.slice(0, 16)}…
        </div>
        <Show when={open()}>
          <div
            style={{
              "margin-top": "0.5rem",
              padding: "0.5rem 0.75rem",
              background: "var(--surface-canvas)",
              border: "1px solid var(--surface-border)",
              "border-radius": "var(--radius-sm)",
              "font-size": "0.85rem",
            }}
          >
            <div style={{ color: "var(--surface-muted)", "margin-bottom": "0.25rem" }}>
              Safety number — compare these digits with your
              contact out-of-band (voice / video / paper). If they
              match, your pinned identity keys agree.
            </div>
            <Show when={number() !== null} fallback={
              <div style={{ color: "var(--surface-muted)" }}>
                {busy() ? "computing…" : (err() ?? "—")}
              </div>
            }>
              <code style={{ "font-size": "1rem", "line-height": "1.5" }}>
                {number()}
              </code>
            </Show>
          </div>
        </Show>
      </div>
      <div style={{ display: "flex", "flex-direction": "column", gap: "0.25rem" }}>
        <button
          type="button"
          onClick={() => {
            void toggle();
          }}
          disabled={busy()}
        >
          {open() ? "Hide safety number" : "Show safety number"}
        </button>
        <button
          type="button"
          onClick={() => {
            if (editing()) {
              setEditing(false);
            } else {
              startEdit();
            }
          }}
        >
          {editing() ? "Close" : "Rename"}
        </button>
        <button type="button" onClick={props.onRemove}>
          Remove
        </button>
      </div>
    </li>
  );
}

function NeedsReauth(): JSX.Element {
  return (
    <section style={panelStyle()}>
      <p style={{ "margin-top": 0 }}>
        This page reload restored your session from the local
        device key, but the secrets needed to sign and decrypt
        contact requests aren't in memory yet. Please re-enter
        your password to use Contacts.
      </p>
      <p>
        <a href="/login">Re-enter password</a>
      </p>
    </section>
  );
}

// ----------------------------------------------------------------------
// Style helpers (kept inline so we don't pollute styles.css with
// per-route layout that 1.16e + 1.16f will rework anyway).
// ----------------------------------------------------------------------

function panelStyle(): JSX.CSSProperties {
  return {
    background: "var(--surface-elevated)",
    border: "1px solid var(--surface-border)",
    "border-radius": "var(--radius-md)",
    padding: "1rem",
    margin: "1rem 0",
  };
}

function listStyle(): JSX.CSSProperties {
  return {
    "list-style": "none",
    padding: 0,
    margin: 0,
    display: "flex",
    "flex-direction": "column",
    gap: "0.5rem",
  };
}

function rowStyle(): JSX.CSSProperties {
  return {
    display: "flex",
    "align-items": "flex-start",
    gap: "0.75rem",
    padding: "0.5rem 0.75rem",
    border: "1px solid var(--surface-border)",
    "border-radius": "var(--radius-sm)",
  };
}

// ----------------------------------------------------------------------
// Error formatting
// ----------------------------------------------------------------------

/** Render a typed-username into an `@handle` label for error copy.
 *  Trims surrounding whitespace + a leading `@` the user may have typed
 *  so we never double it; returns `undefined` for an empty input (the
 *  formatter then falls back to its neutral "This contact" subject). */
function atLabel(raw: string): string | undefined {
  const trimmed = raw.trim().replace(/^@+/, "");
  return trimmed.length > 0 ? `@${trimmed}` : undefined;
}

function formatError(err: unknown, peerLabel?: string): string {
  // KT-log S4 — a fail-closed supersession refusal (an unproven CHANGED
  // key) is the moment the reset-vs-compromise distinction matters most.
  // Render a reset-AWARE message driven by the best-effort log marker
  // the client attached (`loggedReset`). The refusal itself is unchanged
  // — gate 1 threw regardless of the log; this only colours the prompt.
  if (err instanceof SupersessionRefused) {
    return supersessionRefusedMessage(err.loggedReset, peerLabel);
  }
  if (err instanceof ContactsError) {
    switch (err.variant.kind) {
      case "no-identity-secrets":
        return "please re-enter your password";
      case "username-unknown":
        return `no account is registered as @${err.variant.username}`;
      case "no-identity-bundle":
        return "the recipient hasn't published an identity bundle yet";
      case "wrapped":
        return err.message;
    }
  }
  return err instanceof Error ? err.message : String(err);
}

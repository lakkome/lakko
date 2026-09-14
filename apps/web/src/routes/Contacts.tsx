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
 *   2. **Pending** — in memory only (no persistence; intro
 *      plaintext stays out of localStorage), held by the
 *      session-wide `<MailboxProvider>` whose long-poll loop fills
 *      it automatically. The "Refresh" button asks the same
 *      provider for an immediate fetch. Each row has Accept /
 *      Decline buttons.
 *   3. **Accepted** — local address book (public-key shaped),
 *      persisted in `contacts/local-store.ts`.
 *
 * Identity availability: every flow that needs the user's
 * Identity reads `useAuth().identitySecrets()`. A reload restores
 * those secrets with the session, so a `null` here is transient;
 * the fallback panel offers a real way back to the sign-in form
 * rather than a link the anonymous-only route would bounce.
 */
import {
  createEffect,
  createMemo,
  createSignal,
  For,
  on,
  Show,
  type JSX,
} from "solid-js";
import { useNavigate } from "@solidjs/router";
import { useAuth } from "../auth/AuthContext";
import {
  base64Decode,
  base64Encode,
  base64UrlDecodeNoPad,
} from "../auth/account-id";
import {
  ContactsClient,
  ContactsError,
  SupersessionRefused,
} from "../contacts/client";
import type { IncomingContactRequest, SendResult } from "../contacts/client";
import { supersessionRefusedMessage } from "../contacts/supersession-message";
import { useChats } from "../chat/ChatContext";
import { groupIdHex as syncGroupIdHex } from "../devices/sync/state";
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
  loadAccepted,
  removeAccepted,
  upsertAccepted,
  type AcceptedContact,
} from "../contacts/local-store";
import { useMailbox } from "../contacts/MailboxContext";
import { computeSafetyNumber } from "../contacts/safety";
import { useProfiles } from "../profile/ProfileContext";
import { createUsernameCheck } from "../profile/username-field";
import { mayLookUp, usernameReason, type UsernameCheck } from "../profile/rules";

export default function Contacts(): JSX.Element {
  const auth = useAuth();
  const chats = useChats();
  const profiles = useProfiles();
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
    // ADR-0048 §2 — every outgoing request carries this account's profile.
    ownProfileEnvelope: () => profiles.ownProfileEnvelope(),
    onUsernameResolved: (key, username) => profiles.learnUsername(key, username),
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
  const profiles = useProfiles();
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
  // ADR-0048 §4 — no lookup leaves the device for a name that fails the rule.
  const searchCheck = createUsernameCheck(searchInput);

  async function onSend(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    setSendError(null);
    setSendNotice(null);
    if (!mayLookUp(searchCheck())) return;
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
    if (!mayLookUp(searchCheck())) return;
    setSendBusy(true);
    try {
      const result = await props.client.startGroupWithUsername(
        requireIdentity(),
        searchInput(),
      );
      // ADR-0048 — a 1x1 chat is labelled by the resolver (the peer's name),
      // never by the handle typed here; this label is only the fallback.
      const active = chats.add({
        label: "Direct chat",
        group: result.group,
      });
      profiles.groupJoined(active.groupIdB64);
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
  // Owned by the session-wide `<MailboxProvider>`: its long-poll loop
  // dispatches requests into this list, acks into the address book and
  // Welcomes into joined groups, whether or not this route is mounted.
  // The Refresh button only asks it for an immediate fetch.
  const mailbox = useMailbox();
  // Errors from this route's own Accept path (group creation), shown in the
  // same slot as a failed manual refresh.
  const [acceptError, setAcceptError] = createSignal<string | null>(null);

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
      // ADR-0048 — no local rename on accept (keep one the user already set);
      // the name shown comes from the resolver.
      displayName:
        loadAccepted().find((c) => c.signaturePubkeyB64 === base64Encode(req.fromPubkey))
          ?.displayName ?? "",
      acceptedAtMs: Date.now(),
      // ADR-0021 — persist the peer's account_id (hex) when the request
      // carried it, so the SFU group-call roster can resolve names.
      ...(req.fromAccountId
        ? { accountIdHex: syncGroupIdHex(req.fromAccountId) }
        : {}),
    };
    setAccepted(upsertAccepted(contact));
    mailbox.removePending(req.messageId);
    setAcceptError(null);

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
      setAcceptError(
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
        label: "Direct chat",
        group: result.group,
      });
      // ADR-0048 §2 — founding a group is joining it.
      profiles.groupJoined(active.groupIdB64);
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
      setAcceptError(formatError(err, profiles.resolve(req.fromPubkey).name));
    }
  }

  function onDecline(req: IncomingContactRequest): void {
    mailbox.removePending(req.messageId);
  }

  // ---- accepted -------------------------------------------------
  const [accepted, setAccepted] = createSignal<AcceptedContact[]>(
    loadAccepted(),
  );
  // A contact ack the mailbox loop upserted while this route was open.
  createEffect(
    on(
      () => mailbox.acceptedVersion(),
      () => setAccepted(loadAccepted()),
      { defer: true },
    ),
  );
  function onRemoveAccepted(contact: AcceptedContact): void {
    setAccepted(removeAccepted(contact.signaturePubkeyB64));
  }
  /** ADR-0048 §5 — set the viewer's LOCAL RENAME of a contact. A blank value
   *  clears the rename, so the contact's own name shows again. Re-upserts by
   *  the same signature pubkey, so the link + account_id + group id are
   *  preserved. */
  function onRenameAccepted(contact: AcceptedContact, newName: string): void {
    setAccepted(upsertAccepted({ ...contact, displayName: newName.trim() }));
  }

  return (
    <>
      <p style={{ color: "var(--surface-muted)", "margin-top": 0 }}>
        Signed in as <strong>@{props.username}</strong>. Requests
        you send and receive are end-to-end encrypted; the
        server only sees ciphertext + routing pseudonyms.
      </p>

      <SendPanel
        searchCheck={searchCheck}
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
        pending={mailbox.pending}
        busy={mailbox.busy}
        error={() => acceptError() ?? mailbox.error()}
        onRefresh={() => {
          setAcceptError(null);
          void mailbox.refresh();
        }}
        onAccept={onAccept}
        onDecline={onDecline}
      />


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
  readonly searchCheck: () => UsernameCheck;
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
            aria-invalid={props.searchCheck().kind === "invalid"}
          />
        </label>
        <Show when={usernameReason(props.searchCheck())}>
          {(reason) => (
            <div class="form-error" role="status">
              {reason()}
            </div>
          )}
        </Show>
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
          <button
            class="primary"
            type="submit"
            disabled={props.busy() || !mayLookUp(props.searchCheck())}
          >
            {props.busy() ? "Sending…" : "Send contact request"}
          </button>
          <button
            type="button"
            disabled={props.busy() || !mayLookUp(props.searchCheck())}
            onClick={props.onStartGroup}
            title="Start an encrypted chat with this person."
          >
            Start chat
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

interface PendingPanelProps {
  readonly pending: () => readonly IncomingContactRequest[];
  readonly busy: () => boolean;
  readonly error: () => string | null;
  readonly onRefresh: () => void;
  readonly onAccept: (r: IncomingContactRequest) => void;
  readonly onDecline: (r: IncomingContactRequest) => void;
}

function PendingPanel(props: PendingPanelProps): JSX.Element {
  const profiles = useProfiles();
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
                  <div>
                    {/* ADR-0048 UX §5 — `Alice Petrova · @alice wants to
                        connect`, or the verified username / fingerprint when
                        that is all there is. */}
                    <strong>{profiles.resolve(req.fromPubkey).listLabel}</strong>{" "}
                    wants to connect
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
  const profiles = useProfiles();
  const resolved = (): ReturnType<typeof profiles.resolve> =>
    profiles.resolve(props.contact.signaturePubkeyB64);
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
        <Show when={editing()} fallback={<strong>{resolved().listLabel}</strong>}>
          <div style={{ display: "flex", gap: "0.25rem", "align-items": "center" }}>
            <input
              type="text"
              value={draft()}
              placeholder="Your name for this contact (blank = their own)"
              aria-label="Your name for this contact"
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
          Fingerprint {resolved().fingerprint}
        </div>
        {/* ADR-0048 UX §6 — a rename wins everywhere, so the detail says what
            the person calls themselves; a failed username claim is noted here
            and nowhere else. */}
        <Show when={resolved().theirName}>
          {(their) => (
            <div style={{ color: "var(--surface-muted)", "font-size": "0.85rem" }}>
              Their name: {their()}
            </div>
          )}
        </Show>
        <Show when={resolved().usernameNotVerified}>
          <div style={{ color: "var(--surface-muted)", "font-size": "0.85rem" }}>
            Username not verified
          </div>
        </Show>
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
  const auth = useAuth();
  const navigate = useNavigate();
  // `/login` is an anonymous-only route: linking to it while signed in
  // bounces straight back home. Sign out first, then go there — the same
  // sequence the sidebar's Log out uses.
  function signInAgain(): void {
    auth.logout();
    navigate(paths.login, { replace: true });
  }
  return (
    <section style={panelStyle()}>
      <p style={{ "margin-top": 0 }}>
        The keys needed to sign and decrypt contact requests are not
        available in this session. Sign in again to use Contacts.
      </p>
      <p>
        <button class="primary" type="button" onClick={signInAgain}>
          Sign in again
        </button>
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

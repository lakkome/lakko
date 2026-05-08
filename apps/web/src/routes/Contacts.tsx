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
import { createMemo, createSignal, For, Show, type JSX } from "solid-js";
import { A } from "@solidjs/router";
import { useAuth } from "../auth/AuthContext";
import { base64Encode, base64UrlEncodeNoPad } from "../auth/account-id";
import { ContactsClient, ContactsError } from "../contacts/client";
import type { IncomingContactRequest, SendResult } from "../contacts/client";
import { useChats, type ActiveGroup } from "../chat/ChatContext";
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
import { DiscoveryClient } from "../discovery/client";
import {
  fingerprintShort,
  loadAccepted,
  loadCursor,
  removeAccepted,
  saveCursor,
  upsertAccepted,
  type AcceptedContact,
} from "../contacts/local-store";

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
      setSendError(formatError(err));
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
      setSendError(formatError(err));
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
        } else {
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
    };
    setAccepted(upsertAccepted(contact));
    setPending(pending().filter((r) => r.messageId !== req.messageId));

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
      chats.add({
        label: `Group with @${fingerprintShort(req.fromPubkey)}`,
        group: result.group,
      });
    } catch (err) {
      // Don't unwind the acceptance; just surface the error.
      setPollError(formatError(err));
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

      <ActiveGroupsPanel groups={chats.groups} />

      <AcceptedPanel
        accepted={accepted}
        onRemove={onRemoveAccepted}
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
  // Phase 1.18c — local-only state. Form drives
  // `wasm.InviteCapability.compose(...)` and produces a
  // `https://<host>/i/<slug>` URL. No server round-trip; the
  // capability bytes are fully self-contained inside the URL
  // until 1.18d switches to the short-pointer + blob form.
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
  const discovery = new DiscoveryClient();

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
    setError(null);
    setCopied(false);
    setLabel("");
    setBootstrapReferral(false);
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
            onCopy={() => {
              void onCopy();
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
              UI hint in V1; server-side enforcement lands in 1.18d.
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

function GeneratedUrlPanel(props: {
  url: string;
  kind: "pointer" | "inline-fallback";
  copied: () => boolean;
  onCopy: () => void;
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
      <div style={{ display: "flex", gap: "0.5rem", "margin-top": "0.5rem" }}>
        <button class="primary" type="button" onClick={props.onCopy}>
          {props.copied() ? "Copied" : "Copy to clipboard"}
        </button>
        <button type="button" onClick={props.onReset}>
          Generate another
        </button>
      </div>
    </div>
  );
}

function ActiveGroupsPanel(props: {
  groups: () => readonly ActiveGroup[];
}): JSX.Element {
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
          <For each={props.groups()}>
            {(g) => (
              <li
                style={{
                  ...rowStyle(),
                  "align-items": "center",
                }}
              >
                <div style={{ flex: 1 }}>
                  <strong>{g.label}</strong>
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
              </li>
            )}
          </For>
        </ul>
      </Show>
    </section>
  );
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
  readonly onRemove: (c: AcceptedContact) => void;
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
              <li style={rowStyle()}>
                <div style={{ flex: 1 }}>
                  <strong>{c.displayName}</strong>
                  <div
                    style={{
                      color: "var(--surface-muted)",
                      "font-size": "0.85rem",
                      "font-family": "ui-monospace, monospace",
                    }}
                  >
                    {c.signaturePubkeyB64.slice(0, 16)}…
                  </div>
                </div>
                <button type="button" onClick={() => props.onRemove(c)}>
                  Remove
                </button>
              </li>
            )}
          </For>
        </ul>
      </Show>
    </section>
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

function formatError(err: unknown): string {
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

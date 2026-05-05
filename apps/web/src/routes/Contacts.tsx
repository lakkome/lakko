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
import { useAuth } from "../auth/AuthContext";
import { base64Encode } from "../auth/account-id";
import { ContactsClient, ContactsError } from "../contacts/client";
import type { IncomingContactRequest, SendResult } from "../contacts/client";
import type { Group as WasmGroup } from "../../wasm/lakko_ffi_wasm";
import { ChatClient } from "../chat/client";
import type { IncomingChatEvent } from "../chat/client";
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
  const client = new ContactsClient();

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
      addActiveGroup({
        kind: "started",
        peerUsername: result.recipientUsername,
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
            addActiveGroup({
              kind: "joined",
              welcomeMessageId: ev.welcome.messageId,
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
  // In-memory only — Phase-1 doesn't persist MLS state. A
  // page reload drops every group; the peer still has theirs.
  // See ROADMAP 1.16e6 for the persistence sub-sprint.
  type ActiveGroup =
    | { kind: "started"; peerUsername: string; group: WasmGroup }
    | { kind: "joined"; welcomeMessageId: number; group: WasmGroup };
  const [activeGroups, setActiveGroups] = createSignal<ActiveGroup[]>([]);
  function addActiveGroup(g: ActiveGroup): void {
    setActiveGroups([...activeGroups(), g]);
  }

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
      addActiveGroup({
        kind: "started",
        peerUsername: `@${fingerprintShort(req.fromPubkey)}`,
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

      <ActiveGroupsPanel
        groups={activeGroups}
        identity={requireIdentity}
      />

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

interface ActiveGroupEntry {
  readonly kind: "started" | "joined";
  readonly peerUsername?: string;
  readonly welcomeMessageId?: number;
  readonly group: WasmGroup;
}

function ActiveGroupsPanel(props: {
  groups: () => readonly ActiveGroupEntry[];
  identity: () => NonNullable<
    ReturnType<ReturnType<typeof useAuth>["identitySecrets"]>
  >;
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
            {(g) => <ActiveGroupRow group={g} identity={props.identity} />}
          </For>
        </ul>
      </Show>
    </section>
  );
}

function ActiveGroupRow(props: {
  group: ActiveGroupEntry;
  identity: () => NonNullable<
    ReturnType<ReturnType<typeof useAuth>["identitySecrets"]>
  >;
}): JSX.Element {
  const [open, setOpen] = createSignal(false);
  const title = (): string =>
    props.group.kind === "started"
      ? `Group with ${props.group.peerUsername ?? "?"}`
      : `Joined group (Welcome ${props.group.welcomeMessageId})`;
  return (
    <li style={{ ...rowStyle(), "flex-direction": "column", gap: "0.5rem" }}>
      <div
        style={{
          display: "flex",
          "align-items": "center",
          gap: "0.75rem",
          width: "100%",
        }}
      >
        <div style={{ flex: 1 }}>
          <strong>{title()}</strong>
          <div
            style={{
              color: "var(--surface-muted)",
              "font-size": "0.85rem",
            }}
          >
            {props.group.group.memberCount} member
            {props.group.group.memberCount === 1 ? "" : "s"} · seat #
            {props.group.group.ownLeafIndex}
          </div>
        </div>
        <button type="button" onClick={() => setOpen(!open())}>
          {open() ? "Close chat" : "Open chat"}
        </button>
      </div>
      <Show when={open()}>
        <ChatPane group={props.group.group} identity={props.identity} />
      </Show>
    </li>
  );
}

function ChatPane(props: {
  group: WasmGroup;
  identity: () => NonNullable<
    ReturnType<ReturnType<typeof useAuth>["identitySecrets"]>
  >;
}): JSX.Element {
  const chat = new ChatClient();

  const [history, setHistory] = createSignal<IncomingChatEvent[]>([]);
  const [cursor, setCursor] = createSignal(0);
  const [input, setInput] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  async function refresh(): Promise<void> {
    setError(null);
    setBusy(true);
    try {
      const result = await chat.pollMessages(
        props.identity(),
        props.group,
        cursor(),
      );
      if (result.events.length > 0) {
        // De-dup by messageId — multiple refreshes on the same
        // cursor are idempotent.
        const seen = new Set(history().map((e) => e.messageId));
        const fresh = result.events.filter((e) => !seen.has(e.messageId));
        if (fresh.length > 0) {
          setHistory([...history(), ...fresh]);
        }
      }
      setCursor(result.cursor);
    } catch (err) {
      setError(formatError(err));
    } finally {
      setBusy(false);
    }
  }

  async function onSend(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    const text = input();
    if (text.length === 0) return;
    setError(null);
    setBusy(true);
    try {
      const result = await chat.sendMessage(
        props.identity(),
        props.group,
        new TextEncoder().encode(text),
      );
      // Optimistically render — the receive side dedupes on
      // messageId, so even if a refresh later returns this
      // entry it'll be filtered.
      setHistory([
        ...history(),
        {
          kind: "application",
          messageId: result.messageId,
          plaintext: text,
          createdAt: result.createdAt,
        },
      ]);
      setInput("");
    } catch (err) {
      setError(formatError(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div
      style={{
        width: "100%",
        background: "var(--surface-bg)",
        border: "1px solid var(--surface-border)",
        "border-radius": "var(--radius-sm)",
        padding: "0.5rem 0.75rem",
        display: "flex",
        "flex-direction": "column",
        gap: "0.5rem",
      }}
    >
      <header
        style={{
          display: "flex",
          "align-items": "center",
          "justify-content": "space-between",
        }}
      >
        <strong style={{ "font-size": "0.9rem" }}>Timeline</strong>
        <button
          type="button"
          onClick={() => {
            void refresh();
          }}
          disabled={busy()}
        >
          {busy() ? "Refreshing…" : "Refresh"}
        </button>
      </header>
      {error() !== null && <div class="form-error">{error()}</div>}
      <Show
        when={history().length > 0}
        fallback={
          <p
            style={{
              color: "var(--surface-muted)",
              "font-size": "0.85rem",
              margin: 0,
            }}
          >
            No messages yet. Send the first one below or click Refresh
            after the peer sends.
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
            gap: "0.25rem",
            "max-height": "16rem",
            "overflow-y": "auto",
          }}
        >
          <For each={history()}>
            {(ev) =>
              ev.kind === "application" ? (
                <li
                  style={{
                    padding: "0.4rem 0.6rem",
                    background: "var(--surface-elevated)",
                    border: "1px solid var(--surface-border)",
                    "border-radius": "var(--radius-sm)",
                  }}
                >
                  <div style={{ "white-space": "pre-wrap" }}>{ev.plaintext}</div>
                  <div
                    style={{
                      color: "var(--surface-muted)",
                      "font-size": "0.75rem",
                      "margin-top": "0.2rem",
                    }}
                  >
                    #{ev.messageId} · {ev.createdAt}
                  </div>
                </li>
              ) : (
                <li
                  style={{
                    "font-size": "0.8rem",
                    color: "var(--surface-muted)",
                    "font-style": "italic",
                    padding: "0.2rem 0.6rem",
                  }}
                >
                  · {ev.systemKind} #{ev.messageId} ·
                </li>
              )
            }
          </For>
        </ol>
      </Show>
      <form
        onSubmit={onSend}
        style={{ display: "flex", gap: "0.5rem", "margin-top": "0.25rem" }}
      >
        <input
          type="text"
          placeholder="Message"
          autocomplete="off"
          value={input()}
          onInput={(e) => setInput(e.currentTarget.value)}
          disabled={busy()}
          style={{ flex: 1 }}
        />
        <button
          class="primary"
          type="submit"
          disabled={busy() || input().length === 0}
        >
          Send
        </button>
      </form>
    </div>
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

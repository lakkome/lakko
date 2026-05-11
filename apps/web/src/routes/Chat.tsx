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
import { createSignal, For, onMount, Show, type JSX } from "solid-js";
import { A, useNavigate, useParams } from "@solidjs/router";
import { useAuth } from "../auth/AuthContext";
import { base64Encode, base64UrlDecodeNoPad } from "../auth/account-id";
import { ChatClient } from "../chat/client";
import type { IncomingChatEvent } from "../chat/client";
import { useChats, type ActiveGroup } from "../chat/ChatContext";
import { paths } from "./paths";

/** A timeline entry — verified incoming events PLUS optimistic
 *  outbound bubbles that haven't been confirmed yet. */
type TimelineRow =
  | { kind: "incoming"; event: IncomingChatEvent }
  | {
      kind: "outgoing";
      /** Stable client-side id for `<For>` keying — distinct
       *  from any DS-assigned id. */
      tempId: number;
      plaintext: string;
      ack: "pending" | "sent";
      /** Filled when ack flips to "sent". */
      messageId?: number;
      createdAt?: string;
    };

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
  const chat = new ChatClient(undefined, {
    identityProvider: () => chats.identity(),
    onPersist: async () => {
      const s = auth.state();
      if (s.kind !== "session") return;
      await chats.persistSnapshot(s.account.accountId);
    },
  });

  const [rows, setRows] = createSignal<TimelineRow[]>([]);
  const [cursor, setCursor] = createSignal(0);
  const [input, setInput] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  let nextTempId = 1;

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
      const fresh = result.events
        .filter((e) => !incomingIds.has(e.messageId))
        .map(
          (e): TimelineRow => ({ kind: "incoming", event: e }),
        );
      if (fresh.length > 0) {
        setRows([...rows(), ...fresh]);
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

    // Optimistic outbound bubble.
    const tempId = nextTempId++;
    setRows([
      ...rows(),
      { kind: "outgoing", tempId, plaintext: text, ack: "pending" },
    ]);
    setInput("");

    try {
      const result = await chat.sendMessage(
        props.requireIdentity(),
        props.entry.group,
        new TextEncoder().encode(text),
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
      // Roll back the optimistic bubble + surface the error.
      setRows(
        rows().filter((r) => !(r.kind === "outgoing" && r.tempId === tempId)),
      );
      setError(formatError(err));
    }
  }

  // Pull the current backlog on mount so a fresh `/chat/:id`
  // visit shows whatever the peer has sent so far.
  onMount(() => {
    void refresh();
  });

  return (
    <div
      style={{
        display: "flex",
        "flex-direction": "column",
        gap: "1rem",
      }}
    >
      <header
        style={{
          display: "flex",
          "align-items": "center",
          "justify-content": "space-between",
          gap: "0.75rem",
        }}
      >
        <div>
          <h1 style={{ margin: 0 }}>{props.entry.label}</h1>
          <div
            style={{
              color: "var(--surface-muted)",
              "font-size": "0.9rem",
              "margin-top": "0.25rem",
            }}
          >
            {props.entry.group.memberCount} member
            {props.entry.group.memberCount === 1 ? "" : "s"} · seat #
            {props.entry.group.ownLeafIndex}
          </div>
        </div>
        <div style={{ display: "flex", gap: "0.5rem" }}>
          <button
            type="button"
            onClick={() => {
              void refresh();
            }}
            disabled={busy()}
          >
            {busy() ? "Refreshing…" : "Refresh"}
          </button>
          <button type="button" onClick={props.onLeave}>
            Back to Contacts
          </button>
        </div>
      </header>

      {error() !== null && <div class="form-error">{error()}</div>}

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
          <For each={rows()}>{(row) => <TimelineBubble row={row} />}</For>
        </ol>
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
          disabled={input().length === 0}
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

function TimelineBubble(props: { row: TimelineRow }): JSX.Element {
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
    return (
      <li
        style={{
          padding: "0.5rem 0.75rem",
          background: "var(--surface-elevated)",
          border: "1px solid var(--surface-border)",
          "border-radius": "var(--radius-md)",
          "align-self": "flex-start",
          "max-width": "75%",
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
          peer · #{ev.messageId} · {ev.createdAt}
        </div>
      </li>
    );
  }
  // outgoing
  const r = props.row;
  return (
    <li
      style={{
        padding: "0.5rem 0.75rem",
        background: "var(--accent)",
        color: "var(--accent-fg)",
        "border-radius": "var(--radius-md)",
        "align-self": "flex-end",
        "max-width": "75%",
        opacity: r.ack === "pending" ? 0.7 : 1,
      }}
    >
      <div style={{ "white-space": "pre-wrap" }}>{r.plaintext}</div>
      <div style={{ "font-size": "0.75rem", "margin-top": "0.2rem" }}>
        you ·{" "}
        {r.ack === "pending"
          ? "sending…"
          : `sent · #${r.messageId ?? "?"} · ${r.createdAt ?? ""}`}
      </div>
    </li>
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

function formatError(err: unknown): string {
  if (err instanceof Error) return err.message;
  return String(err);
}

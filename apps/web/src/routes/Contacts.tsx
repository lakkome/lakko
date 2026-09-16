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
 *      it automatically; there is no manual refresh. Each row has
 *      Accept / Decline buttons.
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
  onMount,
  Show,
  type JSX,
} from "solid-js";
import { useNavigate } from "@solidjs/router";
import { useAuth } from "../auth/AuthContext";
import {
  base64Decode,
  base64Encode,
  base64UrlDecodeNoPad,
  base64UrlEncodeNoPad,
} from "../auth/account-id";
import { Avatar } from "../ui/Avatar";
import { Icon } from "../ui/icons";
import CreateCallLinkPanel from "../calls/CreateCallLinkPanel";
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
import { recordConversationKind } from "../chat/conversation-kind-store";
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
    <Show
      when={identity() !== null}
      fallback={
        <article>
          <NeedsReauth />
        </article>
      }
    >
      <ConnectedContacts client={client} username={accountUsername() ?? ""} />
    </Show>
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
      setSendNotice(`Request sent to @${result.recipientUsername}.`);
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
      // docs/ux/client-redesign.md section 3 — a conversation started from
      // a contact is a chat, filed under Chats until a third person joins.
      {
        const s = auth.state();
        if (s.kind === "session") recordConversationKind(s.account.accountId, active.groupIdB64, "chat");
      }
      setSendNotice(`Chat started with @${result.recipientUsername}.`);
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
  const mailbox = useMailbox();
  // Errors from this route's own Accept path (group creation), shown in the
  // same slot as a failed poll.
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
        "This request came from an older version of Lakko. Ask them to update and send it again.",
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
      // docs/ux/client-redesign.md section 3 — an accepted contact's
      // conversation is a chat.
      {
        const s = auth.state();
        if (s.kind === "session") recordConversationKind(s.account.accountId, active.groupIdB64, "chat");
      }
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

  // ---- layout: the list pane + the detail / add pane ----------------
  const [selectedKey, setSelectedKey] = createSignal<string | null>(null);
  // The add menu's pick (docs/ux section 5.3): add by username, the invite
  // links sheet, or a call link. `null` = nothing open.
  const [adding, setAdding] = createSignal<"username" | "invite" | "call" | null>(null);
  const [addMenuOpen, setAddMenuOpen] = createSignal(false);
  const [filter, setFilter] = createSignal("");
  const selected = (): AcceptedContact | undefined =>
    accepted().find((c) => c.signaturePubkeyB64 === selectedKey());
  const filtered = (): AcceptedContact[] => {
    const q = filter().trim().toLowerCase();
    if (q.length === 0) return accepted();
    return accepted().filter((c) =>
      profiles.resolve(c.signaturePubkeyB64).listLabel.toLowerCase().includes(q),
    );
  };
  /** The 1x1 chat with this contact, if this device holds one: linked at
   *  accept, or found by its two-member roster. */
  const chatWith = (c: AcceptedContact): string | null => {
    if (c.groupIdB64 !== undefined) {
      return paths.chat(base64UrlEncodeNoPad(base64Decode(c.groupIdB64)));
    }
    const own = chats.identity();
    const ownB64 = own === null ? null : base64Encode(own.signaturePublicKey);
    for (const g of chats.groups()) {
      let keys: string[] = [];
      try {
        keys = (g.group.members() as Array<{ signaturePublicKey: Uint8Array }>).map((m) =>
          base64Encode(m.signaturePublicKey),
        );
      } catch {
        continue;
      }
      if (keys.length === 2 && keys.includes(c.signaturePubkeyB64) && keys.includes(ownB64 ?? "")) {
        return paths.chat(base64UrlEncodeNoPad(g.group.groupId));
      }
    }
    return null;
  };

  return (
    <div
      class="lk-split"
      data-open={selected() !== undefined || adding() !== null ? "true" : "false"}
    >
      <aside class="lk-pane" aria-label="Contacts">
        <div class="lk-pane-head">
          <h1>Contacts</h1>
          <div class="lk-menu-anchor">
            <button
              type="button"
              class="lk-icon-button accent"
              title="Add"
              aria-label="Add"
              aria-haspopup="menu"
              aria-expanded={addMenuOpen()}
              onClick={() => setAddMenuOpen(!addMenuOpen())}
            >
              <Icon name="plus" size={18} />
            </button>
            <Show when={addMenuOpen()}>
              <div class="lk-menu-backdrop" onClick={() => setAddMenuOpen(false)} />
              <div class="lk-menu" role="menu">
                {(
                  [
                    ["username", "Add by username", "user"],
                    ["invite", "Invite links", "link"],
                    ["call", "Create call link", "phone"],
                  ] as const
                ).map(([mode, label, icon]) => (
                  <button
                    type="button"
                    role="menuitem"
                    onClick={() => {
                      setAddMenuOpen(false);
                      setSelectedKey(null);
                      setAdding(mode);
                    }}
                  >
                    <Icon name={icon} size={18} />
                    {label}
                  </button>
                ))}
              </div>
            </Show>
          </div>
        </div>
        <div class="lk-search">
          <Icon name="search" size={18} />
          <input
            type="search"
            placeholder="Search"
            aria-label="Search contacts"
            value={filter()}
            onInput={(e) => setFilter(e.currentTarget.value)}
          />
        </div>
        <div class="lk-pane-body">
          <PendingRows
            pending={mailbox.pending}
            busy={mailbox.busy}
            error={() => acceptError() ?? mailbox.error()}
            onAccept={onAccept}
            onDecline={onDecline}
          />
          <Show
            when={accepted().length > 0}
            fallback={
              <Show when={mailbox.pending().length === 0}>
                <div class="lk-empty">
                  <Icon name="contacts" size={40} strokeWidth={1.4} />
                  <strong>No contacts yet</strong>
                  <span>Add someone by username, or share your invite link.</span>
                  <button type="button" class="primary" onClick={() => setAdding("username")}>
                    Add contact
                  </button>
                </div>
              </Show>
            }
          >
            <div class="lk-section-label">Contacts</div>
            <For each={filtered()}>
              {(c) => (
                <div
                  class="lk-row"
                  role="button"
                  tabIndex={0}
                  classList={{ selected: selectedKey() === c.signaturePubkeyB64 }}
                  onClick={() => {
                    setAdding(null);
                    setSelectedKey(c.signaturePubkeyB64);
                  }}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" || e.key === " ") {
                      e.preventDefault();
                      setAdding(null);
                      setSelectedKey(c.signaturePubkeyB64);
                    }
                  }}
                >
                  <Avatar
                    label={profiles.resolve(c.signaturePubkeyB64).listLabel}
                    id={c.signaturePubkeyB64}
                  />
                  <div class="lk-row-body">
                    <span class="lk-row-title">{profiles.resolve(c.signaturePubkeyB64).listLabel}</span>
                    {/* A second line only when there is something to say: the
                        person's own name under a local rename. The key
                        fingerprint stays in the detail's Security card. */}
                    <Show when={profiles.resolve(c.signaturePubkeyB64).theirName}>
                      {(their) => <span class="lk-row-sub">{their()}</span>}
                    </Show>
                  </div>
                </div>
              )}
            </For>
          </Show>
        </div>
      </aside>
      <div class="lk-main">
        <Show
          when={selected()}
          fallback={
            <article>
              <div class="lk-page lk-stack" style={{ "max-width": "560px" }}>
                <button
                  type="button"
                  class="ghost small lk-narrow-only"
                  style={{ "align-self": "flex-start" }}
                  onClick={() => setAdding(null)}
                >
                  <Icon name="back" size={16} />
                  Contacts
                </button>
                <Show when={adding() === "username" || adding() === null}>
                  <div class="lk-page-head">
                    <h1>Add a contact</h1>
                    <p>Requests are end-to-end encrypted; Lakko's servers cannot read them.</p>
                  </div>
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
                </Show>
                <Show when={adding() === "invite"}>
                  <div class="lk-page-head">
                    <h1>Invite links</h1>
                    <p>Anyone who opens one of your links can send you a contact request.</p>
                  </div>
                  <InvitePanel />
                  <ActiveInvitesPanel />
                </Show>
                <Show when={adding() === "call"}>
                  <div class="lk-page-head">
                    <h1>Call link</h1>
                    <p>A call anyone with the link can ask to join. You host it.</p>
                  </div>
                  <CreateCallLinkPanel />
                </Show>
              </div>
            </article>
          }
        >
          {(c) => (
            <ContactDetail
              contact={c()}
              chatHref={chatWith(c())}
              myPubkey={() => {
                const id = chats.identity();
                return id?.signaturePublicKey ?? null;
              }}
              onBack={() => setSelectedKey(null)}
              onRemove={() => {
                onRemoveAccepted(c());
                setSelectedKey(null);
              }}
              onRename={(name) => onRenameAccepted(c(), name)}
            />
          )}
        </Show>
      </div>
    </div>
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
    <section class="lk-card">
      <h2>Add by username</h2>
      <form onSubmit={props.onSubmit} class="lk-stack" style={{ gap: "12px" }}>
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
        setError("Link revoked.");
      } else {
        // 404 — already gone or someone else's blob. Either
        // way, wipe the panel.
        reset();
        setError("This link no longer exists.");
      }
    } catch (err) {
      setError(formatError(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section class="lk-card">
      <h2>New invite link</h2>
      <p class="lk-card-desc">Share it with someone you want to hear from.</p>

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
            <span class="lk-meta" style={{ display: "block", "margin-top": "0.2rem" }}>
              Optional. Shown to the person who opens the link.
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
              <option value="never">Never</option>
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
            <span>For someone without a Lakko account yet</span>
          </label>

          {error() !== null && <div class="form-error">{error()}</div>}

          <button class="primary" type="submit" disabled={busy()}>
            {busy() ? "Creating…" : "Create link"}
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
 * Sits below [`InvitePanel`] in the Contacts add pane. Loaded when the
 * invite-links pane opens (one signed SELECT), never behind a button:
 * the pane is only mounted when the user asks for invite links, so the
 * timing side-channel ("Alice's discovery server sees an owner-list
 * query") stays tied to that gesture.
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
  // `null` = not loaded yet; `"loaded"` = the list has been read at least
  // once, so an empty list means "no links" rather than "not yet known".
  const [loadStatus, setLoadStatus] = createSignal<null | "loaded">(null);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [revokeBusySlug, setRevokeBusySlug] = createSignal<string | null>(null);

  onMount(() => {
    void load();
  });

  async function load(): Promise<void> {
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
          `"${row.label || "Invite link"}" had already been removed.`,
        );
      }
    } catch (err) {
      setError(formatError(err));
    } finally {
      setRevokeBusySlug(null);
    }
  }

  return (
    <section class="lk-card">
      <h2>
        Active links
        <Show when={loadStatus() === "loaded" && invites().length > 0}>
          <span class="lk-meta" style={{ "margin-left": "0.5rem", "font-weight": "normal" }}>
            ({invites().length})
          </span>
        </Show>
      </h2>
      <p class="lk-card-desc">
        Every link this account has created, on any of your devices.
      </p>

      <Show when={error() !== null}>
        <div class="form-error">{error()}</div>
      </Show>

      <Show
        when={loadStatus() === "loaded"}
        fallback={<p class="lk-muted">{busy() ? "Loading..." : ""}</p>}
      >
        <Show
          when={invites().length > 0}
          fallback={<p class="lk-muted">No active links.</p>}
        >
          <ul style={listStyle()}>
            <For each={invites()}>
              {(row) => (
                <li style={rowStyle()}>
                  <div style={{ flex: 1, "min-width": 0 }}>
                    <strong>{row.label || "Invite link"}</strong>
                    <div class="lk-meta" style={{ "margin-top": "0.2rem" }}>
                      {describeExpiryUnixMs(row.expiryMs)}
                      {" · "}
                      {describeUsage(row.usedCount, row.maxUses)}
                      <Show when={row.bootstrapReferral}>
                        {" · for a new account"}
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
  const canShare = (): boolean =>
    typeof navigator !== "undefined" && typeof navigator.share === "function";
  return (
    <div class="lk-stack" style={{ gap: "10px" }}>
      <div class="lk-link-row" role="group" aria-label="Invite link">
        <span class="lk-link-text" title={props.url}>
          {props.url.replace(/^https?:\/\//, "")}
        </span>
        <button
          type="button"
          class="lk-icon-button"
          title={props.copied() ? "Copied" : "Copy link"}
          aria-label={props.copied() ? "Copied" : "Copy link"}
          onClick={props.onCopy}
          disabled={props.busy()}
        >
          <Icon name={props.copied() ? "check" : "copy"} size={16} />
        </button>
        <Show when={canShare()}>
          <button
            type="button"
            class="lk-icon-button"
            title="Share"
            aria-label="Share"
            onClick={() => void navigator.share({ url: props.url }).catch(() => undefined)}
          >
            <Icon name="share" size={16} />
          </button>
        </Show>
      </div>
      <p class="lk-meta" style={{ margin: 0 }}>
        <Show
          when={props.kind === "pointer"}
          fallback="Lakko could not shorten this link, so it is long. It works the same way and cannot be revoked."
        >
          Anyone with the link can send you a contact request until it expires.
        </Show>
      </p>
      <div class="lk-actions">
        <button type="button" onClick={props.onReset} disabled={props.busy()}>
          Create another
        </button>
        <Show when={props.canRevoke}>
          <button
            type="button"
            class="ghost"
            onClick={props.onRevoke}
            disabled={props.busy()}
            style={{ "margin-left": "auto", color: "var(--danger)" }}
          >
            {props.busy() ? "Revoking..." : "Revoke"}
          </button>
        </Show>
      </div>
    </div>
  );
}

interface PendingRowsProps {
  readonly pending: () => readonly IncomingContactRequest[];
  readonly busy: () => boolean;
  readonly error: () => string | null;
  readonly onAccept: (r: IncomingContactRequest) => void;
  readonly onDecline: (r: IncomingContactRequest) => void;
}

/** Incoming contact requests at the top of the list pane, each with Accept /
 *  Ignore in place. */
function PendingRows(props: PendingRowsProps): JSX.Element {
  const profiles = useProfiles();
  return (
    <>
      {/* Requests arrive on their own (the mailbox long-poll); no refresh. */}
      <div class="lk-section-label">
        Requests
        <Show when={props.pending().length > 0}> ({props.pending().length})</Show>
      </div>
      <Show when={props.error() !== null}>
        <div class="form-error" style={{ padding: "0 12px 8px" }}>{props.error()}</div>
      </Show>
      <Show
        when={props.pending().length > 0}
        fallback={<div class="form-info" style={{ padding: "0 12px 8px" }}>No new requests.</div>}
      >
        <For each={props.pending()}>
          {(req) => (
            <div class="lk-row" style={{ "align-items": "flex-start" }}>
              <Avatar label={profiles.resolve(req.fromPubkey).listLabel} id={base64Encode(req.fromPubkey)} />
              <div class="lk-row-body">
                <span class="lk-row-title">{profiles.resolve(req.fromPubkey).listLabel}</span>
                <span class="lk-row-sub" style={{ "white-space": "normal" }}>
                  {req.intro !== null && req.intro !== "" ? req.intro : "wants to connect"}
                </span>
                <div class="lk-actions" style={{ "margin-top": "6px" }}>
                  <button
                    class="primary small"
                    type="button"
                    disabled={props.busy()}
                    onClick={() => {
                      void props.onAccept(req);
                    }}
                  >
                    Accept
                  </button>
                  <button class="small" type="button" onClick={() => props.onDecline(req)}>
                    Ignore
                  </button>
                </div>
              </div>
            </div>
          )}
        </For>
      </Show>
    </>
  );
}

interface ContactDetailProps {
  readonly contact: AcceptedContact;
  /** The route of the 1x1 chat with this contact, or `null` when this device
   *  holds none (the Message button is then not offered). */
  readonly chatHref: string | null;
  readonly myPubkey: () => Uint8Array | null;
  readonly onBack: () => void;
  readonly onRemove: () => void;
  /** Set this contact's display name; a blank value resets to the
   *  key-fingerprint default (ADR-0021 follow-up). */
  readonly onRename: (newName: string) => void;
}

/** The selected contact: name, actions, the safety number (Phase 1.9b3,
 *  ADR-0005 §13.5, computed lazily on first reveal), rename and remove. */
function ContactDetail(props: ContactDetailProps): JSX.Element {
  const profiles = useProfiles();
  const navigate = useNavigate();
  const resolved = (): ReturnType<typeof profiles.resolve> =>
    profiles.resolve(props.contact.signaturePubkeyB64);
  const [open, setOpen] = createSignal(false);
  const [number, setNumber] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);
  const [err, setErr] = createSignal<string | null>(null);
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
      setErr("Sign in again to compute the safety number.");
      return;
    }
    setBusy(true);
    setErr(null);
    try {
      const their = base64Decode(props.contact.signaturePubkeyB64);
      setNumber(await computeSafetyNumber(my, their));
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  async function toggle(): Promise<void> {
    const next = !open();
    setOpen(next);
    if (next) await ensureNumber();
  }

  return (
    <article>
      <div class="lk-page lk-stack" style={{ "max-width": "560px" }}>
        <button
          type="button"
          class="ghost small lk-narrow-only"
          style={{ "align-self": "flex-start" }}
          onClick={props.onBack}
        >
          <Icon name="back" size={16} />
          Contacts
        </button>
        <div style={{ display: "flex", "flex-direction": "column", "align-items": "center", gap: "12px" }}>
          <Avatar label={resolved().listLabel} id={props.contact.signaturePubkeyB64} size="xl" />
          <Show
            when={editing()}
            fallback={
              <div style={{ "text-align": "center" }}>
                <h1 style={{ margin: 0 }}>{resolved().listLabel}</h1>
                <Show when={resolved().theirName}>
                  {(their) => <div class="form-info">Calls themselves {their()}</div>}
                </Show>
                <Show when={resolved().usernameNotVerified}>
                  <div class="form-info">Username not verified</div>
                </Show>
              </div>
            }
          >
            <div class="lk-actions" style={{ "flex-wrap": "nowrap", width: "100%" }}>
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
                style={{ flex: 1 }}
              />
              <button type="button" class="primary" onClick={saveEdit}>
                Save
              </button>
              <button type="button" class="ghost" onClick={() => setEditing(false)}>
                Cancel
              </button>
            </div>
          </Show>
          <div class="lk-actions">
            <Show when={props.chatHref}>
              {(href) => (
                <button type="button" class="primary" onClick={() => navigate(href())}>
                  <Icon name="chat" size={18} />
                  Message
                </button>
              )}
            </Show>
            <button type="button" onClick={() => (editing() ? setEditing(false) : startEdit())}>
              <Icon name="compose" size={18} />
              Rename
            </button>
          </div>
        </div>

        <section class="lk-card">
          <h2>Security</h2>
          <div class="lk-card-row">
            <span class="lk-card-row-icon">
              <Icon name="shield" size={18} />
            </span>
            <div class="lk-card-row-body">
              <span class="lk-card-row-title">Safety number</span>
              <span class="lk-card-row-sub">
                Compare it with {resolved().name} in person or on a call. Matching
                numbers mean you are talking to each other and nobody in between.
              </span>
            </div>
            <button
              type="button"
              class="ghost small lk-card-row-action"
              style={{ color: "var(--accent)" }}
              disabled={busy()}
              onClick={() => {
                void toggle();
              }}
            >
              {open() ? "Hide" : "Show"}
            </button>
          </div>
          <Show when={open()}>
            <div
              style={{
                padding: "12px 14px",
                background: "var(--surface-sunken)",
                "border-radius": "var(--radius-sm)",
                "text-align": "center",
              }}
            >
              <Show
                when={number() !== null}
                fallback={<span class="form-info">{busy() ? "Computing…" : (err() ?? "")}</span>}
              >
                <code style={{ "font-size": "17px", "line-height": "1.6", background: "transparent", "letter-spacing": "0.04em" }}>
                  {number()}
                </code>
              </Show>
            </div>
          </Show>
          <div class="lk-card-row">
            <span class="lk-card-row-icon">
              <Icon name="check" size={18} />
            </span>
            <div class="lk-card-row-body">
              <span class="lk-card-row-title">Added</span>
              <span class="lk-card-row-sub">{new Date(props.contact.acceptedAtMs).toLocaleString()}</span>
            </div>
          </div>
        </section>

        <div style={{ display: "flex", "justify-content": "center" }}>
          <button type="button" class="danger" onClick={props.onRemove}>
            Remove contact
          </button>
        </div>
      </div>
    </article>
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

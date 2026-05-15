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
import { createMemo, createSignal, For, onMount, Show, type JSX } from "solid-js";
import { A, useNavigate, useParams } from "@solidjs/router";
import { useAuth } from "../auth/AuthContext";
import { base64Encode, base64UrlDecodeNoPad } from "../auth/account-id";
import { ChatClient } from "../chat/client";
import { ContactsClient } from "../contacts/client";
import { loadAccepted } from "../contacts/local-store";
import type { IncomingChatEvent } from "../chat/client";
import {
  composeEnvelope,
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
  const [input, setInput] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [notice, setNotice] = createSignal<string | null>(null);
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

  // Pull the current backlog on mount so a fresh `/chat/:id`
  // visit shows whatever the peer has sent so far.
  onMount(() => {
    void refresh();
  });

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
          <h1 style={{ margin: 0 }}>{props.entry.label}</h1>
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
}): JSX.Element {
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
        <Show when={ev.text.length > 0}>
          <div style={{ "white-space": "pre-wrap" }}>{ev.text}</div>
        </Show>
        <Show when={ev.attachments.length > 0}>
          <AttachmentChips
            attachments={ev.attachments}
            onDownload={props.onDownload}
            onPreview={props.onPreview}
            onHidePreview={props.onHidePreview}
            getCachedUrl={props.getCachedUrl}
            tone="incoming"
          />
        </Show>
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
      <Show when={r.plaintext.length > 0}>
        <div style={{ "white-space": "pre-wrap" }}>{r.plaintext}</div>
      </Show>
      <Show when={r.attachments.length > 0}>
        <AttachmentChips
          attachments={r.attachments}
          onDownload={props.onDownload}
          onPreview={props.onPreview}
          onHidePreview={props.onHidePreview}
          getCachedUrl={props.getCachedUrl}
          tone="outgoing"
        />
      </Show>
      <div style={{ "font-size": "0.75rem", "margin-top": "0.2rem" }}>
        you ·{" "}
        {r.ack === "pending"
          ? "sending…"
          : `sent · #${r.messageId ?? "?"} · ${r.createdAt ?? ""}`}
      </div>
    </li>
  );
}

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

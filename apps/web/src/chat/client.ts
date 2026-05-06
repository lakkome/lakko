// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `ChatClient` — Application-message orchestrator on top of
 * MLS groups (Phase 1.16e4).
 *
 * Two operations:
 *
 *   - `sendMessage(identity, group, plaintextBytes)` —
 *     `Group.createApplicationMessage` → POST to
 *     `/ds/group/{group_id_hex}/messages`. Returns the DS
 *     message id (also serves as the cursor for the next poll).
 *
 *   - `pollMessages(identity, group, after, waitSecs?)` —
 *     `DsClient.fetchGroup` → for each row,
 *     `group.processIncoming(identity, payload)` → if it's
 *     an application message, append the plaintext to the
 *     output. Commits / proposals advance group state but
 *     surface as `null` plaintext (the UI can render them as
 *     "system" rows or skip).
 *
 * The Group instance is held by the **caller** (typically by
 * the global `<ChatProvider>` registry from 1.16e5); this class
 * never owns it.
 *
 * ## Self-message echo
 *
 * `Group.createApplicationMessage` produces wire bytes that
 * look like a normal MLS application message — the DS does
 * not deduplicate, and the local poll WILL pick our own
 * messages back up. We drop the echo on receive by
 * remembering the DS ids of our own publishes during this
 * session. Cross-device (the same user sending from another
 * tab) is out of scope for 1.16e4 — the echo would surface
 * as a `Cannot decrypt own message` error inside OpenMLS,
 * because the sender's own application messages aren't
 * decryptable by the sender at the same epoch. We surface
 * those as warnings + skip.
 *
 * ## Canonical Identity (1.16e6c2)
 *
 * Every MLS operation must run against the **same**
 * `WasmIdentity` instance — the one whose OpenMLS provider
 * holds the live state for every Group on this device. The
 * `identityProvider` constructor hook is how the chat context
 * hands that canonical Identity in. When it returns `null`
 * (session restored from IDB without a fresh password) the
 * methods throw `ChatClientArgError` — the route gates re-auth
 * before reaching here.
 *
 * Pre-1.16e6c2 fallback: if `identityProvider` is unset (tests,
 * direct callers without a ChatProvider), each call rebuilds an
 * `Identity.fromSeed(secrets.exportKey)` and discards it. That
 * still works for stateless ops but means provider-storage
 * mutations vanish at the end of each call. Production paths
 * MUST wire the canonical accessor.
 *
 * ## Persistence (1.16e6c2)
 *
 * After every successful state-mutating call we fire the
 * optional `onPersist` callback so the on-disk MLS snapshot
 * stays in sync with the canonical Identity's provider. Failures
 * log-and-continue — the in-memory state is still good and the
 * next mutation will retry the persist naturally.
 */
import { DsClient } from "../ds/client";
import { loadWasm } from "../wasm";
import type {
  Group as WasmGroup,
  Identity as WasmIdentity,
} from "../../wasm/lakko_ffi_wasm";
import type { IdentitySecrets } from "../auth/AuthContext";

/** Result of `sendMessage`. */
export interface SendMessageResult {
  /** DS-assigned monotonic id (also the cursor value). */
  messageId: number;
  /** ISO-8601 timestamp from the DS. */
  createdAt: string;
}

/** A decoded application message returned by `pollMessages`.
 *  `Commit` and `Proposal` rows advance group state but don't
 *  carry user-visible bytes; we surface them as
 *  `kind: "system"` so the UI can render a thin separator if
 *  it wants. */
export type IncomingChatEvent =
  | {
      kind: "application";
      messageId: number;
      plaintext: string;
      createdAt: string;
    }
  | {
      kind: "system";
      messageId: number;
      /** The MLS frame kind: "commit" / "proposal" /
       *  "external-join-proposal". Lower-case for parity with
       *  `ProcessedMessageKind` strings on the Rust side. */
      systemKind: "commit" | "proposal" | "external-join-proposal";
      createdAt: string;
    };

/** Result of `pollMessages`. */
export interface PollMessagesResult {
  events: IncomingChatEvent[];
  cursor: number;
  hasMore: boolean;
}

/** Programmer error caught before any network call. */
export class ChatClientArgError extends Error {
  override name = "ChatClientArgError";
}

/** Wrap any underlying failure (DS, MLS process, etc). */
export class ChatError extends Error {
  override name = "ChatError";
  constructor(message: string, public readonly cause?: unknown) {
    super(message);
  }
}

/** Optional wires the route layer hands to ChatClient. */
export interface ChatClientOptions {
  /** Returns the **canonical** `WasmIdentity` for this session.
   *  See `<ChatProvider>` (1.16e6c1). When unset, the client
   *  falls back to building a throwaway `Identity.fromSeed` per
   *  call — only acceptable for tests; production paths MUST
   *  wire this so MLS state stays coherent across calls. */
  identityProvider?: () => WasmIdentity | null;
  /** Fires after every successful MLS state-mutating call so
   *  the route layer can persist the canonical Identity's
   *  provider snapshot to disk. Best-effort — exceptions are
   *  logged and swallowed; the in-memory state stays good. */
  onPersist?: () => Promise<void>;
}

export class ChatClient {
  /** Track DS message ids we ourselves just published, so the
   *  next poll can drop the echo. Cleared per `ChatClient`
   *  instance; if the UI loses this list (e.g. swap clients)
   *  the echo surfaces as a `processIncoming` failure that
   *  we log-and-skip. */
  private readonly ownPublishedIds = new Set<number>();
  private readonly identityProvider: (() => WasmIdentity | null) | undefined;
  private readonly onPersist: (() => Promise<void>) | undefined;

  constructor(
    private readonly ds: DsClient = new DsClient(),
    options: ChatClientOptions = {},
  ) {
    this.identityProvider = options.identityProvider;
    this.onPersist = options.onPersist;
  }

  /**
   * Resolve the `WasmIdentity` to drive an MLS operation. When
   * a canonical accessor is wired (production), we hand back
   * its instance — same provider for every call, mutations
   * accumulate, snapshots capture the full state. When unset,
   * we fall back to deterministic `Identity.fromSeed` so tests
   * and standalone callers keep working.
   *
   * Throws `ChatClientArgError` if neither path produces an
   * Identity (i.e. canonical is wired but the user has no
   * fresh export key in scope) — the caller should surface a
   * "please re-enter your password" UI.
   */
  private async resolveIdentity(
    secrets: IdentitySecrets | null,
  ): Promise<WasmIdentity> {
    if (this.identityProvider !== undefined) {
      const canonical = this.identityProvider();
      if (canonical !== null) return canonical;
      throw new ChatClientArgError(
        "no canonical Identity in session — re-enter your password",
      );
    }
    if (secrets === null) {
      throw new ChatClientArgError("session has no Identity in memory");
    }
    const wasm = await loadWasm();
    return wasm.Identity.fromSeed(secrets.exportKey);
  }

  /** Run the persist hook, swallow + log failures. The
   *  in-memory MLS state is still good even if the snapshot
   *  write missed; the next mutation will retry. */
  private async persistBestEffort(): Promise<void> {
    if (this.onPersist === undefined) return;
    try {
      await this.onPersist();
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("ChatClient: MLS snapshot persist failed", err);
    }
  }

  /**
   * Encrypt `plaintext` for the current group epoch and post
   * the wire bytes to the DS. Returns the DS-assigned id.
   *
   * Caller is responsible for keeping `group` alive — the
   * Group instance carries the MLS ratchet state. With
   * `identityProvider` wired (1.16e6c2), the underlying provider
   * storage is persisted to IDB after each successful publish.
   */
  async sendMessage(
    identity: IdentitySecrets | null,
    group: WasmGroup,
    plaintext: Uint8Array,
  ): Promise<SendMessageResult> {
    if (plaintext.length === 0) {
      throw new ChatClientArgError("plaintext must be non-empty");
    }
    const myIdentity = await this.resolveIdentity(identity);

    let wireBytes: Uint8Array;
    try {
      wireBytes = group.createApplicationMessage(myIdentity, plaintext);
    } catch (err) {
      throw new ChatError("createApplicationMessage failed", err);
    }

    let groupId: Uint8Array;
    try {
      groupId = group.groupId;
    } catch (err) {
      throw new ChatError("group.groupId unavailable", err);
    }

    let publish;
    try {
      publish = await this.ds.publishGroup(groupId, wireBytes);
    } catch (err) {
      throw new ChatError("DS publish failed", err);
    }
    this.ownPublishedIds.add(publish.id);

    // 1.16e6c2: createApplicationMessage advanced the message
    // secret tree on disk (well, in the canonical provider's
    // storage). Snapshot it.
    await this.persistBestEffort();

    return { messageId: publish.id, createdAt: publish.createdAt };
  }

  /**
   * Pull all rows newer than `after` for the group's journal,
   * decrypt application messages, return discriminated events
   * + a fresh cursor. Persists the MLS snapshot once at the
   * end if any rows were processed (each `processIncoming`
   * mutates provider storage — application messages drain
   * keys, commits replace state, proposals queue).
   */
  async pollMessages(
    identity: IdentitySecrets | null,
    group: WasmGroup,
    after: number,
    waitSecs = 0,
  ): Promise<PollMessagesResult> {
    const myIdentity = await this.resolveIdentity(identity);

    const groupId: Uint8Array = group.groupId;
    let raw;
    try {
      raw = await this.ds.fetchGroup(groupId, { after, waitSecs });
    } catch (err) {
      throw new ChatError("DS fetch failed", err);
    }

    const events: IncomingChatEvent[] = [];
    let cursor = after;
    let mutated = false;
    for (const message of raw.messages) {
      cursor = message.id;
      // Drop our own echoes — the DS doesn't dedupe and
      // OpenMLS can't decrypt them on the sender side at the
      // same epoch.
      if (this.ownPublishedIds.has(message.id)) {
        this.ownPublishedIds.delete(message.id);
        continue;
      }
      let processed;
      try {
        processed = group.processIncoming(myIdentity, message.payload);
      } catch (err) {
        // Bad frame, wrong epoch, malformed wire — log and
        // skip. Cursor still advances.
        // eslint-disable-next-line no-console
        console.warn("processIncoming failed", message.id, err);
        continue;
      }
      // processIncoming touched provider storage even when the
      // result is a no-op for the UI (e.g. an external commit
      // that we rejected). Track the mutation so the persist
      // hook still fires.
      mutated = true;
      events.push(toEvent(message.id, message.createdAt, processed));
    }

    if (mutated) {
      await this.persistBestEffort();
    }

    return { events, cursor, hasMore: raw.hasMore };
  }
}

function toEvent(
  messageId: number,
  createdAt: string,
  processed: ReturnType<WasmGroup["processIncoming"]>,
): IncomingChatEvent {
  // The WASM `ProcessedMessage.kind` is a PascalCase string
  // discriminator: "Application" | "Commit" | "Proposal" |
  // "ExternalJoinProposal" (see lakko-ffi-wasm/src/mls.rs).
  if (processed.kind === "Application" && processed.applicationData !== undefined) {
    return {
      kind: "application",
      messageId,
      plaintext: new TextDecoder("utf-8", { fatal: false }).decode(
        processed.applicationData,
      ),
      createdAt,
    };
  }
  const sk: "commit" | "proposal" | "external-join-proposal" =
    processed.kind === "Commit"
      ? "commit"
      : processed.kind === "Proposal"
        ? "proposal"
        : "external-join-proposal";
  return { kind: "system", messageId, systemKind: sk, createdAt };
}

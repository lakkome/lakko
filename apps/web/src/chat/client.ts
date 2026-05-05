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
 * The Group instance is held by the **caller** (typically
 * inline in `Contacts.tsx` for 1.16e4); this class never
 * owns it. Phase 1 doesn't persist MLS state, so dropping
 * the Group reference loses the chat on this device.
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
 */
import { DsClient } from "../ds/client";
import { loadWasm } from "../wasm";
import type { Group as WasmGroup } from "../../wasm/lakko_ffi_wasm";
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

export class ChatClient {
  /** Track DS message ids we ourselves just published, so the
   *  next poll can drop the echo. Cleared per `ChatClient`
   *  instance; if the UI loses this list (e.g. swap clients)
   *  the echo surfaces as a `processIncoming` failure that
   *  we log-and-skip. */
  private readonly ownPublishedIds = new Set<number>();

  constructor(private readonly ds: DsClient = new DsClient()) {}

  /**
   * Encrypt `plaintext` for the current group epoch and post
   * the wire bytes to the DS. Returns the DS-assigned id.
   *
   * Caller is responsible for keeping `group` alive — the
   * Group instance carries the MLS ratchet state. Phase 1.16e4
   * does not persist that state.
   */
  async sendMessage(
    identity: IdentitySecrets | null,
    group: WasmGroup,
    plaintext: Uint8Array,
  ): Promise<SendMessageResult> {
    if (identity === null) {
      throw new ChatClientArgError("session has no Identity in memory");
    }
    if (plaintext.length === 0) {
      throw new ChatClientArgError("plaintext must be non-empty");
    }

    const wasm = await loadWasm();
    const myIdentity = wasm.Identity.fromSeed(identity.exportKey);

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
    return { messageId: publish.id, createdAt: publish.createdAt };
  }

  /**
   * Pull all rows newer than `after` for the group's journal,
   * decrypt application messages, return discriminated events
   * + a fresh cursor.
   */
  async pollMessages(
    identity: IdentitySecrets | null,
    group: WasmGroup,
    after: number,
    waitSecs = 0,
  ): Promise<PollMessagesResult> {
    if (identity === null) {
      throw new ChatClientArgError("session has no Identity in memory");
    }

    const wasm = await loadWasm();
    const myIdentity = wasm.Identity.fromSeed(identity.exportKey);

    const groupId: Uint8Array = group.groupId;
    let raw;
    try {
      raw = await this.ds.fetchGroup(groupId, { after, waitSecs });
    } catch (err) {
      throw new ChatError("DS fetch failed", err);
    }

    const events: IncomingChatEvent[] = [];
    let cursor = after;
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
      events.push(toEvent(message.id, message.createdAt, processed));
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

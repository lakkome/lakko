// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0098 section 1 — the account-deleted statement, receive side.
 *
 * A deleting device posts one statement into every conversation it holds
 * (`auth/account-deletion-cleanups.ts`). It rides an ordinary MLS
 * application message, so the receive paths (the open chat's poll, the
 * background poll, the committer sweep) recognise it AHEAD of the chat
 * parser and hand it here with the account key the MLS sender resolved to at
 * receipt (C-68) and this conversation's group id. The runtime that owns the
 * per-person state installs the sink. Never a timeline row, never a
 * notification.
 *
 * Accepted only when the core's `verifyAccountDeletedEnvelope` answers
 * `true` for (plaintext, this group's id, the sender's account key): the
 * statement is signed by the very key the sender resolved to and is bound to
 * this conversation. Anything else is dropped and changes nothing.
 */

/** The wasm exports this needs, structurally. */
export interface AccountDeletedWasm {
  isAccountDeletedEnvelope(plaintext: Uint8Array): boolean;
  accountDeletedEnvelopeLen(): number;
  verifyAccountDeletedEnvelope(plaintext: Uint8Array, groupId: Uint8Array, senderAccountKey: Uint8Array): boolean;
}

/** First byte every `lakko/…` service-envelope domain starts with. */
const SERVICE_DOMAIN_FIRST_BYTE = 0x6c;

/** Whether `bytes` is an account-deleted statement (well-formed). Never
 *  throws; a chat message never pays for the core call (wrong length). */
export function isAccountDeletedBytes(wasm: AccountDeletedWasm, bytes: Uint8Array): boolean {
  if (bytes.length === 0 || bytes[0] !== SERVICE_DOMAIN_FIRST_BYTE) return false;
  try {
    if (bytes.length !== wasm.accountDeletedEnvelopeLen()) return false;
    return wasm.isAccountDeletedEnvelope(bytes);
  } catch {
    return false;
  }
}

/** Whether a statement is accepted. `false` for an unresolved sender. */
export function acceptAccountDeleted(
  wasm: AccountDeletedWasm,
  plaintext: Uint8Array,
  groupId: Uint8Array,
  senderAccountKey: Uint8Array | undefined,
): boolean {
  if (senderAccountKey === undefined || senderAccountKey.length !== 32) return false;
  try {
    return wasm.verifyAccountDeletedEnvelope(plaintext, groupId, senderAccountKey);
  } catch {
    return false;
  }
}

/** One statement as the receive path hands it on. */
export interface ArrivedAccountDeleted {
  readonly groupId: Uint8Array;
  readonly groupIdB64: string;
  readonly plaintext: Uint8Array;
  /** The account key the carrying message's MLS sender resolved to at
   *  receipt; `undefined` when it could not be resolved (dropped). */
  readonly senderAccountKey: Uint8Array | undefined;
}

export type AccountDeletedSink = (arrived: ArrivedAccountDeleted) => Promise<void>;

let sink: AccountDeletedSink | null = null;

/** Statements that arrived before a sink was installed, in memory. */
const waiting: ArrivedAccountDeleted[] = [];
const MAX_WAITING = 64;

/** Install (or, with `null`, remove) the handler the runtime owns. */
export function installAccountDeletedSink(next: AccountDeletedSink | null): void {
  sink = next;
  if (next === null) return;
  const queued = waiting.splice(0, waiting.length);
  for (const arrived of queued) void deliverAccountDeleted(arrived);
}

/** Hand one statement on. Never throws. */
export async function deliverAccountDeleted(arrived: ArrivedAccountDeleted): Promise<void> {
  const current = sink;
  if (current === null) {
    if (waiting.length >= MAX_WAITING) waiting.shift();
    waiting.push(arrived);
    return;
  }
  try {
    await current(arrived);
  } catch {
    // eslint-disable-next-line no-console
    console.warn("[person-status] a deletion statement could not be handled");
  }
}

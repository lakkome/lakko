// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0091 section 3 - recognise a `lakko-contacts::conversation_reentry`
 * envelope (82 to 4177 bytes, its own dispatch kind) without decoding it.
 * Pure over the wasm surface passed in; never throws.
 */

/** The envelope's length bounds (the core's, S2 as built). */
export const CONVERSATION_REENTRY_MIN_LEN = 82;
export const CONVERSATION_REENTRY_MAX_LEN = 4177;

/** The part of the wasm surface this needs, structurally. */
export interface ReentryEnvelopeWasm {
  readonly IncomingEnvelopeKind: { readonly ConversationReentry: number };
  dispatchIncomingEnvelope(bytes: Uint8Array): { readonly kind: number; free(): void };
}

/** Whether `bytes` is a conversation re-entry request envelope. */
export function isConversationReentryEnvelope(wasm: ReentryEnvelopeWasm, bytes: Uint8Array): boolean {
  if (bytes.length < CONVERSATION_REENTRY_MIN_LEN || bytes.length > CONVERSATION_REENTRY_MAX_LEN) return false;
  let dispatched;
  try {
    dispatched = wasm.dispatchIncomingEnvelope(bytes);
  } catch {
    return false;
  }
  try {
    return Number(dispatched.kind) === Number(wasm.IncomingEnvelopeKind.ConversationReentry);
  } finally {
    dispatched.free();
  }
}

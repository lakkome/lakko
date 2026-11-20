// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0098 section 2 — every answer of `GET /discovery/identity/{id}` this
 * device gets, whoever asked (the ADR-0016 verify gate, contact add, request
 * accept, the on-screen check), is handed to one observer, which records it
 * for the people this device holds under that account id. A `404` is handed
 * on as `null`; a network error or any other status is not an answer and is
 * not handed on.
 */

import type { IdentityFetchResponse } from "./wire";

export type IdentityAnswerObserver = (accountId: Uint8Array, bundle: IdentityFetchResponse | null) => void;

let observer: IdentityAnswerObserver | null = null;

/** Install (or, with `null`, remove) the observer the runtime owns. */
export function installIdentityAnswerObserver(next: IdentityAnswerObserver | null): void {
  observer = next;
}

/** Hand one answer on. Never throws. */
export function noteIdentityAnswer(accountId: Uint8Array, bundle: IdentityFetchResponse | null): void {
  try {
    observer?.(accountId, bundle);
  } catch {
    // Recording is best-effort; the caller's own use of the answer goes on.
  }
}

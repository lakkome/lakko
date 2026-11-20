// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A log tag for an error that carries no identifier (ADR-0091 phase 1b).
 *
 * An error's message is not safe to log: a DS error names its endpoint, and
 * the endpoint names the journal (`/ds/group/<hex>/...`). The tag keeps what
 * explains a failure and nothing that names a conversation, a row, a key or
 * an account: the error names along the `cause` chain, the HTTP status of a
 * DS refusal, and a typed membership refusal reason. Never the message, the
 * body or the endpoint.
 */

import { refusalReasonOf } from "./membership-refusal";

/** At most this many links of a `cause` chain are followed. */
const MAX_DEPTH = 6;

/** `Name[>Name...][ status=N][ refusal=reason]`, never the message text. */
export function errorLogTag(err: unknown): string {
  const names: string[] = [];
  let status: number | undefined;
  let current: unknown = err;
  for (let depth = 0; depth < MAX_DEPTH && current !== undefined && current !== null; depth += 1) {
    if (typeof current !== "object") {
      names.push(typeof current);
      break;
    }
    const record = current as { name?: unknown; status?: unknown; cause?: unknown; variant?: { cause?: unknown } };
    const name = typeof record.name === "string" && /^[A-Za-z][A-Za-z0-9_]{0,40}$/.test(record.name)
      ? record.name
      : "Error";
    names.push(name);
    if (status === undefined && typeof record.status === "number" && Number.isInteger(record.status)) {
      status = record.status;
    }
    current = record.cause ?? record.variant?.cause;
  }
  const reason = refusalReasonOf(err);
  return (
    (names.length === 0 ? "unknown" : names.join(">")) +
    (status === undefined ? "" : ` status=${status}`) +
    (reason === undefined ? "" : ` refusal=${reason}`)
  );
}

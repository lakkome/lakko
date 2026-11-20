// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Every refusal the panel API can answer, as one readable sentence on what
 * happened and one on what to do. A code the table does not know still reads
 * as a sentence, with the status, never as a bare code.
 */

/** An answer that was not 2xx. `code` is the `{"error"}` value, when there was one. */
export class ApiError extends Error {
  readonly status: number;
  readonly code: string | null;
  readonly body: unknown;

  constructor(status: number, code: string | null, body: unknown) {
    super(code ?? `status ${status}`);
    this.name = "ApiError";
    this.status = status;
    this.code = code;
    this.body = body;
  }
}

const MESSAGES: Readonly<Record<string, string>> = {
  "session-required": "You were signed out. Sign in again with your security key.",
  "unknown-challenge": "The sign-in request expired. Try again.",
  "not-a-moderator": "This security key isn't on the moderator list.",
  "bad-assertion": "The security key's answer didn't check out. Try again.",
  "bad-request": "The panel sent a request the service didn't understand. Reload the page and try again.",
  busy: "The service is busy. Wait a minute and try again.",
  "bad-status": "That status filter isn't known. Clear the filters and try again.",
  "bad-reason": "That reason isn't known. Choose one from the list.",
  "bad-kind": "That kind of target isn't known.",
  "bad-target": "That target isn't valid for its kind.",
  "unknown-case": "No report names this target.",
  locked: "Another moderator is reviewing this case.",
  "bad-receipt": "That report id isn't valid.",
  "bad-checks": "The message checks couldn't be saved.",
  "unknown-report": "That report no longer exists.",
  "bad-statement": "The signed decision couldn't be read. Nothing was published. Try again.",
  "not-your-statement": "The decision was signed by another moderator's key. Nothing was published.",
  "note-too-long": "The internal note is too long. Keep it under 2000 characters.",
  "reporting-unavailable": "Moderation isn't set up on this server. Nothing was published.",
  "not-authorised": "Your key isn't allowed to sign this decision. Nothing was published.",
  "reason-mismatch": "The reason didn't match the signed decision. Nothing was published. Try again.",
  "bad-list": "The decision names a moderator list the service doesn't hold. Reload the page and try again.",
  "wrong-service": "This decision can't be applied by any service. Nothing was published.",
  "stale-statement":
    "This decision was signed more than 24 hours ago, and no service accepts it any more. Nothing new was published. Sign a new decision.",
  "future-statement":
    "This computer's clock is more than 5 minutes ahead of the service's, so the decision was refused. Nothing was published. Set the clock right and try again.",
  "stale-list":
    "A newer moderator list is in force than the one this decision names. Nothing was published. Reload the page and sign the decision again.",
  "statement-id-conflict": "A different decision already uses this id. Nothing was published. Try again.",
  "apply-refused": "The decision is in the moderation log, but the service that applies it refused it.",
  "apply-failed": "The decision is in the moderation log, but it couldn't be applied yet.",
  "auth-failed": "The account service didn't answer. Try again in a minute.",
  "auth-unavailable": "The account service isn't set up on this server.",
  "bad-statement-id": "That decision id isn't valid.",
  "not-found": "The panel asked for something the service doesn't have. Reload the page.",
  "bad-blob-id": "This file's id isn't valid, so it can't be fetched.",
  "blob-not-found": "This file is gone: it expired, or its sender or a moderator deleted it.",
  "storage-unavailable": "Files can't be fetched: file storage isn't set up for the panel on this server.",
  "storage-failed": "File storage didn't answer, or the file is too large. Try again in a minute.",
};

/** One readable sentence for an error from a call or from the browser. */
export function errorMessage(e: unknown): string {
  if (e instanceof ApiError) {
    if (e.code !== null) {
      const known = MESSAGES[e.code];
      if (known !== undefined) return known;
      return `The service refused the request (${e.code}). Try again.`;
    }
    if (e.status === 0) return "Couldn't reach the moderation service. Check the connection and try again.";
    if (e.status >= 500) return `The moderation service failed (status ${e.status}). Try again in a minute.`;
    return `The moderation service refused the request (status ${e.status}). Reload the page and try again.`;
  }
  if (e instanceof DOMException) {
    if (e.name === "NotAllowedError") return "The security key wasn't touched, or the request was cancelled. Nothing was signed.";
    if (e.name === "InvalidStateError") return "This security key isn't registered for this panel.";
    if (e.name === "SecurityError") return "The browser refused the security key on this address. Open the panel at its own address.";
    return `The security key couldn't be used (${e.name}). Try again.`;
  }
  if (e instanceof Error && e.message !== "") return e.message;
  return "Something went wrong. Try again.";
}

/**
 * One readable sentence for an error while fetching an evidence file. The
 * service fetches only a few files at a time and answers `busy` beyond that.
 */
export function blobErrorMessage(e: unknown): string {
  if (e instanceof ApiError && e.status === 503 && e.code === "busy") {
    return "Too many files are being fetched right now. Wait a moment and try again.";
  }
  return errorMessage(e);
}

/** `true` for the answer that means the session is gone. */
export function isSignedOut(e: unknown): boolean {
  return e instanceof ApiError && e.status === 401 && e.code === "session-required";
}

/** What the panel was doing with the core when it failed. */
export type CoreContext = "report" | "appeal" | "statement";

/**
 * One readable sentence for an error the wasm core threw. The core's message
 * is `"<code>: <reason>"`; only the code decides the sentence.
 */
export function coreErrorMessage(e: unknown, context: CoreContext): string {
  const raw = e instanceof Error ? e.message : typeof e === "string" ? e : "";
  const code = raw.split(":", 1)[0]?.trim() ?? "";
  const what = context === "report" ? "report" : context === "appeal" ? "appeal" : "decision";
  switch (code) {
    case "cannot-open":
      return `This ${what} wasn't sealed to your key, so it can't be opened here. Another moderator can open it.`;
    case "malformed":
    case "wrong-domain":
    case "unknown-version":
    case "unknown-value":
      return `This ${what} is damaged or in a format the panel doesn't read.`;
    case "invalid":
      return context === "statement"
        ? "The decision couldn't be prepared. Reload the page and try again."
        : `This ${what} couldn't be opened with your key. Sign out and sign in again.`;
    case "bad-signature":
    case "assertion":
      return "The security key's signature didn't check out. Nothing was published. Try again.";
    case "not-authorised":
      return "Your key isn't allowed to do this. Nothing was published.";
    default:
      return `The ${what} couldn't be processed. Reload the page and try again.`;
  }
}

// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Sending an edit of one's own message (ADR-0023; base scenario 10).
 *
 * The edited text is drawn at once, as a sent message appears at once: the
 * person pressed Save and the bubble says what they wrote. The edit then goes
 * out; when it cannot, the message says `Not sent` with a retry, and the text
 * stays as edited - nothing is reverted behind the person's back. The order
 * is here, outside the view, so a test can hold the publish open and look.
 */

/** What sending an edit needs from the conversation view. */
export interface EditSendPort {
  /** Draw `text` as the message's text now; `opId` is the edit's own DS id
   *  once published (the version a translation is keyed on). */
  apply(target: number, text: string, opId: number | undefined): void;
  /** The text the message is drawn with now (the latest edit), if edited. */
  drawnText(target: number): string | undefined;
  /** Publish the edit; the edit's DS id, or `undefined` when it is applied
   *  here and never sent (a superseded one-to-one, ADR-0073 R.7). */
  publish(target: number, text: string): Promise<number | undefined>;
  /** The edit did not go out: the message says so. */
  markNotSent(target: number, text: string): void;
  /** The message's edit is no longer unsent. */
  clearNotSent(target: number): void;
}

/** Draw the edit, then send it. Resolves `true` once it went out. */
export async function sendEdit(port: EditSendPort, target: number, text: string): Promise<boolean> {
  port.apply(target, text, undefined);
  port.clearNotSent(target);
  try {
    const opId = await port.publish(target, text);
    // A later edit of the same message may be drawn by now; this one's id
    // belongs to this text only.
    if (opId !== undefined && port.drawnText(target) === text) port.apply(target, text, opId);
    return true;
  } catch {
    // Only the edit still drawn is reported: a newer one has its own send.
    if (port.drawnText(target) === text) port.markNotSent(target, text);
    return false;
  }
}

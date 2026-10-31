// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * When a synced draft may fill the composer.
 *
 * A draft from another device fills an empty composer once, when the chat
 * opens. It must not do so after the person has touched this composer: a draft
 * this device published comes back through the DeviceGroup, and on a slow path
 * it lands after the message was sent, when the composer is empty again, which
 * put the sent text back in the field.
 */
export interface DraftSeed {
  /** The person typed in or sent from this composer. */
  touched(): void;
  /** The text to put in the composer for this draft, or `null` to leave it. */
  offer(draftText: string, composerText: string): string | null;
}

export function createDraftSeed(): DraftSeed {
  let done = false;
  return {
    touched() {
      done = true;
    },
    offer(draftText, composerText) {
      if (done) return null;
      done = true;
      return composerText === "" ? draftText : null;
    },
  };
}

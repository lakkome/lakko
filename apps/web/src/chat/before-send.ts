// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0048 §2 — what must leave this device before its next message in a
 * group. A newcomer reads a member's messages under that member's name, so a
 * profile this device owes the group (a growth republish waiting out its
 * jitter, a failed join publish waiting for its retry) is published before the
 * message, never after it.
 *
 * The profile layer installs the hook (`ProfileContext.tsx`); the chat client
 * runs it before it takes the group's lock to seal a message
 * (`ChatClient.sendMessage`). Control envelopes (`broadcastControl`) do not
 * run it: the profile itself is one of them.
 *
 * The wait is bounded ({@link BEFORE_SEND_MAX_WAIT_MS}, chosen, not
 * measured): a publish that hangs on the network must not hold a message
 * forever. A hook that throws is ignored. Pure apart from the one slot.
 */

/** Longest a send waits for what is owed before it. */
export const BEFORE_SEND_MAX_WAIT_MS = 15_000;

export type BeforeSendHook = (groupIdB64: string) => Promise<void>;

let installed: BeforeSendHook | null = null;

/** Install (or clear, with `null`) the hook. Returns the clear. */
export function installBeforeSend(hook: BeforeSendHook | null): () => void {
  installed = hook;
  return () => {
    if (installed === hook) installed = null;
  };
}

/** Run the hook for `groupIdB64`, bounded. Never rejects. */
export async function runBeforeSend(
  groupIdB64: string,
  maxWaitMs: number = BEFORE_SEND_MAX_WAIT_MS,
): Promise<void> {
  const hook = installed;
  if (hook === null) return;
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    await Promise.race([
      hook(groupIdB64).catch(() => undefined),
      new Promise<void>((resolve) => {
        timer = setTimeout(resolve, maxWaitMs);
      }),
    ]);
  } finally {
    if (timer !== undefined) clearTimeout(timer);
  }
}

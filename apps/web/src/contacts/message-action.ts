// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What Message does on a contact's card.
 *
 * A person in contacts can always be messaged when their account is known
 * (maintainer, 2026-11-05): with a chat on this device Message opens it,
 * with none it starts a new one-to-one and opens that. A contact whose
 * account is not known cannot be messaged from here and the card says so.
 * Nothing founds a chat by itself: only the person pressing Message does.
 */

/** `open`: the chat is on this device. `start`: no chat here, the account is
 *  known, Message founds one. `unavailable`: the account is not known. */
export type MessageAction = "open" | "start" | "unavailable";

export function messageAction(facts: {
  readonly hasChat: boolean;
  /** Whether the contact's account id is known. */
  readonly accountKnown: boolean;
}): MessageAction {
  if (facts.hasChat) return "open";
  return facts.accountKnown ? "start" : "unavailable";
}

/**
 * How Message starts a chat with a contact this device holds no chat with
 * (`docs/ux/who-can-find-me.md` section 6.4, one order everywhere):
 *
 * 1. `found`: a grant this device holds still reaches the person - the
 *    one-to-one is founded and its Welcome goes to their inbox, as before.
 * 2. `waiting`: a request to them already stands in Sent - nothing is sent
 *    again (one request per person until it is answered or removed).
 * 3. `group`: both are current members of a group that is not a one-to-one -
 *    the contact request goes inside it; with several, the one with the
 *    newest message.
 * 4. `cannot`: nothing is sent.
 */
export type StartRoute =
  | { readonly kind: "found" }
  | { readonly kind: "waiting" }
  | { readonly kind: "group"; readonly groupIdB64: string }
  | { readonly kind: "cannot" };

/** A group both people are current members of, with its newest message's
 *  time (ms; the group's creation on this device when it has none). */
export interface SharedGroup {
  readonly groupIdB64: string;
  readonly newestMs: number;
}

export function chooseStartRoute(facts: {
  readonly grantReaches: boolean;
  readonly requestWaiting: boolean;
  readonly sharedGroups: readonly SharedGroup[];
}): StartRoute {
  if (facts.grantReaches) return { kind: "found" };
  if (facts.requestWaiting) return { kind: "waiting" };
  let best: SharedGroup | null = null;
  for (const g of facts.sharedGroups) {
    if (best === null || g.newestMs > best.newestMs) best = g;
  }
  return best === null ? { kind: "cannot" } : { kind: "group", groupIdB64: best.groupIdB64 };
}

/** The line Message leaves on the card after a press. */
export type MessageNote = "request-sent" | "cannot-message";

/** What `runMessage` needs from the screen. */
export interface MessageDeps {
  /** The route of the chat with the contact on this device, or `null`. */
  readonly chatHref: string | null;
  /** Whether the contact's account id is known. */
  readonly accountKnown: boolean;
  /** Whether a grant this device holds still reaches the person. */
  readonly grantReaches: () => Promise<boolean>;
  /** Whether a request to the person stands in Sent. */
  readonly requestWaiting: () => boolean;
  /** The groups both people are current members of (not one-to-ones). */
  readonly sharedGroups: () => Promise<readonly SharedGroup[]>;
  /** Found the one-to-one with the contact and return its route. */
  readonly found: () => Promise<string>;
  /** Send the contact request inside `groupIdB64` and record it in Sent. */
  readonly sendInGroup: (groupIdB64: string) => Promise<void>;
  readonly navigate: (href: string) => void;
  /** Progress on the button; `true` while a chat is founded or a request
   *  sent. */
  readonly setBusy: (busy: boolean) => void;
  /** The failure line, `null` to clear it: `start` for a chat that could not
   *  be founded (`contacts.error.start_chat`), `request` for a request that
   *  could not be sent (`contacts.detail.request_failed`). */
  readonly setError: (err: unknown | null, what?: "start" | "request") => void;
  /** The line under Message after a press, `null` to clear it. */
  readonly setNote?: (note: MessageNote | null) => void;
  /** Logs a failure (plain text, no identifiers), whether or not the screen
   *  still shows this contact. */
  readonly logFailure?: (err: unknown) => void;
  /** Whether the screen still shows this contact; a result for another
   *  contact is not shown, though what was done is kept either way. */
  readonly stillOn: () => boolean;
}

/** Message pressed: open the chat, found it, or send a contact request in a
 *  shared group. A failure leaves the button usable with the error shown. */
export async function runMessage(deps: MessageDeps): Promise<void> {
  switch (messageAction({ hasChat: deps.chatHref !== null, accountKnown: deps.accountKnown })) {
    case "open":
      if (deps.chatHref !== null) deps.navigate(deps.chatHref);
      return;
    case "unavailable":
      return;
    case "start":
      break;
  }
  deps.setError(null);
  deps.setNote?.(null);
  deps.setBusy(true);
  let what: "start" | "request" = "start";
  try {
    const route = chooseStartRoute({
      grantReaches: await deps.grantReaches(),
      requestWaiting: deps.requestWaiting(),
      sharedGroups: await deps.sharedGroups(),
    });
    switch (route.kind) {
      case "found": {
        const href = await deps.found();
        if (deps.stillOn()) deps.navigate(href);
        break;
      }
      case "waiting":
        if (deps.stillOn()) deps.setNote?.("request-sent");
        break;
      case "group":
        what = "request";
        await deps.sendInGroup(route.groupIdB64);
        if (deps.stillOn()) deps.setNote?.("request-sent");
        break;
      case "cannot":
        if (deps.stillOn()) deps.setNote?.("cannot-message");
        break;
    }
  } catch (err) {
    deps.logFailure?.(err);
    if (deps.stillOn()) deps.setError(err, what);
  } finally {
    if (deps.stillOn()) deps.setBusy(false);
  }
}

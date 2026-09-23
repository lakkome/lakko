// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The shared presentation rules, reached from the web client.
 *
 * `docs/ux/navigation.md` section 8. The rules that decide what a person sees
 * - the order of the one conversation list, whether a typed string is worth a
 * network lookup - live in `crates/lakko-ui-rules` and are reached here over
 * wasm, on iOS and Android over UniFFI. One copy, three clients: three
 * hand-written translations of one paragraph drift, and the drift is only
 * found when someone notices the clients disagreeing.
 *
 * The rules are a strict subset of the wasm namespace rather than a wrapper
 * around it, so a caller cannot accidentally reach past them into something
 * that is not a rule, and so a test can hand in a fake without standing up
 * wasm (vitest has no wasm here).
 */
import { createSignal } from "solid-js";

import { loadWasm, type Wasm } from "../wasm";

/** What the client is allowed to ask the shared rules. */
export type UiRules = Pick<
  Wasm,
  // The one conversation list (`docs/ux/navigation.md` section 2).
  | "ConversationKind"
  | "ConversationRow"
  | "orderConversations"
  | "bareName"
  | "looksLikeAName"
  // What a conversation's name opens, and what is on it
  // (`docs/ux/conversation-info.md`).
  | "InfoTarget"
  | "InfoOrigin"
  | "PersonInfoCapabilities"
  | "GroupInfoCapabilities"
  | "ChannelInfoCapabilities"
  | "infoTarget"
  | "personInfoSections"
  | "groupInfoSections"
  | "channelInfoSections"
>;

const [rules, setRules] = createSignal<UiRules | null>(null);

// Primed once, at import. In the running app this has long resolved before a
// conversation list renders - nothing reaches this module before the session
// exists, and a session needs wasm - but the signal is what makes the list
// correct rather than merely lucky: a caller that finds `null` renders
// nothing and re-renders when the rules arrive, instead of falling back to a
// second, drifting copy of the rule.
void loadWasm().then((wasm) => {
  setRules(() => wasm);
});

/**
 * The shared rules, or `null` until wasm has loaded.
 *
 * A Solid signal: read it inside a memo or the JSX and the caller re-renders
 * on its own when the rules arrive.
 */
export function uiRules(): UiRules | null {
  return rules();
}

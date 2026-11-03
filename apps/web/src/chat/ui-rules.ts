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
  | "PersonInfoCapabilities"
  | "ChannelInfoCapabilities"
  | "infoTarget"
  | "personInfoSections"
  | "conversationInfoSections"
  | "channelInfoSections"
  // Whether an arriving call offer rings, and for how long
  // (`docs/ux/calls.md` section 2.1). The ring window is here because it is a
  // number two DIFFERENT devices have to agree on, and mute because no client
  // was reading it at all; `calls/ring-rule.ts` is the adapter the call paths
  // reach it through.
  | "RingDecision"
  | "ringDecision"
  | "remainingRingMs"
  | "callRingWindowMs"
  // Whether a call is HOLDING the microphone and camera, so the composer's
  // recorder must stand aside (`docs/ux/media-messages.md`). Here because
  // every client asked a different question - is there a call in this chat -
  // and the two part company for a ringing offer nobody has answered;
  // `calls/capture-rule.ts` is the adapter the composer reaches it through.
  | "callHoldsCapture"
  // Whether a live call is put away into the bar across the top of the app,
  // and what that bar's one status line says (`docs/ux/calls.md` section 4.5).
  // Here because the second decision is a PAIR - a kind and, only for one of
  // the kinds, a running time - and three clients each expressing it as three
  // independent booleans is one careless `if` from a line that says two
  // things; `calls/call-bar-model.ts` is the adapter the bar reaches it
  // through.
  | "CallBarPhase"
  | "callBarVisible"
  | "callBarStatusKind"
  | "callBarElapsedMs"
  // A group call's screen (`docs/ux/group-call-grid.md`): the grid's geometry
  // and who is on the stage, so the three clients draw the same thing on the
  // same width; and whether the outgoing tone plays at all, which every
  // client used to answer "yes" for a call nobody was being rung in.
  // `calls/group-call-layout.ts` is the adapter.
  | "ringbackPlays"
  | "callGrid"
  | "callStage"
  // Chat folders (`docs/ux/chat-folders.md`, ADR-0058): which conversations
  // a folder holds and in what order, and the name rule and id minting the
  // editor needs. `chat/folders/folder-view.ts` is the adapter.
  | "FolderRule"
  | "folderMatches"
  | "orderConversationsInFolder"
  | "isValidFolderName"
  | "mintFolderId"
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

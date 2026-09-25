// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Stand-ins for the two contexts `TimelineBubble` reads, and nothing else.
 *
 * This is the real-browser twin of the `vi.mock` at the top of
 * `src/chat/TimelineBubble.test.tsx`, and it is stood in for the same reason:
 * both are NAME RESOLVERS. `useProfiles()` turns a signature key into a
 * display name and `useChats().identity()` answers "is this row from another
 * device of mine". Reaching the real ones means an MLS identity, IndexedDB, a
 * discovery round trip and an OPAQUE session — none of which is in the frame
 * of a question about a `contextmenu` handler or a bubble's height.
 *
 * What is NOT stood in for is the part under test: the bubble, the menu, the
 * voice player, the recorder, the stylesheet and the browser are all the real
 * ones. A fixture that reproduced a component's markup would prove nothing,
 * so nothing here renders anything.
 *
 * Substituted by module id in `vite.config.ts`, so `src/` is untouched.
 */

/** The shape of `ProfileApi` that `TimelineBubble` actually calls. */
export function useProfiles(): {
  version: () => number;
  resolve: (key: Uint8Array | string) => { name: string };
} {
  return {
    version: () => 0,
    resolve: (key) => ({ name: typeof key === "string" ? key : "Member" }),
  };
}

/** The shape of `ChatApi` that `TimelineBubble` actually calls. `null` is the
 *  honest answer here: with no identity no row can be from our own device, so
 *  an incoming row renders as a peer's — which is the case under test. */
export function useChats(): { identity: () => null } {
  return { identity: () => null };
}

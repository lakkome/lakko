// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A value a mounted part of the screen keeps reading while it is taken down.
 *
 * The call screen draws a person under "is this person in the roster", a
 * picture under "is there a stream", and so on. Solid's `<Show>` hands its
 * child an accessor for the value it tested, and that accessor THROWS
 * ("Stale read from <Show>") when it is read after the condition turned
 * false. A child reads it at exactly that moment in two ordinary ways: in an
 * `onCleanup` (a tile telling the call it no longer shows a person), and in a
 * memo of its own that the same change made stale and that its cleanup then
 * reads.
 *
 * The throw does not stay local. It leaves the signal write that caused it
 * half applied: the new value is stored, every memo and effect that was
 * waiting to run is dropped, and those effects never run again, because Solid
 * queues an effect only when it is clean. Seen live on a three-person call:
 * one person's browser closed, the store dropped them, and their tile, the
 * header's count of three and the picture asked of the remaining person all
 * stayed as they were; when the call then ended, taking the screen down threw
 * at the same place, before the session was closed, so the camera and the
 * microphone stayed on.
 *
 * {@link lastDefined} is what such a child reads instead: the value while
 * there is one, and the last one there was afterwards. It never throws, so a
 * part of the screen that is on its way out can still say who it was showing.
 * Whether the part is drawn at all stays the business of the condition around
 * it; this only answers "what was it".
 *
 * Solid only: no DOM, no JSX.
 */

import { createMemo, type Accessor } from "solid-js";

/**
 * The last value `read` gave that was neither `undefined` nor `null`;
 * `undefined` until it gives one. Must be created inside the owner that
 * reads it (a component body), so it is disposed with it.
 */
export function lastDefined<T>(read: () => T | undefined | null): Accessor<T | undefined> {
  return createMemo<T | undefined>((prev) => read() ?? prev, undefined);
}

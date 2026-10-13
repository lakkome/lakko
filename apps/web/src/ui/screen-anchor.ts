// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Is the person still on the screen this request was started from?"
 *
 * A request that outlives its screen is ordinary: a forward is still
 * re-encrypting attachments, a chat is still being created, a handle is still
 * being resolved, and the person has already gone somewhere else. Nothing
 * cancels a promise, so the code after the `await` runs anyway, and the one
 * thing it must not do is act on the screen that is on display NOW — above
 * all it must not `navigate`, which would pull the person out of wherever
 * they went and into the result of something they walked away from.
 *
 * The durable half of such a request (the message was sent, the group
 * exists, the row is in the log) still stands and must still be recorded.
 * Only the part addressed to the abandoned screen is dropped.
 *
 * ## Why the path, and not "is the component mounted"
 *
 * Two shapes defeat a mounted flag:
 *
 *   - a component that STAYS mounted while what it shows changes (the
 *     contacts list with a selected person, the channels layout), where the
 *     person can leave the item without unmounting anything;
 *   - a request whose own success unmounts its component (deleting the open
 *     chat removes the registry row the view is keyed on), where the
 *     follow-up navigation is exactly what is wanted although the component
 *     is gone.
 *
 * The router's location answers both: it is one object for the whole app,
 * readable after any component is disposed, and it changes exactly when the
 * person goes somewhere else. Only the pathname is compared — a query
 * (`?at=`, `?call=`) or a hash change is the same screen.
 */

/** The slice of the router's location this module reads. */
export interface PathSource {
  readonly pathname: string;
}

/**
 * Remember where the person is now. The returned function answers whether
 * they are still there.
 *
 * Call it synchronously when the request starts (or when the screen is
 * built, for a screen whose path never changes while it is up), and consult
 * the result after every `await` whose continuation would navigate or
 * otherwise address the screen.
 */
export function screenAnchor(location: PathSource): () => boolean {
  const startedAt = location.pathname;
  return () => location.pathname === startedAt;
}

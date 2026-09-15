// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The hand-off between the two receive paths for a group: the open chat view's
 * long poll (`routes/Chat.tsx`) and the background receive for every chat that
 * is not open (`BackgroundReceiveContext.tsx`).
 *
 * A group moves between the two whenever the user opens or leaves its chat, and
 * a background poll can still be on the network when that happens. Three
 * things have to survive the move, and each is held here, once, for the whole
 * signed-in session:
 *
 *   - **The journal cursor.** The chat view kept its cursor in a component
 *     signal, persisted to the message log on a 500 ms debounce, and hydrated it
 *     from the log on mount. If the background receive advanced a group and the
 *     log write had not landed yet, the view would start from the older value
 *     and re-fetch rows the MLS state had already consumed — every one of which
 *     fails to decrypt and is skipped, so they would vanish from the timeline.
 *     Both paths now read and advance ONE in-memory cursor per group, and the
 *     cursor only moves forward.
 *   - **Who polls.** A group whose chat view is mounted is polled by that view
 *     and excluded from the background sweep. Claims are reference-counted,
 *     because a route change can mount the new view before the old one's
 *     cleanup runs.
 *   - **Decrypted events.** MLS forward secrecy means a row, once applied,
 *     cannot be decrypted again. Everything the background receive applies and
 *     does not settle itself (edits, deletes, reactions, typing, the timeline
 *     rows) is delivered to the chat view if one is attached, or held until
 *     one attaches. Events delivered while a view is attached never touch the
 *     buffer, so a view that re-mounts never sees them twice.
 *
 * The cursor rule is the C-67 direction the chat journals already follow
 * (`durable-cursor.ts`): `ChatClient.pollMessages` advances this cursor only
 * after the MLS snapshot is durable, so it may lag the durable state and never
 * lead it.
 */

/** Events retained per group while no chat view is attached. */
export const MAX_BUFFERED_EVENTS_PER_GROUP = 1_000;

export class ReceiveCoordinator<E> {
  private readonly cursors = new Map<string, number>();
  private readonly claims = new Map<string, number>();
  private readonly buffers = new Map<string, E[]>();
  private readonly views = new Map<string, (events: readonly E[]) => void>();
  private readonly listeners = new Set<() => void>();
  private readonly pollRequests = new Map<string, () => void>();

  // ----- cursor ---------------------------------------------------------

  /** The furthest cursor either path has recorded for `groupId`, or
   *  `undefined` when neither has yet. */
  cursor(groupId: string): number | undefined {
    return this.cursors.get(groupId);
  }

  /** Record a cursor. Never moves backward; returns the resulting value. */
  noteCursor(groupId: string, cursor: number): number {
    const prev = this.cursors.get(groupId);
    if (!Number.isSafeInteger(cursor) || cursor < 0) return prev ?? 0;
    const next = prev === undefined ? cursor : Math.max(prev, cursor);
    this.cursors.set(groupId, next);
    return next;
  }

  // ----- foreground claims ------------------------------------------------

  /** A chat view for `groupId` is mounted. Returns an idempotent release. */
  claimForeground(groupId: string): () => void {
    this.claims.set(groupId, (this.claims.get(groupId) ?? 0) + 1);
    this.notify();
    let released = false;
    return () => {
      if (released) return;
      released = true;
      const n = (this.claims.get(groupId) ?? 1) - 1;
      if (n <= 0) this.claims.delete(groupId);
      else this.claims.set(groupId, n);
      this.notify();
    };
  }

  isForeground(groupId: string): boolean {
    return (this.claims.get(groupId) ?? 0) > 0;
  }

  /** Observe claim changes. Returns an unsubscribe. */
  onForegroundChange(listener: () => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  // ----- event hand-off ---------------------------------------------------

  /**
   * Hand events the background receive applied to the chat view for
   * `groupId`: straight to it when one is attached, else into the buffer (the
   * oldest dropped past {@link MAX_BUFFERED_EVENTS_PER_GROUP}).
   */
  deliver(groupId: string, events: readonly E[]): "delivered" | "buffered" | "empty" {
    if (events.length === 0) return "empty";
    const view = this.views.get(groupId);
    if (view !== undefined) {
      view(events);
      return "delivered";
    }
    const buffer = this.buffers.get(groupId) ?? [];
    buffer.push(...events);
    if (buffer.length > MAX_BUFFERED_EVENTS_PER_GROUP) {
      buffer.splice(0, buffer.length - MAX_BUFFERED_EVENTS_PER_GROUP);
    }
    this.buffers.set(groupId, buffer);
    return "buffered";
  }

  /**
   * Attach the chat view's consumer. Everything buffered for the group is
   * handed over first, in arrival order, and dropped from the buffer; later
   * deliveries go straight to `handler` until the returned detach runs.
   *
   * Attach AFTER the view has seeded its timeline from the persisted log: the
   * view's consumer de-duplicates against the rows it holds, and a buffer
   * drained into an empty timeline would stop the log seed from running.
   */
  attachView(groupId: string, handler: (events: readonly E[]) => void): () => void {
    this.views.set(groupId, handler);
    const buffered = this.buffers.get(groupId);
    if (buffered !== undefined && buffered.length > 0) {
      this.buffers.delete(groupId);
      handler(buffered);
    }
    return () => {
      if (this.views.get(groupId) === handler) this.views.delete(groupId);
    };
  }

  // ----- poll requests ----------------------------------------------------

  /**
   * Ask the chat view that holds `groupId`'s foreground claim to poll now.
   * The background receive must not sweep a foreground group (two sweepers
   * over two cursors re-feed consumed rows), but it can want one swept at
   * once — the ADR-0050 device fan-out publishes a proposal and the view's
   * long poll may be parked (tab hidden). Returns whether a view was listening;
   * a `false` means the view's own next poll picks it up.
   */
  requestPoll(groupId: string): boolean {
    const handler = this.pollRequests.get(groupId);
    if (handler === undefined) return false;
    handler();
    return true;
  }

  /** The chat view's side of {@link requestPoll}. Returns a detach. */
  onPollRequested(groupId: string, handler: () => void): () => void {
    this.pollRequests.set(groupId, handler);
    return () => {
      if (this.pollRequests.get(groupId) === handler) this.pollRequests.delete(groupId);
    };
  }

  /** Diagnostics + tests. */
  bufferedCount(groupId: string): number {
    return this.buffers.get(groupId)?.length ?? 0;
  }

  /** Sign-out: forget every cursor, claim, buffer and view. */
  reset(): void {
    this.cursors.clear();
    this.claims.clear();
    this.buffers.clear();
    this.views.clear();
    this.pollRequests.clear();
    this.notify();
  }

  private notify(): void {
    for (const l of [...this.listeners]) l();
  }
}

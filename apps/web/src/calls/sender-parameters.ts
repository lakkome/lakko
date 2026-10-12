// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * One queue of parameter writes per `RTCRtpSender`.
 *
 * A write to a sender's parameters is a read (`getParameters`), a change and
 * an asynchronous `setParameters`. Two of them on one sender must not
 * overlap: a browser rejects a `setParameters` made on a reading that
 * another call has outdated, and where it does not, the second write puts
 * back what the first had just changed (it read before the first landed).
 *
 * Several parts of the group call write a camera sender's parameters: its
 * frame-rate preference and temporal layers (`camera-sender.ts`), whether its
 * encoding is active (`sender-activity.ts`) and its bitrate ceiling
 * (`publish-budget.ts`). Each hands its whole read-change-set to
 * {@link queueParameterWrite}, so on any one sender they run one after the
 * other, in the order they were asked.
 *
 * No DOM type is needed: the sender is only the key.
 */

const pendingWrites = new WeakMap<object, Promise<void>>();

/**
 * Run `write` (one whole read-change-set of `sender`'s parameters) once
 * every write queued for that sender before it has settled, and resolve or
 * reject as `write` does.
 *
 * A sender with nothing pending runs `write` at once, in this same turn, so
 * a write made right after a sender was created reads it as it was created.
 * A failed write does not hold back the ones behind it.
 */
export function queueParameterWrite<T>(sender: object, write: () => Promise<T>): Promise<T> {
  const pending = pendingWrites.get(sender);
  let run: Promise<T>;
  if (pending === undefined) {
    try {
      run = write();
    } catch (err) {
      run = Promise.reject(err);
    }
  } else {
    run = pending.then(write);
  }
  const settled = run.then(
    () => {},
    () => {},
  );
  pendingWrites.set(sender, settled);
  void settled.then(() => {
    if (pendingWrites.get(sender) === settled) pendingWrites.delete(sender);
  });
  return run;
}

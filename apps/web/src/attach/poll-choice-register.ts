// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0092 section 3.6 "Own choice" — the person's own choice in a poll, as
 * the account's other devices learn it: a `lakko-sync` register in the
 * DeviceGroup, keyed on (conversation ref, poll message id), last writer
 * wins. Never read from a ballot (nobody, the voter included, can).
 *
 * `DeviceGroupSyncContext` owns the synced state and installs the port once it
 * is loaded; the poll screens read and write through it. The same module
 * shape as `channels/own-feedback-register.ts`.
 */
import { createSignal } from "solid-js";

/** The conversation ref kinds of the synced state (`1` one-to-one, `2`
 *  group, `3` channel). */
export type ChoiceRefKind = 1 | 2 | 3;

export interface OwnPollChoice {
  /** Empty = retracted. */
  readonly choices: readonly number[];
  /** Ring version of the account's anonymity set (0 for a channel poll). */
  readonly setVersion: number;
}

export interface PollChoicePort {
  readonly accountIdB64: string;
  choiceOf(refKind: ChoiceRefKind, refId: Uint8Array, pollMessageId: number): OwnPollChoice | undefined;
  /** Record a vote (or a retract) and send it to the other devices. A first
   *  vote passes no `setVersion`; a revote passes the recorded one. */
  setChoice(
    refKind: ChoiceRefKind,
    refId: Uint8Array,
    pollMessageId: number,
    choices: readonly number[],
    setVersion: number,
  ): Promise<boolean>;
}

const [version, setVersion] = createSignal(0);
let port: PollChoicePort | null = null;

/** Called by the sync context whenever the register may have changed. */
export function pollChoicesChanged(): void {
  setVersion((v) => v + 1);
}

export function installPollChoicePort(next: PollChoicePort): () => void {
  port = next;
  pollChoicesChanged();
  return () => {
    if (port === next) port = null;
    pollChoicesChanged();
  };
}

export function pollChoicePort(): PollChoicePort | null {
  return port;
}

/** The own choice, reactive; `undefined` when none is recorded or the synced
 *  state is not loaded yet. */
export function ownPollChoice(
  refKind: ChoiceRefKind,
  refId: Uint8Array,
  pollMessageId: number,
): OwnPollChoice | undefined {
  void version();
  if (port === null) return undefined;
  try {
    return port.choiceOf(refKind, refId, pollMessageId);
  } catch {
    return undefined;
  }
}

// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0090 — the app-wide part of the attachment kinds: binds the poll and
 * checklist store and the anonymous-ballot queue to the signed-in account.
 * Mounted once at the top of the app, like `BlobAccessRuntime`, so a queued
 * ballot leaves the device even when its conversation is not open (UX 7.5:
 * "The person may leave the conversation").
 *
 * "Never before a successful session landing" (ADR-0090 section 3.4a): the
 * queue posts nothing until this run's auth state reached `session`.
 */
import { createEffect, onCleanup, type JSX } from "solid-js";

import { useAuth } from "../auth/AuthContext";
import { base64Decode } from "../auth/account-id";
import { DsClient } from "../ds/client";
import { bindContentAccount } from "./content-store";
import { bindBallotQueue, secureRandom01, type QueuedBallot } from "./ballot-queue";
import { pollChoicePort } from "./poll-choice-register";

export function AttachRuntime(): JSX.Element {
  const auth = useAuth();
  let landed = false;

  createEffect(() => {
    const s = auth.state();
    const account = s.kind === "session" ? s.account.accountId : null;
    if (account !== null) landed = true;
    bindContentAccount(account);
    const ds = new DsClient();
    void bindBallotQueue(
      account,
      account === null
        ? null
        : {
            async post(groupIdB64: string, row: Uint8Array): Promise<number> {
              // A journal row proven by the epoch's access key (ADR-0053),
              // which every member holds: the proof names nobody. No wake.
              const r = await ds.publishGroup(base64Decode(groupIdB64), row);
              return r.id;
            },
            async recordChoice(q: QueuedBallot): Promise<void> {
              const port = pollChoicePort();
              if (port === null) return;
              // An anonymous poll exists only in a group (ref kind 2).
              await port.setChoice(2, base64Decode(q.groupIdB64), q.pollMessageId, q.choices, q.setVersion);
            },
            sessionLanded: () => landed,
            now: () => Date.now(),
            rand: secureRandom01,
          },
    );
  });
  onCleanup(() => {
    void bindBallotQueue(null, null);
    bindContentAccount(null);
  });
  return null;
}

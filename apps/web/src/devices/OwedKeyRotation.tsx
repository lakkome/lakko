// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Finish the key rotation this device still owes (ADR-0074 section 3.2): a
 * rotation whose commit landed and whose publishing steps (the directory,
 * the KeyPackage, the journal roots, the channel owner keys, the backup) did
 * not all get an answer is continued here at every landing and whenever the
 * connection returns, with nothing shown; the new words are withheld until
 * the record is gone.
 *
 * Also installs, for the session, the identity owner requests are signed
 * by: the account's current one once it rotated.
 *
 * Renders nothing. Whole-session, beside `OwedDeviceRemovals`.
 */
import { createEffect, onCleanup, type JSX } from "solid-js";

import { useAuth } from "../auth/AuthContext";
import { installAccountIdentities, proofIdentitiesOf } from "../channels/api";
import { useChats } from "../chat/ChatContext";
import { useKeyRotation } from "./key-rotation-runtime";

/** After a landing the chats, the channels and the DeviceGroup handle are
 *  given a moment to load before the owed steps run. Chosen. */
const RESUME_DELAY_MS = 8_000;
/** The longest pause between two tries of a step that keeps failing. */
const RESUME_MAX_DELAY_MS = 10 * 60_000;

export function OwedKeyRotation(): JSX.Element {
  const auth = useAuth();
  const chats = useChats();
  const rotation = useKeyRotation();
  let timer: ReturnType<typeof setTimeout> | null = null;

  // 2026-11-20: owner proofs by the CURRENT identity, as iOS and Android sign
  // them (ADR-0074 section 3.5: the owner key moves to the new seed's); before,
  // only the channels this device itself had handed over were, so a web device
  // and a phone of one account signed one channel with two keys and the
  // service refused one of them for good. A channel whose key did not move
  // yet is moved by the owner-key repair (`channel-owner-repair.ts`).
  // Co-publisher proofs by the lineage identity, the one the appointed binding
  // names. One mapping in `channels/api.ts` (`ownerProofIdentity`,
  // `coPublisherProofIdentity`).
  installAccountIdentities(() => proofIdentitiesOf(chats));
  onCleanup(() => installAccountIdentities(null));

  // Every run ends by telling the readers the record changed, which runs the
  // effect below again: a step that keeps failing is retried with a growing
  // pause, and the pause starts over whenever a step lands.
  let attempts = 0;
  let lastDone = -1;
  const schedule = (reset: boolean): void => {
    if (reset) attempts = 0;
    if (timer !== null) clearTimeout(timer);
    const delay = Math.min(RESUME_DELAY_MS * 2 ** attempts, RESUME_MAX_DELAY_MS);
    timer = setTimeout(() => {
      timer = null;
      attempts += 1;
      void rotation.resume();
    }, delay);
  };

  createEffect(() => {
    const landed = auth.state().kind === "session" && auth.identitySecrets() !== null;
    const record = rotation.record();
    if (!landed || auth.signedOut() !== null || record === null) return;
    const done = record.done.length;
    const progressed = done !== lastDone;
    lastDone = done;
    schedule(progressed);
  });

  const onOnline = (): void => {
    if (auth.state().kind === "session") schedule(true);
  };
  if (typeof window !== "undefined") {
    window.addEventListener("online", onOnline);
    onCleanup(() => window.removeEventListener("online", onOnline));
  }
  onCleanup(() => {
    if (timer !== null) clearTimeout(timer);
  });

  return null;
}

// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Finish the device removals this device still owes (ADR-0074 section 2.3).
 *
 * A removal whose device was signed out at the server but whose later steps
 * (the directory row, the DeviceGroup, the chats) did not all get an answer
 * leaves a record (`devices/removal.ts`). It is continued here at every
 * landing and whenever the connection returns, with nothing shown: the
 * Devices list shows `Removing...` on the row until the record is gone.
 *
 * Renders nothing. Whole-session, like the other runtimes in `App.tsx`:
 * the person is rarely on the Devices screen when the app next opens.
 */
import { createEffect, onCleanup, type JSX } from "solid-js";

import { useAuth } from "../auth/AuthContext";
import { useDeviceRemoval } from "./removal-runtime";

/** After a landing the chats and the DeviceGroup handle are given a moment
 *  to load before the owed steps run. Chosen. */
const RESUME_DELAY_MS = 5_000;

export function OwedDeviceRemovals(): JSX.Element {
  const auth = useAuth();
  const removal = useDeviceRemoval();
  let timer: ReturnType<typeof setTimeout> | null = null;

  const schedule = (): void => {
    if (timer !== null) clearTimeout(timer);
    timer = setTimeout(() => {
      timer = null;
      void removal.resume();
    }, RESUME_DELAY_MS);
  };

  // A landing: the session exists and its keys are in memory.
  createEffect(() => {
    const landed = auth.state().kind === "session" && auth.identitySecrets() !== null;
    if (landed && auth.signedOut() === null) schedule();
  });

  const onOnline = (): void => {
    if (auth.state().kind === "session") schedule();
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

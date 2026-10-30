// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/who-can-find-me.md` section 4.1 - the contacts of the account as the
 * "who can find me" list and the `Their contacts can find me` switch read them.
 * Must be called inside `<ProfileProvider>`.
 */
import { createMemo, createSignal, onCleanup, onMount, type Accessor } from "solid-js";

import { loadAccepted, subscribeAcceptedChanged } from "../contacts/local-store";
import { memberNameOf } from "../profile/member-label";
import { useProfiles } from "../profile/ProfileContext";
import { contactEntries, type ContactEntry } from "./who-can-find-me";

/** Reactive: the contacts that can be chosen. */
export function useFindableContacts(): Accessor<readonly ContactEntry[]> {
  const profiles = useProfiles();
  const [changed, setChanged] = createSignal(0);
  onMount(() => {
    const unsubscribe = subscribeAcceptedChanged(() => setChanged((n) => n + 1));
    onCleanup(unsubscribe);
  });
  return createMemo(() => {
    changed();
    return contactEntries(loadAccepted(), (c) => memberNameOf(profiles.resolve(c.signaturePubkeyB64)));
  });
}

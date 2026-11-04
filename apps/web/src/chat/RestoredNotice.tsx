// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What the chat list of a device restored from the phrase or a passkey says
 * until its first conversation arrives (`docs/ux/identity-recovery.md`
 * section 7.2): a card on the empty list when no other device of the account
 * is signed in, a banner while one is. Neither is a loading state, so neither
 * has a spinner. Both go for good with the first conversation.
 */
import { createEffect, createResource, onCleanup, Show, type Accessor, type JSX } from "solid-js";
import { useNavigate } from "@solidjs/router";

import { useAuth } from "../auth/AuthContext";
import { base64Decode } from "../auth/account-id";
import {
  clearRestoreMarker,
  isRestoreMarked,
  restoredListState,
  restoredMarkerVersion,
  type RestoredListState,
} from "../backup/restored-marker";
import { DiscoveryClient } from "../discovery/client";
import { t } from "../l10n";
import { paths } from "../routes/paths";
import { useChats } from "./ChatContext";
import { Icon } from "../ui/icons";

/**
 * Whether another device of the account is signed in: the directory lists a
 * device other than this one. `null` until that is known; a failed lookup
 * stays `null` and asks again when the device comes back online.
 */
async function otherDeviceListed(
  accountIdB64: string,
  sessionToken: string | null,
  ownDeviceKey: Uint8Array | null,
): Promise<boolean | null> {
  if (ownDeviceKey === null) return null;
  try {
    const discovery = new DiscoveryClient();
    discovery.setSessionToken(sessionToken);
    const listed = await discovery.fetchKeyPackages(base64Decode(accountIdB64));
    return (listed ?? []).some((row) => !sameKey(row.deviceId, ownDeviceKey));
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("restored list: device lookup failed", err instanceof Error ? err.name : "");
    return null;
  }
}

function sameKey(a: Uint8Array, b: Uint8Array): boolean {
  return a.length === b.length && a.every((v, i) => v === b[i]);
}

/** The restored state of the list. The marker goes the moment a conversation
 *  is there, and never comes back. */
export function useRestoredList(conversationCount: Accessor<number>): Accessor<RestoredListState> {
  const auth = useAuth();
  const chats = useChats();
  const account = (): string | null => {
    const s = auth.state();
    return s.kind === "session" ? s.account.accountId : null;
  };
  const marked = (): boolean => {
    restoredMarkerVersion();
    const id = account();
    return id !== null && isRestoreMarked(id);
  };
  const deviceKey = (): Uint8Array | null => chats.identity()?.signaturePublicKey ?? null;

  createEffect(() => {
    const id = account();
    if (id !== null && conversationCount() > 0 && marked()) clearRestoreMarker(id);
  });

  const [other, { refetch }] = createResource(
    () => (marked() ? { id: account() as string, key: deviceKey() } : null),
    ({ id, key }) => otherDeviceListed(id, auth.identitySecrets()?.sessionToken ?? null, key),
  );
  const onOnline = (): void => {
    if (marked() && (other() ?? null) === null) void refetch();
  };
  if (typeof window !== "undefined") window.addEventListener("online", onOnline);
  onCleanup(() => {
    if (typeof window !== "undefined") window.removeEventListener("online", onOnline);
  });

  return () =>
    restoredListState({
      marked: marked(),
      conversationCount: conversationCount(),
      otherDeviceSignedIn: other.loading ? null : (other() ?? null),
    });
}

export function RestoredNotice(props: { readonly state: "card" | "banner" }): JSX.Element {
  const navigate = useNavigate();
  return (
    <Show
      when={props.state === "card"}
      fallback={
        <div class="lk-alert" role="status" aria-live="polite">
          <div class="lk-fill">{t("chats.restored.banner_other_devices")}</div>
        </div>
      }
    >
      <div class="lk-empty">
        <Icon name="key" size={40} strokeWidth={1.4} />
        <span>{t("chats.restored.card")}</span>
        <button type="button" class="primary" onClick={() => navigate(paths.contacts)}>
          {t("nav.contacts")}
        </button>
      </div>
    </Show>
  );
}

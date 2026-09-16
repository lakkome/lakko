// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The owner's Manage screen (docs/ux/client-redesign.md section 5.4):
 * subscribers, co-publishers and the protection refresh.
 *
 * Opens the channel's persistent `PublisherGroup` **once** and shares the
 * single handle (plus the `Identity`) with `RotationPanel` and
 * `CoPublishersPanel`.
 *
 * **Why one handle (load-bearing).** `PublisherGroup.rotate()` +
 * `approveExternalJoin()` both advance the MLS epoch **durably** (persisted
 * in the OpenMLS provider). Two independent `openPublisherGroup` handles held
 * in the same screen would each cache a stale in-memory epoch — a rotation on
 * one would leave the other believing it is still at epoch N, and the next
 * commit on that stale handle would fork the group. Sharing one handle keeps
 * every commit path on the same epoch.
 *
 * **Fail-closed (invariant #4).** The panels only render once the handle is
 * open; a failed open surfaces the error and renders nothing — the cron never
 * starts and no "Refresh now" / "Create invite link" affordance appears, so
 * there is never a plaintext or bad-key path.
 *
 * **Stays mounted.** The channel view keeps this panel in the DOM while the
 * owner has the channel open and only hides it (`hidden`) when the Manage
 * screen is closed: the subscriber fan-out, the rotation heartbeat and the
 * co-publisher journal poll must keep running whether or not the owner is
 * looking at them.
 */
import { createSignal, onCleanup, onMount, Show, type JSX } from "solid-js";

import { openPublisherGroup } from "./copublisher";
import type { WasmIdentity } from "./composer";
import { channelErrorMessage } from "./legacy-v1";
import type { PublisherGroupHandle } from "./publisher-group-handle";
import { CoPublishersPanel } from "./CoPublishersPanel";
import { RotationPanel } from "./RotationPanel";
import { SubscribersPanel } from "./SubscribersPanel";
import { useAuth } from "../auth/AuthContext";
import { loadWasm } from "../wasm";

export function ManagePanel(props: { readonly channelIdHex: string }): JSX.Element {
  const auth = useAuth();
  const [identity, setIdentity] = createSignal<WasmIdentity | null>(null);
  const [group, setGroup] = createSignal<PublisherGroupHandle | null>(null);
  const [openError, setOpenError] = createSignal<string | null>(null);

  onMount(() => {
    const secrets = auth.identitySecrets();
    if (secrets === null) {
      setOpenError("Sign in again to manage this channel.");
      return;
    }
    void (async (): Promise<void> => {
      try {
        const wasm = await loadWasm();
        const id = wasm.Identity.fromSeed(secrets.accountIdentitySeed);
        const pg = await openPublisherGroup(id, props.channelIdHex);
        setIdentity(id);
        setGroup(pg);
      } catch (err) {
        setOpenError(
          channelErrorMessage(err, (e) => `Could not open the channel's settings: ${String(e)}`),
        );
      }
    })();
  });

  // Own the shared handles' lifetime. The child panels stop their timers on
  // their own cleanup (which runs before this parent cleanup), so no
  // in-flight callback touches a freed handle on unmount.
  onCleanup(() => {
    try {
      group()?.free();
    } catch {
      /* ignore */
    }
    try {
      identity()?.free();
    } catch {
      /* ignore */
    }
  });

  return (
    <div class="lk-stack lk-channel-manage-body">
      <SubscribersPanel channelIdHex={props.channelIdHex} />
      <Show
        when={group() !== null && identity() !== null}
        fallback={
          <Show when={openError()}>
            {(msg) => (
              <div class="lk-alert info" role="status">
                {msg()}
              </div>
            )}
          </Show>
        }
      >
        <CoPublishersPanel
          channelIdHex={props.channelIdHex}
          publisherGroup={group()!}
          identity={identity()!}
        />
        <RotationPanel
          channelIdHex={props.channelIdHex}
          publisherGroup={group()!}
          identity={identity()!}
        />
      </Show>
    </div>
  );
}

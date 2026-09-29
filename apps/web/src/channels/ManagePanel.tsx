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
import { createEffect, createSignal, on, onCleanup, onMount, Show, type JSX } from "solid-js";

import {
  CHANNEL_NEEDS_NAME,
  channelMetaErrorCode,
  channelMetaErrorText,
  publishChannelMeta,
} from "./channel-meta";
import { apiError, channelErrorText } from "./api";
import { openPublisherGroup, registerPublisherGroupAccess } from "./copublisher";
import type { WasmIdentity } from "./composer";
import { channelErrorMessage } from "./legacy-v1";
import type { PublisherGroupHandle } from "./publisher-group-handle";
import { CoPublishersPanel } from "./CoPublishersPanel";
import { RotationPanel } from "./RotationPanel";
import { SubscribersPanel } from "./SubscribersPanel";
import { ListInDiscover } from "../discover/ListInDiscover";
import { refreshListingAfterEdit } from "../discover/listing";
import { useAuth } from "../auth/AuthContext";
import { useChats } from "../chat/ChatContext";
import { awaitChannelsHydration, channelsActions, channelsState } from "../state/channels-state";

/**
 * ADR-0051 §4 — the channel's name and description, editable by the
 * publisher. Save posts a new metadata record (an ordinary signed encrypted
 * post) and keeps the copy this device edits from. Every subscriber
 * re-titles from the newest record; the `@handle` stays the unique thing.
 */
function ChannelDetailsPanel(props: { readonly channelIdHex: string }): JSX.Element {
  const chats = useChats();
  const held = (): { name: string; description: string } => {
    const shared = channelsState.metaByChannel().get(props.channelIdHex);
    if (shared !== undefined) return { name: shared.name, description: shared.description };
    const local = channelsState.channels().find((c) => c.id === props.channelIdHex);
    return { name: local?.localName ?? "", description: local?.localDescription ?? "" };
  };
  const [name, setName] = createSignal(held().name);
  const [description, setDescription] = createSignal(held().description);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [notice, setNotice] = createSignal<string | null>(null);
  // Follow a record that lands while the form is open (a co-publisher's
  // edit): the newest record is the truth on every device.
  createEffect(
    on(
      () => channelsState.metaByChannel().get(props.channelIdHex)?.updatedAtMs,
      (_stamp, prev) => {
        if (prev === undefined) return;
        setName(held().name);
        setDescription(held().description);
      },
    ),
  );
  const unchanged = (): boolean =>
    name().trim() === held().name && description().trim() === held().description;

  async function onSave(): Promise<void> {
    if (busy()) return;
    setError(null);
    setNotice(null);
    if (name().trim().length === 0) {
      setError(CHANNEL_NEEDS_NAME);
      return;
    }
    setBusy(true);
    try {
      const meta = await publishChannelMeta(props.channelIdHex, {
        name: name(),
        description: description(),
      });
      // The local fields follow, so a device that never reads its own posts
      // back still titles the row the same way.
      channelsActions.updateLocalMeta(props.channelIdHex, {
        localName: meta.name,
        localDescription: meta.description,
      });
      setName(meta.name);
      setDescription(meta.description);
      setNotice("Saved. Subscribers see the new name with their next posts.");
      // ADR-0059 — a listed channel's listing carries the name and
      // description too; editing them updates it (UX spec 6.4).
      const id = chats.accountIdentity();
      if (id !== null) {
        try {
          await refreshListingAfterEdit(props.channelIdHex, id);
        } catch {
          setNotice("Saved. The Discover listing could not be updated; save again to retry.");
        }
      }
    } catch (err) {
      const code = channelMetaErrorCode(err);
      if (code !== undefined) setError(channelMetaErrorText(code));
      else if (apiError.is(err)) setError(`Could not save. ${channelErrorText(err)}`);
      else setError(`Could not save. Try again. (${String(err)})`);
    } finally {
      setBusy(false);
    }
  }

  return (
    <section class="lk-card">
      <h2>Channel</h2>
      <p class="lk-card-desc">
        The name and description reach every subscriber, encrypted. The server never reads them.
      </p>
      <form
        onSubmit={(e): void => {
          e.preventDefault();
          void onSave();
        }}
      >
        <label>
          Name
          <input
            type="text"
            value={name()}
            maxLength={64}
            disabled={busy()}
            aria-invalid={error() === CHANNEL_NEEDS_NAME}
            onInput={(e): void => {
              setName(e.currentTarget.value);
            }}
          />
        </label>
        <label>
          Description (optional)
          <textarea
            rows={3}
            value={description()}
            maxLength={300}
            disabled={busy()}
            onInput={(e): void => {
              setDescription(e.currentTarget.value);
            }}
          />
        </label>
        <Show when={error()}>
          {(msg) => (
            <div class="lk-alert danger" role="alert">
              {msg()}
            </div>
          )}
        </Show>
        <Show when={notice()}>
          {(msg) => (
            <div class="lk-alert info" role="status">
              {msg()}
            </div>
          )}
        </Show>
        <div class="lk-actions lk-row-end">
          <button type="submit" class="primary" disabled={busy() || unchanged()}>
            {busy() ? "Saving..." : "Save"}
          </button>
        </div>
      </form>
    </section>
  );
}

export function ManagePanel(props: { readonly channelIdHex: string }): JSX.Element {
  const auth = useAuth();
  const chats = useChats();
  const [identity, setIdentity] = createSignal<WasmIdentity | null>(null);
  const [group, setGroup] = createSignal<PublisherGroupHandle | null>(null);
  const [openError, setOpenError] = createSignal<string | null>(null);

  let unregisterDsAccess: (() => void) | null = null;
  onMount(() => {
    // The SESSION-LONG account identity, not a throwaway `Identity.fromSeed`:
    // the PublisherGroup lives in this identity's provider, which the channel
    // log snapshots. This panel borrows the handle and never frees it.
    const id = chats.accountIdentity();
    if (auth.identitySecrets() === null || id === null) {
      setOpenError("Sign in again to manage this channel.");
      return;
    }
    void (async (): Promise<void> => {
      try {
        // The provider is restored by the hydration; opening before it would
        // create a second, forked group.
        await awaitChannelsHydration();
        const pg = await openPublisherGroup(id, props.channelIdHex);
        // ADR-0053 — the DS client proves this journal's requests under the
        // group's access key while the panel holds the handle.
        unregisterDsAccess = registerPublisherGroupAccess(id, pg);
        setIdentity(id);
        setGroup(pg);
      } catch (err) {
        setOpenError(
          channelErrorMessage(err, (e) => `Could not open the channel's settings: ${String(e)}`),
        );
      }
    })();
  });

  // Own the group handle's lifetime. The child panels stop their timers on
  // their own cleanup (which runs before this parent cleanup), so no
  // in-flight callback touches a freed handle on unmount. The identity is
  // the session's; it is NOT freed here.
  onCleanup(() => {
    // Withdraw the DS access source BEFORE the handle it reads is freed.
    unregisterDsAccess?.();
    unregisterDsAccess = null;
    try {
      group()?.free();
    } catch {
      /* ignore */
    }
  });

  return (
    <div class="lk-stack lk-channel-manage-body">
      <ChannelDetailsPanel channelIdHex={props.channelIdHex} />
      <ListInDiscover channelIdHex={props.channelIdHex} identity={identity()} />
      <SubscribersPanel channelIdHex={props.channelIdHex} identity={identity()} />
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

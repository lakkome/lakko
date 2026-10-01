// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A channel's member list (docs/ux/who-can-find-me.md section 6.3), opened
 * from the `Members` row of channel info.
 *
 * Each row is a member card this device could open: the member's name, and
 * their `@username` only when they let this channel's members find them
 * (section 6.2). Cards sealed under an epoch this device holds no seed for
 * are counted, not shown.
 */
import { createResource, For, Show, type JSX } from "solid-js";

import { Avatar } from "../ui/Avatar";
import { Icon } from "../ui/icons";
import { loadChannelMembers, type ChannelMembersResult } from "./member-cards";

export function MembersPanel(props: {
  readonly channelIdHex: string;
  readonly seedForEpoch: (epoch: number) => Uint8Array | undefined;
  readonly ownSubscriberPkHex: string | null;
  readonly onBack: () => void;
}): JSX.Element {
  const [members, { refetch }] = createResource(
    () => props.channelIdHex,
    (id): Promise<ChannelMembersResult> =>
      loadChannelMembers({
        channelIdHex: id,
        seedForEpoch: props.seedForEpoch,
        ownSubscriberPkHex: props.ownSubscriberPkHex,
      }),
  );

  return (
    <section class="lk-card" aria-label="Members" style={{ gap: "8px" }}>
      <div class="lk-card-row">
        <button
          type="button"
          class="lk-icon-button"
          title="Back"
          aria-label="Back to channel info"
          onClick={(): void => props.onBack()}
        >
          <Icon name="back" size={18} />
        </button>
        <div class="lk-card-row-body">
          <span class="lk-card-row-title">
            <Show when={members()} fallback="Members">
              {(m) => `Members (${m().rows.length})`}
            </Show>
          </span>
        </div>
      </div>

      <Show when={members.loading}>
        <p class="lk-meta">Loading members...</p>
      </Show>
      <Show when={members.error !== undefined && !members.loading}>
        <div class="lk-alert danger" role="alert">
          Could not load the members. Check your connection and try again.
          <button type="button" class="ghost" onClick={(): void => void refetch()}>
            Try again
          </button>
        </div>
      </Show>
      <Show when={!members.loading && members.error === undefined ? members() : undefined}>
        {(m) => (
          <>
            <Show
              when={m().rows.length > 0}
              fallback={<p class="lk-meta">No members have shared a card yet.</p>}
            >
              <ul class="lk-member-list" style={{ "list-style": "none", margin: 0, padding: 0 }}>
                <For each={m().rows}>
                  {(row): JSX.Element => (
                    <li class="lk-card-row">
                      <span class="lk-card-row-icon">
                        <Avatar label={row.name} id={row.key} size="sm" />
                      </span>
                      <div class="lk-card-row-body">
                        <span class="lk-card-row-title">{row.name}</span>
                        <Show when={row.username}>
                          {(u) => <span class="lk-card-row-sub">@{u()}</span>}
                        </Show>
                      </div>
                    </li>
                  )}
                </For>
              </ul>
            </Show>
            <Show when={m().unreadable > 0}>
              <p class="lk-meta">
                {m().unreadable === 1
                  ? "1 member shared a card under an older key this device does not hold."
                  : `${m().unreadable} members shared cards under an older key this device does not hold.`}
              </p>
            </Show>
            <Show when={m().truncated}>
              <p class="lk-meta">Showing the first members only.</p>
            </Show>
          </>
        )}
      </Show>
    </section>
  );
}

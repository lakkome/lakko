// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The Channels destination: the channel list pane on the left (your channels,
 * plus find-and-subscribe by `@handle` or channel id), the open channel, the
 * new-channel form or the subscribe flow on the right. Wraps every
 * `/channels*` route so the list stays mounted; a narrow screen shows one
 * pane at a time.
 */
import { createSignal, For, Show, type JSX } from "solid-js";
import { A, useLocation, useNavigate, useParams, type RouteSectionProps } from "@solidjs/router";

import { resolveChannelIdOrHandle } from "../channels/channel-discovery";
import { channelsState } from "../state/channels-state";
import { Avatar } from "../ui/Avatar";
import { Icon } from "../ui/icons";
import { paths } from "./paths";

export default function ChannelsLayout(props: RouteSectionProps): JSX.Element {
  const params = useParams<{ id?: string }>();
  const location = useLocation();
  const navigate = useNavigate();

  const open = (): boolean => params.id !== undefined || location.pathname !== paths.channels;

  // G4-5: subscribe entry point. Accepts EITHER a typed `@handle` (resolved
  // to a channel id via OPRF discovery) OR a pasted 32-char hex channel id
  // (used verbatim). Both land on the same subscribe page.
  const [findInput, setFindInput] = createSignal("");
  const [findBusy, setFindBusy] = createSignal(false);
  const [findError, setFindError] = createSignal<string | null>(null);

  async function onFind(): Promise<void> {
    if (findBusy()) return;
    const raw = findInput().trim();
    if (raw.length === 0) return;
    setFindBusy(true);
    setFindError(null);
    try {
      const idHex = await resolveChannelIdOrHandle(raw);
      if (idHex === null) {
        setFindError(`No channel found for ${raw.startsWith("@") ? raw : `@${raw}`}.`);
        return;
      }
      setFindInput("");
      navigate(paths.channelSubscribe(idHex));
    } catch (err) {
      setFindError(`Lookup failed: ${String(err)}`);
    } finally {
      setFindBusy(false);
    }
  }

  return (
    <div class="lk-split" data-open={open() ? "true" : "false"}>
      <aside class="lk-pane" aria-label="Channels">
        <div class="lk-pane-head">
          <h1>Channels</h1>
          <button
            type="button"
            class="lk-icon-button accent"
            title="New channel"
            aria-label="New channel"
            onClick={() => navigate(paths.channelNew)}
          >
            <Icon name="plus" size={18} />
          </button>
        </div>
        <form
          class="lk-search"
          onSubmit={(e) => {
            e.preventDefault();
            void onFind();
          }}
        >
          <Icon name="search" size={18} />
          <input
            type="text"
            placeholder="Find a channel by @handle"
            aria-label="Find a channel"
            autocapitalize="none"
            autocomplete="off"
            spellcheck={false}
            value={findInput()}
            disabled={findBusy()}
            onInput={(e) => setFindInput(e.currentTarget.value)}
          />
        </form>
        <Show when={findError()}>
          {(msg) => <div class="form-error" style={{ padding: "0 20px 8px" }}>{msg()}</div>}
        </Show>
        <Show
          when={channelsState.channels().length > 0}
          fallback={
            <div class="lk-empty">
              <Icon name="channels" size={40} strokeWidth={1.4} />
              <strong>No channels yet</strong>
              <span>Create one to start broadcasting, or find one by its handle to subscribe.</span>
              <button type="button" class="primary" onClick={() => navigate(paths.channelNew)}>
                New channel
              </button>
            </div>
          }
        >
          <div class="lk-pane-body">
            <For each={channelsState.channels()}>
              {(c) => (
                <A
                  href={paths.channel(c.id)}
                  class="lk-row"
                  classList={{ selected: params.id === c.id }}
                  activeClass=""
                  inactiveClass=""
                >
                  <Avatar label={c.localName || "?"} id={c.id} size="md" />
                  <div class="lk-row-body">
                    <span class="lk-row-title">{c.localName || "Unnamed channel"}</span>
                    <span class="lk-row-sub">
                      {c.localDescription !== ""
                        ? c.localDescription
                        : channelsState.keysByChannel().get(c.id) !== undefined
                          ? "Your channel"
                          : "Subscribed"}
                    </span>
                  </div>
                </A>
              )}
            </For>
          </div>
        </Show>
      </aside>
      <div class="lk-main">{props.children}</div>
    </div>
  );
}

/** `/channels` with nothing open: one hint in the main pane. */
export function ChannelsIndex(): JSX.Element {
  return (
    <div class="lk-main-center">
      <span class="lk-hint-pill">Select a channel, or create one</span>
    </div>
  );
}

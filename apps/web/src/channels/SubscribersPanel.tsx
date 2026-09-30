// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The owner's subscriber list.
 *
 * Letting people in is `subscriber-admission.ts`, shared with the app-wide
 * `SubscriberAdmission` loop, so a new subscriber is let in while the app is
 * open whatever screen is showing. While this panel is open it runs the same
 * pass every 5 s so the list and the readiness words stay current. The
 * opacity contract is unchanged, every envelope is computed client-side
 * under the publisher's seed.
 *
 * The rows say "Subscriber N" and a plain readiness word; no key, no hex
 * (docs/ux/client-redesign.md section 5.4).
 */
import { createSignal, For, Match, onCleanup, onMount, Show, Switch, type JSX } from "solid-js";

import { apiError, channelErrorText } from "./api";
import { type WasmIdentity } from "./composer";
import { subscriberCountLabel } from "./channel-row";
import { admitNewSubscribers, admitSubscriber, channelAdmission } from "./subscriber-admission";
import { channelsState } from "../state/channels-state";
import { Avatar } from "../ui/Avatar";

const SUBSCRIBERS_POLL_INTERVAL_MS = 5_000;

export function SubscribersPanel(props: {
  readonly channelIdHex: string;
  /** The session-long account identity: the subscriber list is owner-signed
   *  with the key it derives for this channel (ADR-0059 section 4). */
  readonly identity: WasmIdentity | null;
}): JSX.Element {
  const admission = channelAdmission(props.channelIdHex);
  const subs = (): readonly string[] | null => admission().subscribers;
  const [busyPk, setBusyPk] = createSignal<string | null>(null);
  const [error, setError] = createSignal<string | null>(null);

  async function refresh(): Promise<void> {
    const identity = props.identity;
    if (identity === null) return;
    try {
      await admitNewSubscribers(props.channelIdHex, identity);
      setError(null);
    } catch (err) {
      setError(`Could not load subscribers: ${String(err)}`);
    }
  }

  /** Manual Retry for one subscriber whose envelope did not land. */
  async function deliverTo(pubkeyHex: string): Promise<void> {
    if (busyPk() !== null) return;
    setBusyPk(pubkeyHex);
    setError(null);
    try {
      await admitSubscriber(props.channelIdHex, pubkeyHex);
    } catch (err) {
      setError(
        apiError.is(err)
          ? `Could not set up this subscriber: ${channelErrorText(err)}`
          : `Could not set up this subscriber: ${String(err)}`,
      );
    } finally {
      setBusyPk(null);
    }
  }

  onMount(() => {
    void refresh();
    const handle = setInterval(() => void refresh(), SUBSCRIBERS_POLL_INTERVAL_MS);
    onCleanup(() => clearInterval(handle));
  });

  /** Ready only when delivered at the channel's current epoch. */
  const statusOf = (pk: string): "ready" | "failed" | undefined => {
    const st = admission().status.get(pk);
    if (st === undefined) return undefined;
    const epoch = channelsState.epochByChannel().get(props.channelIdHex) ?? 0;
    if (st.status === "ready" && st.epoch !== epoch) return undefined;
    return st.status;
  };

  return (
    <section class="lk-card">
      <h2>
        Subscribers{" "}
        <Show when={subs()}>
          {(list) => <span class="lk-meta lk-regular">({list().length})</span>}
        </Show>
      </h2>
      <p class="lk-card-desc">New subscribers are let in from this device automatically.</p>
      <Show when={error()}>
        {(msg) => (
          <div class="lk-alert danger" role="alert">
            {msg()}
          </div>
        )}
      </Show>
      <Show
        when={subs() !== null && subs()!.length > 0}
        fallback={
          <p class="lk-muted lk-m0">
            <Show
              when={subs() === null}
              fallback="No subscribers yet. Share the channel's handle so people can subscribe."
            >
              Loading...
            </Show>
          </p>
        }
      >
        <ul class="lk-list bordered" aria-label={subscriberCountLabel(subs()!.length)}>
          <For each={subs()!}>
            {(pk, i): JSX.Element => {
              const status = (): "ready" | "failed" | undefined => statusOf(pk);
              return (
                <li>
                  <Avatar label={`S ${i() + 1}`} id={pk} size="sm" />
                  <span class="lk-fill">Subscriber {i() + 1}</span>
                  <Switch fallback={<span class="lk-status-tag">Setting up...</span>}>
                    <Match when={status() === "ready"}>
                      <span class="lk-status-tag ok">Ready</span>
                    </Match>
                    <Match when={status() === "failed"}>
                      <span class="lk-status-tag bad">Not ready</span>
                      <button
                        type="button"
                        class="small"
                        disabled={busyPk() === pk}
                        onClick={(): void => {
                          void deliverTo(pk);
                        }}
                      >
                        {busyPk() === pk ? "Retrying..." : "Retry"}
                      </button>
                    </Match>
                  </Switch>
                </li>
              );
            }}
          </For>
        </ul>
      </Show>
    </section>
  );
}

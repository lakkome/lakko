// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The owner's subscriber list, and the fan-out behind it.
 *
 * Every 5 s the panel re-fetches the subscriber list, diffs against the
 * already-delivered set, and seals + delivers an envelope for each new
 * public key (the same trade-off the subscribe-screen poll uses on the other
 * side). The loop runs for as long as the panel is mounted, which is for as
 * long as the owner has the channel open: a new subscriber is let in whether
 * or not the Manage screen is showing. The opacity contract is unchanged,
 * every envelope is computed client-side under the publisher's seed.
 *
 * The rows say "Subscriber N" and a plain readiness word; no key, no hex
 * (docs/ux/client-redesign.md section 5.4).
 */
import { createEffect, createSignal, For, Match, onCleanup, Show, Switch, type JSX } from "solid-js";

import {
  apiError,
  channelErrorText,
  deliverEnvelope,
  listSubscribers,
} from "./api";
import { sealSubscriberSeedEnvelope, type WasmIdentity } from "./composer";
import { subscriberCountLabel } from "./channel-row";
import { rememberSubscriberCount } from "./ui-cache";
import { channelsState } from "../state/channels-state";
import { Avatar } from "../ui/Avatar";

const SUBSCRIBERS_POLL_INTERVAL_MS = 5_000;

type DeliveryStatus = "delivered" | "auto" | "failed";

export function SubscribersPanel(props: {
  readonly channelIdHex: string;
  /** The session-long account identity: the subscriber list is owner-signed
   *  with the key it derives for this channel (ADR-0059 section 4). */
  readonly identity: WasmIdentity | null;
}): JSX.Element {
  const [subs, setSubs] = createSignal<ReadonlyArray<string> | null>(null);
  const [busyPk, setBusyPk] = createSignal<string | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  /** Per-subscriber delivery status. `"auto"` means the polling loop sealed
   *  + delivered without a click; `"delivered"` means the owner clicked
   *  Retry; `"failed"` covers both. */
  const [statusByPk, setStatusByPk] = createSignal<ReadonlyMap<string, DeliveryStatus>>(new Map());

  async function refresh(): Promise<void> {
    const identity = props.identity;
    if (identity === null) return;
    try {
      const list = await listSubscribers(props.channelIdHex, identity);
      setSubs(list.subscribers);
      rememberSubscriberCount(props.channelIdHex, list.subscribers.length);
    } catch (err) {
      setError(`Could not load subscribers: ${String(err)}`);
    }
  }

  // First-load fetch.
  void refresh();

  async function deliverTo(pubkeyHex: string, trigger: "manual" | "auto" = "manual"): Promise<void> {
    // Manual delivery is one-at-a-time (single busy spinner across the
    // panel). The auto path ignores busyPk so a slow manual click on one
    // subscriber does not stall the polling loop's progress on others.
    if (trigger === "manual" && busyPk() !== null) return;
    const keys = channelsState.keysByChannel().get(props.channelIdHex);
    if (keys === undefined) {
      setError("This device cannot post to this channel.");
      return;
    }
    if (trigger === "manual") {
      setBusyPk(pubkeyHex);
      setError(null);
    }
    try {
      const subPubBytes = new Uint8Array(pubkeyHex.length / 2);
      for (let i = 0; i < subPubBytes.length; i += 1) {
        subPubBytes[i] = Number.parseInt(pubkeyHex.slice(i * 2, i * 2 + 2), 16);
      }
      const epoch = channelsState.epochByChannel().get(props.channelIdHex) ?? 0;
      const wire = await sealSubscriberSeedEnvelope(
        subPubBytes,
        props.channelIdHex,
        epoch,
        keys.seed,
      );
      // ADR-0031 — file the envelope under the concrete epoch it was sealed
      // for so the server retains it per-epoch (and prunes to the last N).
      await deliverEnvelope(props.channelIdHex, pubkeyHex, wire, undefined, epoch);
      setStatusByPk((m) => new Map(m).set(pubkeyHex, trigger === "auto" ? "auto" : "delivered"));
    } catch (err) {
      if (trigger === "manual") {
        if (apiError.is(err)) {
          setError(`Could not set up this subscriber: ${channelErrorText(err)}`);
        } else {
          setError(`Could not set up this subscriber: ${String(err)}`);
        }
      }
      setStatusByPk((m) => new Map(m).set(pubkeyHex, "failed"));
    } finally {
      if (trigger === "manual") {
        setBusyPk(null);
      }
    }
  }

  // Auto-fanout polling loop. Every SUBSCRIBERS_POLL_INTERVAL_MS: refresh
  // the list, then for every pubkey not yet successfully delivered to
  // (status absent or "failed"), auto-seal + deliver. Sequenced so a fresh
  // subscriber storm does not open N parallel wasm calls.
  createEffect(() => {
    const handle = setInterval(() => {
      void (async (): Promise<void> => {
        await refresh();
        const list = subs();
        if (list === null) return;
        const status = statusByPk();
        for (const pk of list) {
          const st = status.get(pk);
          if (st === undefined || st === "failed") {
            await deliverTo(pk, "auto");
          }
        }
      })();
    }, SUBSCRIBERS_POLL_INTERVAL_MS);
    onCleanup(() => {
      clearInterval(handle);
    });
  });

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
              const status = (): DeliveryStatus | undefined => statusByPk().get(pk);
              return (
                <li>
                  <Avatar label={`S ${i() + 1}`} id={pk} size="sm" />
                  <span class="lk-fill">Subscriber {i() + 1}</span>
                  <Switch fallback={<span class="lk-status-tag">Setting up...</span>}>
                    <Match when={status() === "delivered" || status() === "auto"}>
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

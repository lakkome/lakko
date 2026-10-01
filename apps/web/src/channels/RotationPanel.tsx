// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Scheduled content-key rotation (ADR-0010 / C-54), on the owner's Manage
 * screen: one plain status line and a "Refresh now" button. Drives the
 * `RotationCron` heartbeat while mounted, over the SHARED PublisherGroup
 * handle `ManagePanel` opened.
 *
 * The *scheduled* half of rotation (the membership-change half advances on
 * every MLS epoch bump). The pure `channelShouldRotate` policy (default 24 h
 * / 1000 posts) decides *when*; `runRotationOnce` / `startRotationCron` do
 * *how*: advance the shared PublisherGroup epoch, derive the new seed, and
 * reseal it to every subscriber via the staged + atomic fanout (ADR-0030).
 *
 * **Genesis-seed, not {0,0}.** `channelShouldRotate(default, {0,0}, now)`
 * fires immediately against real wall-clock. Seeding the heartbeat with a
 * `{0,0}` state would rotate + reseal every subscriber the instant the
 * screen opens, so the first open seeds `channelRotationStateAfterRotation(now)`
 * ("just rotated"). The "Refresh now" button deliberately forces a rotation
 * by feeding a fresh `channelRotationStateNew()` through `runRotationOnce`,
 * then adopts the returned state as the new baseline (via the context's
 * `saveState`, which both paths share).
 *
 * **Fail-closed (invariant #4).** A rotation that fails part-way never
 * exposes plaintext or a bad key: the reseal batch is atomic server-side, a
 * single seal failure aborts the whole batch, and a failed fanout only
 * leaves subscribers unable to read the *next* post until the next
 * successful reseal.
 */
import { createSignal, onCleanup, onMount, Show, type JSX } from "solid-js";

import { republishChannelMeta } from "./channel-meta";
import { relativeTimeLabel } from "./relative-time";
import {
  runRotationOnce,
  startRotationCron,
  type RotationContext,
  type RotationCronHandle,
} from "./rotation-cron";
import {
  loadRotationState,
  saveRotationState,
  type ChannelRotationState,
} from "./rotation-state-store";
import type { WasmIdentity } from "./composer";
import type { ChannelRotationPolicy, PublisherGroupHandle } from "./publisher-group-handle";
import { channelsActions, channelsState } from "../state/channels-state";
import { loadWasm } from "../wasm";
import { t } from "../l10n";
import { createLiveText } from "../l10n/live-text";
import { Icon } from "../ui/icons";

export function RotationPanel(props: {
  readonly channelIdHex: string;
  readonly publisherGroup: PublisherGroupHandle;
  readonly identity: WasmIdentity;
}): JSX.Element {
  const [status, setStatus] = createLiveText();
  setStatus(() => t("common.loading"));
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createLiveText();

  let cron: RotationCronHandle | null = null;
  let policy: ChannelRotationPolicy | null = null;

  /** A fresh rotation context bound to the SHARED handle. Both the
   *  heartbeat and the button call this so their `saveState` writes land
   *  in the same localStorage key. */
  function context(): RotationContext {
    return {
      publisherGroup: props.publisherGroup,
      identity: props.identity,
      channelIdHex: props.channelIdHex,
      saveState: (next: ChannelRotationState): void => {
        saveRotationState(props.channelIdHex, next);
      },
      onRotated: (newEpoch: number): void => {
        // The rotation advanced the PublisherGroup's MLS epoch, so the
        // exporter — the content seed every post is sealed under — changed.
        // Re-derive it into the publisher keys (keeping the signing seed) so
        // the next post is sealed under the seed the subscribers were just
        // resealed to, and snapshot the advanced provider into the channel
        // log so a reload re-opens the group at THIS epoch.
        try {
          const prev = channelsState.keysByChannel().get(props.channelIdHex);
          if (prev !== undefined) {
            const seed = props.publisherGroup.channelSeed(props.identity);
            channelsActions.setKeys(props.channelIdHex, {
              seed,
              signingKeySeed: prev.signingKeySeed,
            });
          }
          channelsActions.setEpoch(props.channelIdHex, newEpoch);
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("post-rotation seed refresh failed", err);
        }
        channelsActions.notePublisherGroupChanged();
        refreshStatus();
        // ADR-0051 §4 — re-issue the metadata record under the NEW epoch
        // (original stamp), so a late subscriber who holds only the newest
        // seeds still finds the channel's name. After the seed + epoch
        // refresh above, which is what the post is sealed with.
        void republishChannelMeta(props.channelIdHex, props.identity).catch((err: unknown) => {
          // eslint-disable-next-line no-console
          console.warn("post-rotation channel meta re-issue failed", err);
        });
      },
    };
  }

  /** Load the persisted state, or genesis-seed `after_rotation(now)` on
   *  first open so the heartbeat does not fire an immediate rotation. */
  async function loadOrSeedState(): Promise<ChannelRotationState> {
    const existing = loadRotationState(props.channelIdHex);
    if (existing !== null) return existing;
    const wasm = await loadWasm();
    const seeded = wasm.channelRotationStateAfterRotation(BigInt(Date.now()));
    saveRotationState(props.channelIdHex, seeded);
    return seeded;
  }

  function refreshStatus(): void {
    const state = loadRotationState(props.channelIdHex);
    if (state === null || state.lastRotationMs <= 0n) {
      setStatus(() => t("channels.rotation.never"));
      return;
    }
    const since = state.lastRotationMs;
    setStatus(() => t("channels.rotation.refreshed", { when: relativeTimeLabel(since) }));
  }

  /** Force a rotation regardless of the schedule. */
  async function onRotateNow(): Promise<void> {
    if (busy()) return;
    setBusy(true);
    setError(null);
    try {
      const wasm = await loadWasm();
      const forcePolicy = policy ?? wasm.channelRotationPolicyDefault();
      await runRotationOnce(context(), forcePolicy, wasm.channelRotationStateNew());
      refreshStatus();
    } catch (err) {
      console.warn("[channels] rotation failed", err);
      setError(() => t("channels.rotation.error.refresh"));
    } finally {
      setBusy(false);
    }
  }

  onMount(() => {
    void (async (): Promise<void> => {
      const wasm = await loadWasm();
      const defaultPolicy = wasm.channelRotationPolicyDefault();
      policy = defaultPolicy;
      await loadOrSeedState();
      refreshStatus();
      cron = startRotationCron({
        context: context(),
        policy: defaultPolicy,
        loadState: (): Promise<ChannelRotationState> => loadOrSeedState(),
        onError: (err: unknown): void => {
          console.warn("[channels] rotation check failed", err);
          setError(() => t("channels.rotation.error.check"));
        },
      });
    })();
  });

  onCleanup(() => {
    cron?.stop();
    cron = null;
  });

  return (
    <section class="lk-card">
      <h2>{t("channels.rotation.title")}</h2>
      <p class="lk-card-desc">{t("channels.rotation.desc")}</p>
      <div class="lk-card-row">
        <span class="lk-card-row-icon">
          <Icon name="shield" size={18} />
        </span>
        <div class="lk-card-row-body">
          <span class="lk-card-row-sub" role="status">
            {status()}
          </span>
        </div>
        <button
          type="button"
          class="small lk-card-row-action"
          disabled={busy()}
          onClick={(): void => {
            void onRotateNow();
          }}
        >
          {busy() ? t("channels.rotation.refreshing") : t("channels.rotation.refresh_now")}
        </button>
      </div>
      <Show when={error()}>
        {(msg) => (
          <div class="lk-alert danger" role="alert">
            {msg()}
          </div>
        )}
      </Show>
    </section>
  );
}

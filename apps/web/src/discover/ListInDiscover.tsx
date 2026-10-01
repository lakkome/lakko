// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "List in Discover" on the channel manage screen (docs/ux/feed.md 6.4,
 * ADR-0059). The switch is the OWNER's: the listing is signed by a key only
 * the account that created the channel derives. A co-publisher sees the
 * state and no switch. Whether this account is the owner is asked of the
 * server once (an owner-signed read it refuses to a co-publisher).
 */

import { createEffect, createSignal, For, onMount, Show, type JSX } from "solid-js";

import { probeChannelOwner } from "../channels/api";
import type { WasmIdentity } from "../channels/composer";
import { channelsState } from "../state/channels-state";
import { requestConfirm } from "../ui/confirm-dialog";
import { loadWasm } from "../wasm";
import { appLanguage, languageLabel, languageOptions } from "./languages";
import {
  LISTING_CONFIRM,
  LISTING_NEEDS_HANDLE,
  LISTING_REFUSED,
  ListingNeedsHandleError,
  listChannel,
  listedStamp,
  unlistChannel,
} from "./listing";
import { topicLabel } from "./topic-names";
import { t } from "../l10n";
import { createLiveText } from "../l10n/live-text";

type Topic = { readonly id: number };

export function ListInDiscover(props: {
  readonly channelIdHex: string;
  readonly identity: WasmIdentity | null;
}): JSX.Element {
  const [owner, setOwner] = createSignal<boolean | null>(null);
  const [listed, setListed] = createSignal<boolean | null>(null);
  const [topics, setTopics] = createSignal<readonly Topic[]>([]);
  const held = (): { topic: number; language: string } => {
    const m = channelsState.metaByChannel().get(props.channelIdHex);
    return {
      topic: m?.topic ?? 0,
      language: m?.language !== undefined && m.language !== "" ? m.language : appLanguage(navigator.language),
    };
  };
  const [topic, setTopic] = createSignal(held().topic);
  const [language, setLanguage] = createSignal(held().language);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createLiveText();

  const handle = (): string | undefined =>
    channelsState.channels().find((c) => c.id === props.channelIdHex)?.localHandle;

  onMount(() => {
    void (async (): Promise<void> => {
      const wasm = await loadWasm();
      setTopics([...wasm.channelTopicIds()].map((id) => ({ id })));
      try {
        setListed((await listedStamp(props.channelIdHex)) !== null);
      } catch {
        setListed(null);
      }
    })();
  });
  let probed = false;
  createEffect(() => {
    const id = props.identity;
    if (id === null || probed) return;
    probed = true;
    void probeChannelOwner(props.channelIdHex, id).then(setOwner, () => setOwner(null));
  });

  async function turnOn(): Promise<void> {
    const id = props.identity;
    if (id === null || busy()) return;
    setError(null);
    if (handle() === undefined) {
      setError(LISTING_NEEDS_HANDLE);
      return;
    }
    const ok = await requestConfirm({
      title: t("discover.listing.confirm_title"),
      body: LISTING_CONFIRM(),
      confirmLabel: t("discover.listing.confirm_action"),
      cancelLabel: t("common.cancel"),
      tone: "neutral",
    });
    if (!ok) return;
    setBusy(true);
    setListed(true);
    try {
      await listChannel(props.channelIdHex, id, topic(), language());
    } catch (err) {
      // The switch returns to off when the server refused the listing.
      const live = await listedStamp(props.channelIdHex).catch(() => null);
      setListed(live !== null);
      setError(
        live !== null
          ? () => t("discover.listing.listed_notice")
          : err instanceof ListingNeedsHandleError
            ? LISTING_NEEDS_HANDLE
            : LISTING_REFUSED,
      );
    } finally {
      setBusy(false);
    }
  }

  async function turnOff(): Promise<void> {
    const id = props.identity;
    if (id === null || busy()) return;
    setError(null);
    setBusy(true);
    try {
      await unlistChannel(props.channelIdHex, id);
      setListed(false);
    } catch {
      const live = await listedStamp(props.channelIdHex).catch(() => null);
      setListed(live !== null);
      setError(live !== null ? () => t("discover.listing.remove_failed") : null);
    } finally {
      setBusy(false);
    }
  }

  return (
    <section class="lk-card">
      <h2>{t("discover.listing.title")}</h2>
      <Show
        when={owner() === true}
        fallback={
          <p class="lk-card-desc">
            {listed() === true
              ? t("discover.listing.viewer_listed")
              : listed() === false
                ? t("discover.listing.viewer_not_listed")
                : t("discover.listing.checking")}
          </p>
        }
      >
        <p class="lk-card-desc">
          {t("discover.listing.owner_desc")}
        </p>
        <label class="lk-row">
          <input
            type="checkbox"
            role="switch"
            checked={listed() === true}
            disabled={busy() || listed() === null}
            onChange={(e): void => {
              const on = e.currentTarget.checked;
              e.currentTarget.checked = listed() === true;
              void (on ? turnOn() : turnOff());
            }}
          />
          <span>{listed() === true ? t("discover.listing.listed") : t("discover.listing.not_listed")}</span>
        </label>
        <Show when={listed() !== true}>
          <label>
            {t("discover.listing.topic")}
            <select
              value={String(topic())}
              disabled={busy()}
              onChange={(e): void => {
                setTopic(Number(e.currentTarget.value));
              }}
            >
              <For each={topics()}>{(tp) => <option value={String(tp.id)}>{topicLabel(tp.id)}</option>}</For>
            </select>
          </label>
          <label>
            {t("discover.language")}
            <select
              value={language()}
              disabled={busy()}
              onChange={(e): void => {
                setLanguage(e.currentTarget.value);
              }}
            >
              <For each={languageOptions(language())}>
                {(code) => <option value={code}>{languageLabel(code)}</option>}
              </For>
            </select>
          </label>
        </Show>
        <Show when={handle() === undefined}>
          <p class="lk-card-desc">{LISTING_NEEDS_HANDLE()}</p>
        </Show>
      </Show>
      <Show when={error()}>
        {(msg) => (
          <div class="lk-alert info" role="status">
            {msg()}
          </div>
        )}
      </Show>
    </section>
  );
}

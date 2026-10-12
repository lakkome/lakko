// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The feed editor (`docs/ux/feed.md` section 4.1): one panel, four sections,
 * and a live preview beside it (wide) or under it (narrow) that re-ranks as
 * the person changes things. Everything is optional; a blank feed is
 * "Latest".
 *
 * Validity is the core's: Save is disabled with the core's refusal turned
 * into one sentence (`feed-draft.ts`), never a check written here.
 */

import { createMemo, createSignal, For, Show, type JSX } from "solid-js";

import {
  blankDraft,
  draftProblem,
  parseWords,
  presetDraft,
  type FeedDraft,
  type FeedDraftCore,
  type FeedWeightsDraft,
  type HalfLifeKey,
  type SuggestionsKey,
} from "./feed-draft";
import { FEED_ICONS, feedIconName } from "./feed-icons";
import { feedSourceKey, type FeedSource } from "./feed-model";
import { Icon } from "../ui/icons";
import { t } from "../l10n";

/** One line of the preview. */
export interface PreviewLine {
  readonly key: string;
  readonly source: string;
  readonly text: string;
}

export interface FeedEditorProps {
  readonly core: FeedDraftCore;
  readonly initial: FeedDraft;
  /** A new feed may start from a preset; an existing one may be deleted. */
  readonly isNew: boolean;
  readonly sources: readonly FeedSource[];
  /** The top of the feed this draft would build, or `null` while it can't. */
  readonly preview: (draft: FeedDraft) => readonly PreviewLine[] | null;
  readonly onSave: (draft: FeedDraft) => void;
  readonly onCancel: () => void;
  readonly onDelete?: (() => void) | undefined;
}

const WEIGHTS: ReadonlyArray<{ readonly key: keyof FeedWeightsDraft; readonly labelKey: string }> = [
  { key: "recency", labelKey: "feed.editor.weight.recency" },
  { key: "reactions", labelKey: "feed.editor.weight.reactions" },
  { key: "replies", labelKey: "feed.editor.weight.replies" },
  { key: "fromContacts", labelKey: "feed.editor.weight.from_contacts" },
  { key: "mentionsMe", labelKey: "feed.editor.weight.mentions" },
  { key: "hasMedia", labelKey: "feed.editor.weight.media" },
];

const HALF_LIVES: ReadonlyArray<{ readonly key: HalfLifeKey; readonly labelKey: string }> = [
  { key: "hours", labelKey: "feed.editor.fade.hours" },
  { key: "day", labelKey: "feed.editor.fade.day" },
  { key: "week", labelKey: "feed.editor.fade.week" },
];

const SUGGESTIONS: ReadonlyArray<{ readonly key: SuggestionsKey; readonly labelKey: string }> = [
  { key: "never", labelKey: "feed.editor.suggestions.never" },
  { key: "rarely", labelKey: "feed.editor.suggestions.rarely" },
  { key: "sometimes", labelKey: "feed.editor.suggestions.sometimes" },
];

type Membership = "any" | "only" | "never";

export function FeedEditor(props: FeedEditorProps): JSX.Element {
  const [draft, setDraft] = createSignal<FeedDraft>(props.initial);
  const [hideText, setHideText] = createSignal(props.initial.hideWords.join("\n"));
  const [onlyText, setOnlyText] = createSignal(props.initial.onlyWords.join("\n"));
  const maxWeight = props.core.feedMaxWeight();

  const update = (patch: Partial<FeedDraft>): void => {
    setDraft((d) => ({ ...d, ...patch }));
  };

  const startFrom = (value: string): void => {
    const name = draft().name;
    const icon = draft().icon;
    const picked =
      value === "blank"
        ? { ...blankDraft(props.core), name }
        : presetDraft(
            props.core,
            {
              latest: props.core.FeedPreset.Latest,
              popular: props.core.FeedPreset.Popular,
              "for-me": props.core.FeedPreset.ForMe,
            }[value as "latest" | "popular" | "for-me"],
            name,
          );
    // The name and icon are the person's; a starting point changes the rules.
    const next = { ...picked, icon };
    setDraft(next);
    setHideText(next.hideWords.join("\n"));
    setOnlyText(next.onlyWords.join("\n"));
  };

  const kinds = (): "both" | "groups" | "channels" => {
    const d = draft();
    if (d.groups && d.channels) return "both";
    return d.groups ? "groups" : "channels";
  };

  const membership = (idHex: string): Membership => {
    const d = draft();
    if (d.include.includes(idHex)) return "only";
    if (d.exclude.includes(idHex)) return "never";
    return "any";
  };

  const setMembership = (idHex: string, value: Membership): void => {
    const d = draft();
    const include = d.include.filter((x) => x !== idHex);
    const exclude = d.exclude.filter((x) => x !== idHex);
    if (value === "only") include.push(idHex);
    if (value === "never") exclude.push(idHex);
    // Naming a conversation makes this the person's own list again.
    update({ include, exclude, sourcesStripped: false });
  };

  const problem = createMemo(() => draftProblem(props.core, draft()));
  const preview = createMemo(() => (problem() === null ? props.preview(draft()) : null));

  const numberIn = (raw: string, min: number, max: number, fallback: number): number => {
    const n = Number.parseInt(raw, 10);
    return Number.isNaN(n) ? fallback : Math.min(max, Math.max(min, n));
  };

  return (
    <section class="lk-feed-editor" aria-label={props.isNew ? t("feed.new") : t("feed.edit_feed")}>
      <header class="lk-feed-editor-head">
        <button type="button" class="ghost" onClick={() => props.onCancel()}>
          {t("common.cancel")}
        </button>
        <h1>{props.isNew ? t("feed.new") : t("feed.edit_feed")}</h1>
        <button
          type="button"
          class="primary"
          disabled={problem() !== null}
          onClick={() => props.onSave(draft())}
        >
          {t("common.save")}
        </button>
      </header>

      <div class="lk-feed-editor-body">
        <form class="lk-feed-editor-form" onSubmit={(e) => e.preventDefault()}>
          <label class="lk-feed-field">
            <span>{t("feed.editor.name")}</span>
            <input
              type="text"
              value={draft().name}
              maxLength={64}
              onInput={(e) => update({ name: e.currentTarget.value })}
            />
          </label>

          {/* UX spec 4.1 "Icons": one of twelve, by index, in a grid under
              the name. An index this client does not know is kept and drawn
              as the default, with no cell selected. */}
          <div class="lk-feed-field">
            <span id="lk-feed-icon-label">{t("feed.editor.icon")}</span>
            <div class="lk-feed-icon-grid" role="radiogroup" aria-labelledby="lk-feed-icon-label">
              <For each={FEED_ICONS}>
                {(icon, index) => (
                  <button
                    type="button"
                    role="radio"
                    class="lk-feed-icon-cell"
                    aria-checked={draft().icon === index()}
                    aria-label={t(icon.labelKey)}
                    title={t(icon.labelKey)}
                    onClick={() => update({ icon: index() })}
                  >
                    <Icon name={feedIconName(index())} size={20} />
                  </button>
                )}
              </For>
            </div>
          </div>

          <Show when={props.isNew}>
            <label class="lk-feed-field">
              <span>{t("feed.editor.start_from")}</span>
              <select onChange={(e) => startFrom(e.currentTarget.value)}>
                <option value="blank">{t("feed.editor.start.blank")}</option>
                <option value="latest">{t("feed.preset.latest")}</option>
                <option value="popular">{t("feed.preset.popular")}</option>
                <option value="for-me">{t("feed.preset.for_me")}</option>
              </select>
            </label>
          </Show>

          <fieldset class="lk-feed-section">
            <legend>{t("feed.editor.sources")}</legend>
            <div class="lk-feed-segmented" role="radiogroup" aria-label={t("feed.editor.sources")}>
              <For
                each={[
                  { key: "both", labelKey: "feed.editor.sources.both" },
                  { key: "groups", labelKey: "feed.editor.sources.groups" },
                  { key: "channels", labelKey: "feed.editor.sources.channels" },
                ] as const}
              >
                {(o) => (
                  <button
                    type="button"
                    role="radio"
                    aria-checked={kinds() === o.key}
                    onClick={() =>
                      update({ groups: o.key !== "channels", channels: o.key !== "groups" })
                    }
                  >
                    {t(o.labelKey)}
                  </button>
                )}
              </For>
            </div>
            <label class="lk-feed-check">
              <input
                type="checkbox"
                checked={draft().includeMuted}
                onChange={(e) => update({ includeMuted: e.currentTarget.checked })}
              />
              <span>{t("feed.editor.include_muted")}</span>
            </label>
            <Show when={draft().sourcesStripped}>
              <p class="lk-feed-hint">
                {t("feed.editor.stripped_hint")}
              </p>
            </Show>
            <Show when={props.sources.length > 0}>
              <p class="lk-feed-hint">
                {t("feed.editor.membership_hint")}
              </p>
              <ul class="lk-feed-conversations">
                <For each={props.sources}>
                  {(s) => (
                    <li>
                      <span class="lk-feed-conv-name">{s.title}</span>
                      <select
                        aria-label={t("feed.editor.membership.a11y", { name: s.title })}
                        value={membership(feedSourceKey(s))}
                        onChange={(e) => setMembership(feedSourceKey(s), e.currentTarget.value as Membership)}
                      >
                        <option value="any">—</option>
                        <option value="only">{t("feed.editor.membership.only")}</option>
                        <option value="never">{t("feed.editor.membership.never")}</option>
                      </select>
                    </li>
                  )}
                </For>
              </ul>
            </Show>
          </fieldset>

          <fieldset class="lk-feed-section">
            <legend>{t("feed.editor.what_counts")}</legend>
            <For each={WEIGHTS}>
              {(w) => (
                <label class="lk-feed-slider">
                  <span>{t(w.labelKey)}</span>
                  <input
                    type="range"
                    min={0}
                    max={maxWeight}
                    value={draft().weights[w.key]}
                    onInput={(e) =>
                      update({
                        weights: { ...draft().weights, [w.key]: Number(e.currentTarget.value) },
                      })
                    }
                  />
                  <span class="lk-feed-slider-ends" aria-hidden="true">
                    <span>{t("feed.editor.slider.min")}</span>
                    <span>{t("feed.editor.slider.max")}</span>
                  </span>
                </label>
              )}
            </For>
            <div class="lk-feed-field">
              <span>{t("feed.editor.fade")}</span>
              <div class="lk-feed-segmented" role="radiogroup" aria-label={t("feed.editor.fade")}>
                <For each={HALF_LIVES}>
                  {(h) => (
                    <button
                      type="button"
                      role="radio"
                      aria-checked={draft().halfLife === h.key}
                      onClick={() => update({ halfLife: h.key })}
                    >
                      {t(h.labelKey)}
                    </button>
                  )}
                </For>
              </div>
            </div>
            <label class="lk-feed-field">
              <span>{t("feed.editor.look_back")}</span>
              <input
                type="number"
                min={1}
                max={30}
                value={draft().windowDays}
                onChange={(e) =>
                  update({ windowDays: numberIn(e.currentTarget.value, 1, 30, draft().windowDays) })
                }
              />
            </label>
          </fieldset>

          <fieldset class="lk-feed-section">
            <legend>{t("feed.editor.filters")}</legend>
            <label class="lk-feed-check">
              <input
                type="checkbox"
                checked={draft().forMe}
                onChange={(e) => update({ forMe: e.currentTarget.checked })}
              />
              <span>{t("feed.editor.for_me")}</span>
            </label>
            <label class="lk-feed-check">
              <input
                type="checkbox"
                checked={draft().mediaOnly}
                onChange={(e) => update({ mediaOnly: e.currentTarget.checked })}
              />
              <span>{t("feed.editor.media_only")}</span>
            </label>
            <label class="lk-feed-check">
              <input
                type="checkbox"
                checked={draft().hideOwn}
                onChange={(e) => update({ hideOwn: e.currentTarget.checked })}
              />
              <span>{t("feed.editor.hide_own")}</span>
            </label>
            <label class="lk-feed-field">
              <span>{t("feed.editor.min_reactions")}</span>
              <input
                type="number"
                min={0}
                max={999}
                value={draft().minReactions}
                onChange={(e) =>
                  update({
                    minReactions: numberIn(e.currentTarget.value, 0, 999, draft().minReactions),
                  })
                }
              />
            </label>
            <label class="lk-feed-field">
              <span>{t("feed.editor.hide_words")}</span>
              <textarea
                rows={3}
                value={hideText()}
                onInput={(e) => {
                  setHideText(e.currentTarget.value);
                  update({ hideWords: parseWords(props.core, e.currentTarget.value) });
                }}
              />
            </label>
            <label class="lk-feed-field">
              <span>{t("feed.editor.only_words")}</span>
              <textarea
                rows={3}
                value={onlyText()}
                onInput={(e) => {
                  setOnlyText(e.currentTarget.value);
                  update({ onlyWords: parseWords(props.core, e.currentTarget.value) });
                }}
              />
            </label>
          </fieldset>

          <fieldset class="lk-feed-section">
            <legend>{t("feed.editor.suggestions")}</legend>
            <label class="lk-feed-field">
              <span>{t("feed.editor.suggestions_rate")}</span>
              <select
                value={draft().suggestions}
                onChange={(e) => update({ suggestions: e.currentTarget.value as SuggestionsKey })}
              >
                <For each={SUGGESTIONS}>{(s) => <option value={s.key}>{t(s.labelKey)}</option>}</For>
              </select>
            </label>
          </fieldset>

          <Show when={problem()}>
            {(p) => (
              <p class="lk-feed-problem" role="alert">
                {p()}
              </p>
            )}
          </Show>

          <Show when={!props.isNew && props.onDelete !== undefined}>
            <button type="button" class="lk-feed-delete" onClick={() => props.onDelete?.()}>
              {t("feed.editor.delete")}
            </button>
          </Show>
        </form>

        <aside class="lk-feed-preview" aria-label={t("feed.editor.preview")}>
          <h2>{t("feed.editor.preview")}</h2>
          <Show
            when={preview()}
            fallback={<p class="lk-feed-hint">{t("feed.editor.preview_pending")}</p>}
          >
            {(lines) => (
              <Show when={lines().length > 0} fallback={<p class="lk-feed-hint">{t("feed.empty.no_match")}</p>}>
                <ol>
                  <For each={lines()}>
                    {(l) => (
                      <li>
                        <span class="lk-feed-source">{l.source}</span>
                        <span class="lk-feed-preview-text">{l.text}</span>
                      </li>
                    )}
                  </For>
                </ol>
              </Show>
            )}
          </Show>
        </aside>
      </div>
    </section>
  );
}

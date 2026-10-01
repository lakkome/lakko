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
import type { FeedSource } from "./feed-model";
import { Icon } from "../ui/icons";

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

const WEIGHTS: ReadonlyArray<{ readonly key: keyof FeedWeightsDraft; readonly label: string }> = [
  { key: "recency", label: "How recent" },
  { key: "reactions", label: "Reactions" },
  { key: "replies", label: "Replies" },
  { key: "fromContacts", label: "From your contacts" },
  { key: "mentionsMe", label: "Mentions you" },
  { key: "hasMedia", label: "Has photos or video" },
];

const HALF_LIVES: ReadonlyArray<{ readonly key: HalfLifeKey; readonly label: string }> = [
  { key: "hours", label: "A few hours" },
  { key: "day", label: "A day" },
  { key: "week", label: "A week" },
];

const SUGGESTIONS: ReadonlyArray<{ readonly key: SuggestionsKey; readonly label: string }> = [
  { key: "never", label: "Never" },
  { key: "rarely", label: "Rarely" },
  { key: "sometimes", label: "Sometimes" },
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
    <section class="lk-feed-editor" aria-label={props.isNew ? "New feed" : "Edit feed"}>
      <header class="lk-feed-editor-head">
        <button type="button" class="ghost" onClick={() => props.onCancel()}>
          Cancel
        </button>
        <h1>{props.isNew ? "New feed" : "Edit feed"}</h1>
        <button
          type="button"
          class="primary"
          disabled={problem() !== null}
          onClick={() => props.onSave(draft())}
        >
          Save
        </button>
      </header>

      <div class="lk-feed-editor-body">
        <form class="lk-feed-editor-form" onSubmit={(e) => e.preventDefault()}>
          <label class="lk-feed-field">
            <span>Name</span>
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
            <span id="lk-feed-icon-label">Icon</span>
            <div class="lk-feed-icon-grid" role="radiogroup" aria-labelledby="lk-feed-icon-label">
              <For each={FEED_ICONS}>
                {(icon, index) => (
                  <button
                    type="button"
                    role="radio"
                    class="lk-feed-icon-cell"
                    aria-checked={draft().icon === index()}
                    aria-label={icon.label}
                    title={icon.label}
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
              <span>Start from</span>
              <select onChange={(e) => startFrom(e.currentTarget.value)}>
                <option value="blank">Blank</option>
                <option value="latest">Latest</option>
                <option value="popular">Popular</option>
                <option value="for-me">For me</option>
              </select>
            </label>
          </Show>

          <fieldset class="lk-feed-section">
            <legend>Sources</legend>
            <div class="lk-feed-segmented" role="radiogroup" aria-label="Sources">
              <For
                each={[
                  { key: "both", label: "Both" },
                  { key: "groups", label: "All groups" },
                  { key: "channels", label: "All channels" },
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
                    {o.label}
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
              <span>Include muted conversations</span>
            </label>
            <Show when={draft().sourcesStripped}>
              <p class="lk-feed-hint">
                Shared feeds arrive without specific conversations. Pick some below to make it
                yours.
              </p>
            </Show>
            <Show when={props.sources.length > 0}>
              <p class="lk-feed-hint">
                "Only" limits the feed to the conversations marked so; "Never" leaves one out.
              </p>
              <ul class="lk-feed-conversations">
                <For each={props.sources}>
                  {(s) => (
                    <li>
                      <span class="lk-feed-conv-name">{s.title}</span>
                      <select
                        aria-label={`${s.title} in this feed`}
                        value={membership(s.idHex)}
                        onChange={(e) => setMembership(s.idHex, e.currentTarget.value as Membership)}
                      >
                        <option value="any">—</option>
                        <option value="only">Only</option>
                        <option value="never">Never</option>
                      </select>
                    </li>
                  )}
                </For>
              </ul>
            </Show>
          </fieldset>

          <fieldset class="lk-feed-section">
            <legend>What counts</legend>
            <For each={WEIGHTS}>
              {(w) => (
                <label class="lk-feed-slider">
                  <span>{w.label}</span>
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
                    <span>Ignore</span>
                    <span>A lot</span>
                  </span>
                </label>
              )}
            </For>
            <div class="lk-feed-field">
              <span>How fast things fade</span>
              <div class="lk-feed-segmented" role="radiogroup" aria-label="How fast things fade">
                <For each={HALF_LIVES}>
                  {(h) => (
                    <button
                      type="button"
                      role="radio"
                      aria-checked={draft().halfLife === h.key}
                      onClick={() => update({ halfLife: h.key })}
                    >
                      {h.label}
                    </button>
                  )}
                </For>
              </div>
            </div>
            <label class="lk-feed-field">
              <span>Look back (days)</span>
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
            <legend>Filters</legend>
            <label class="lk-feed-check">
              <input
                type="checkbox"
                checked={draft().forMe}
                onChange={(e) => update({ forMe: e.currentTarget.checked })}
              />
              <span>Only what mentions me, replies to me or reacts to mine</span>
            </label>
            <label class="lk-feed-check">
              <input
                type="checkbox"
                checked={draft().mediaOnly}
                onChange={(e) => update({ mediaOnly: e.currentTarget.checked })}
              />
              <span>Only with photos or video</span>
            </label>
            <label class="lk-feed-check">
              <input
                type="checkbox"
                checked={draft().hideOwn}
                onChange={(e) => update({ hideOwn: e.currentTarget.checked })}
              />
              <span>Hide my own messages</span>
            </label>
            <label class="lk-feed-field">
              <span>At least this many reactions</span>
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
              <span>Hide words (one per line)</span>
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
              <span>Only these words (one per line)</span>
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
            <legend>Suggestions</legend>
            <label class="lk-feed-field">
              <span>How often a suggested channel appears</span>
              <select
                value={draft().suggestions}
                onChange={(e) => update({ suggestions: e.currentTarget.value as SuggestionsKey })}
              >
                <For each={SUGGESTIONS}>{(s) => <option value={s.key}>{s.label}</option>}</For>
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
              Delete feed
            </button>
          </Show>
        </form>

        <aside class="lk-feed-preview" aria-label="Preview">
          <h2>Preview</h2>
          <Show
            when={preview()}
            fallback={<p class="lk-feed-hint">The preview appears once the feed can be saved.</p>}
          >
            {(lines) => (
              <Show when={lines().length > 0} fallback={<p class="lk-feed-hint">No messages match this feed.</p>}>
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

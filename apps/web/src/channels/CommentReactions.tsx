// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The reaction chips under a channel comment (ADR-0087 section 13a, phase
 * B; UX `group-and-channel-privacy.md` 13.2): the post's chips, at a
 * comment's size. The emoji and the number, the person's own highlighted; a
 * tap adds or takes off; a long press or a right click shows the anonymity
 * note. Adding a NEW emoji is the comment menu's `React`.
 */
import { accountSuspended } from "../auth/suspension-state";
import { createMemo, createSignal, For, onCleanup, Show, type JSX } from "solid-js";

import { useAuth } from "../auth/AuthContext";
import { createLongPress } from "../ui/long-press";
import { t } from "../l10n";
import { wasmSync } from "../wasm";
import { channelAllowedReactions } from "./ban-list";
import { channelSeedFor } from "./channel-access";
import {
  commentLabelCounts,
  CommentReactionCapError,
  drawnCommentCounts,
  ownCommentReactions,
  resolveCommentLabelCounts,
  toggleCommentReaction,
} from "./comment-feedback";
import { bytesOfHex } from "./feedback-api";
import { reactionCandidates, recentReactionsFor } from "./post-feedback";
import { showChannelReactionNote } from "./PostReactions";

/** How long the cap line stays (reactions-and-emoji 10). */
const CAP_LINE_MS = 3000;

interface Chip {
  readonly emoji: string;
  readonly count: number;
  readonly mine: boolean;
}

/** Ordered chips through the core's rule, with the allowed set applied. */
export function orderChips(channelIdHex: string, counts: ReadonlyMap<string, number>, mine: readonly string[]): Chip[] {
  const allowed = channelAllowedReactions(channelIdHex);
  const entries = [...counts].filter(([e]) =>
    allowed.mode === "all" ? true : allowed.mode === "only" ? allowed.emoji.includes(e) : false,
  );
  try {
    const w = wasmSync();
    const sources = entries.map(
      ([emoji, count], i) => new w.ChipSource(emoji, count, BigInt(i), mine.includes(emoji), new Uint8Array()),
    );
    const a =
      allowed.mode === "off"
        ? w.AllowedReactions.off()
        : allowed.mode === "only"
          ? w.AllowedReactions.only([...allowed.emoji])
          : w.AllowedReactions.all();
    try {
      const row = w.reactionChips(sources, "channel", a);
      try {
        return row.chips.map((c) => {
          const chip = { emoji: c.emoji, count: c.count, mine: mine.includes(c.emoji) };
          c.free();
          return chip;
        });
      } finally {
        row.free();
      }
    } finally {
      a.free();
    }
  } catch {
    return entries
      .sort((x, y) => y[1] - x[1])
      .slice(0, 11)
      .map(([emoji, count]) => ({ emoji, count, mine: mine.includes(emoji) }));
  }
}

export function CommentReactions(props: {
  channelIdHex: string;
  post: number;
  comment: number;
  epoch: number;
  /** The post's time (the own register's retention runs from it). */
  postAtMs: number;
  /** Shows the cap line (also raised by the menu's `React`). */
  capLine: boolean;
  onCap: () => void;
}): JSX.Element {
  const auth = useAuth();
  const ref = (): { idHex: string; post: number; comment: number } => ({
    idHex: props.channelIdHex,
    post: props.post,
    comment: props.comment,
  });
  const mine = (): readonly string[] => ownCommentReactions(ref());

  const resolved = createMemo((): Map<string, number> | null => {
    const labels = commentLabelCounts(ref());
    if (labels === undefined) return null;
    const seed = channelSeedFor(props.channelIdHex, props.epoch);
    if (seed === undefined) return null;
    if (labels.size === 0) return new Map();
    const s = auth.state();
    const account = s.kind === "session" ? s.account.accountId : null;
    try {
      return resolveCommentLabelCounts(
        wasmSync(),
        seed,
        bytesOfHex(props.channelIdHex),
        props.epoch,
        props.post,
        props.comment,
        labels,
        reactionCandidates([...mine()], recentReactionsFor(account)),
      );
    } catch {
      return new Map();
    }
  });

  const chips = createMemo((): Chip[] => {
    const base = resolved();
    if (base === null && mine().length === 0) return [];
    return orderChips(props.channelIdHex, drawnCommentCounts(ref(), base ?? new Map()), mine());
  });

  const press = async (emoji: string): Promise<void> => {
    try {
      await toggleCommentReaction({ ...ref(), epoch: props.epoch, postAtMs: props.postAtMs }, emoji);
    } catch (err) {
      if (err instanceof CommentReactionCapError) props.onCap();
    }
  };

  const longPress = createLongPress(() => showChannelReactionNote());

  return (
    <Show when={chips().length > 0 || props.capLine}>
      <div class="lk-reactions lk-comment-reactions">
        <For each={chips()}>
          {(chip) => (
            <button
              type="button"
              class="lk-reaction"
              classList={{ mine: chip.mine }}
              // ADR-0089 step 3: the counts stay; a suspended account reacts to nothing.
              disabled={accountSuspended()}
              aria-label={
                chip.mine
                  ? t("chat.reaction.chip_yours_a11y", { emoji: chip.emoji })
                  : t("chat.reaction.chip_a11y", { emoji: chip.emoji, count: chip.count })
              }
              aria-pressed={chip.mine}
              onContextMenu={(e) => {
                e.preventDefault();
                e.stopPropagation();
                showChannelReactionNote();
              }}
              onPointerDown={(e) => {
                e.stopPropagation();
                longPress.onPointerDown(e);
              }}
              onPointerMove={longPress.onPointerMove}
              onPointerUp={longPress.onPointerUp}
              onPointerCancel={longPress.onPointerCancel}
              onPointerLeave={longPress.onPointerLeave}
              onClick={(e) => {
                if (longPress.swallowClick(e)) return;
                e.stopPropagation();
                void press(chip.emoji);
              }}
            >
              <span class="lk-reaction-emoji">{chip.emoji}</span>
              <span class="lk-reaction-count">{chip.count}</span>
            </button>
          )}
        </For>
      </div>
      <Show when={props.capLine}>
        <p class="lk-reaction-cap" role="status">
          {t("chat.reaction.cap_reached")}
        </p>
      </Show>
    </Show>
  );
}

/** A cap line that clears itself after {@link CAP_LINE_MS}. */
export function createCapLine(): { readonly shown: () => boolean; readonly raise: () => void } {
  const [shown, setShown] = createSignal(false);
  let timer: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => {
    if (timer !== undefined) clearTimeout(timer);
  });
  return {
    shown,
    raise: () => {
      setShown(true);
      if (timer !== undefined) clearTimeout(timer);
      timer = setTimeout(() => setShown(false), CAP_LINE_MS);
    },
  };
}

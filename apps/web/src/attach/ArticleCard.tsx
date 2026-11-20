// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/attachments.md` section 11.2 — the article bubble: the first
 * image as a banner, the title, the excerpt the core computed, and
 * `Read article`. A press anywhere opens the reader.
 */
import { Show, type JSX } from "solid-js";

import { t } from "../l10n";
import { Icon } from "../ui/icons";
import type { Article } from "./types";
import "./bubbles.css";

export function ArticleCard(props: {
  readonly article: Article;
  readonly bannerUrl?: string | undefined;
  readonly excerpt: string;
  readonly onOpen: () => void;
}): JSX.Element {
  return (
    <button type="button" class="lk-article-card" onClick={() => props.onOpen()}>
      <Show when={props.bannerUrl !== undefined}>
        <img class="lk-article-banner" src={props.bannerUrl} alt="" />
      </Show>
      <span class="lk-article-title">{props.article.title}</span>
      <Show when={props.excerpt.length > 0}>
        <span class="lk-article-excerpt">{props.excerpt}</span>
      </Show>
      <span class="lk-article-read">
        {t("article.read")}
        <Icon name="chevron-forward" size={16} />
      </span>
    </button>
  );
}

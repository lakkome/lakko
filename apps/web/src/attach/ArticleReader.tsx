// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/attachments.md` section 11.3 — the article reader. Built from the
 * decoded tree only: every node becomes a SolidJS element, text goes in as
 * text. No HTML string is produced, no `innerHTML`, no Markdown parser
 * (ADR-0092 section 8).
 */
import { createResource, For, Match, onCleanup, onMount, Show, Switch, type JSX } from "solid-js";

import { t } from "../l10n";
import { Icon } from "../ui/icons";
import type { Article, ArticleBlock, ArticleRun } from "./types";
import "./bubbles.css";

export function ArticleReader(props: {
  readonly article: Article;
  readonly imageUrl: (index: number) => Promise<string | null>;
  readonly author: string;
  readonly dateText: string;
  readonly onClose: () => void;
  readonly onMenu: () => void;
  readonly onLink: (url: string) => void;
}): JSX.Element {
  let close: HTMLButtonElement | undefined;
  onMount(() => {
    queueMicrotask(() => close?.focus());
    const onKey = (e: KeyboardEvent): void => {
      if (e.key !== "Escape" || e.defaultPrevented) return;
      if (document.querySelector(".lk-msg-menu, dialog[open], .lk-confirm-dialog") !== null) return;
      e.preventDefault();
      props.onClose();
    };
    document.addEventListener("keydown", onKey);
    onCleanup(() => document.removeEventListener("keydown", onKey));
  });
  return (
    <section class="lk-article-screen" role="dialog" aria-modal="true" aria-label={props.article.title}>
      <header class="lk-article-bar">
        <div class="lk-article-bar-meta">
          <span class="lk-ak-ellipsis" style={{ "font-weight": "600" }}>{props.author}</span>
          <span class="lk-ak-small lk-ak-muted">{props.dateText}</span>
        </div>
        <button
          type="button"
          class="lk-icon-button"
          aria-label={t("article.more")}
          title={t("article.more")}
          onClick={() => props.onMenu()}
        >
          <Icon name="ellipsis-circle" />
        </button>
        <button
          ref={close}
          type="button"
          class="lk-icon-button"
          aria-label={t("common.close")}
          title={t("common.close")}
          onClick={() => props.onClose()}
        >
          <Icon name="xmark" />
        </button>
      </header>
      <div class="lk-article-scroll">
        <article class="lk-article-column">
          <h1>{props.article.title}</h1>
          <For each={props.article.blocks}>
            {(block) => <Block block={block} imageUrl={props.imageUrl} onLink={props.onLink} />}
          </For>
        </article>
      </div>
    </section>
  );
}

function Block(props: {
  readonly block: ArticleBlock;
  readonly imageUrl: (index: number) => Promise<string | null>;
  readonly onLink: (url: string) => void;
}): JSX.Element {
  const b = props.block;
  return (
    <Switch>
      <Match when={b.kind === "paragraph" && b}>
        {(p) => (
          <p>
            <Runs runs={p().runs} onLink={props.onLink} />
          </p>
        )}
      </Match>
      <Match when={b.kind === "heading" && b}>
        {(h) => (
          <Show
            when={h().level === "h1"}
            fallback={
              <h3 class="lk-article-h2" style={{ "font-size": "17px", "font-weight": "600", margin: "6px 0 0" }}>
                <Runs runs={h().runs} onLink={props.onLink} />
              </h3>
            }
          >
            <h2 style={{ "font-size": "22px", "font-weight": "600", margin: "8px 0 0" }}>
              <Runs runs={h().runs} onLink={props.onLink} />
            </h2>
          </Show>
        )}
      </Match>
      <Match when={b.kind === "quote" && b}>
        {(q) => (
          <blockquote>
            <Runs runs={q().runs} onLink={props.onLink} />
          </blockquote>
        )}
      </Match>
      <Match when={b.kind === "list" && b}>
        {(l) => {
          const items = (): JSX.Element => (
            <For each={l().items}>
              {(it) => (
                <li class={it.depth === 1 ? "depth-1" : undefined}>
                  <Runs runs={it.runs} onLink={props.onLink} />
                </li>
              )}
            </For>
          );
          return <Show when={l().ordered} fallback={<ul>{items()}</ul>}><ol>{items()}</ol></Show>;
        }}
      </Match>
      <Match when={b.kind === "image" && b}>
        {(img) => <Figure index={img().attachmentIndex} caption={img().caption} imageUrl={props.imageUrl} onLink={props.onLink} />}
      </Match>
      <Match when={b.kind === "divider"}>
        <hr />
      </Match>
    </Switch>
  );
}

function Figure(props: {
  readonly index: number;
  readonly caption: readonly ArticleRun[];
  readonly imageUrl: (index: number) => Promise<string | null>;
  readonly onLink: (url: string) => void;
}): JSX.Element {
  const [url] = createResource(
    () => props.index,
    async (i) => {
      try {
        return await props.imageUrl(i);
      } catch {
        return null;
      }
    },
  );
  const label = (): string => {
    const text = props.caption.map((r) => r.text).join("").trim();
    return text.length > 0 ? text : t("article.format.image");
  };
  return (
    <figure>
      <Show when={url()} fallback={<div class="lk-article-image-placeholder" role="img" aria-label={label()} />}>
        {(src) => <img src={src()} alt={label()} />}
      </Show>
      <Show when={props.caption.length > 0}>
        <figcaption aria-hidden="true">
          <Runs runs={props.caption} onLink={props.onLink} />
        </figcaption>
      </Show>
    </figure>
  );
}

function Runs(props: { readonly runs: readonly ArticleRun[]; readonly onLink: (url: string) => void }): JSX.Element {
  return (
    <For each={props.runs}>
      {(run) => {
        const inner = (): JSX.Element => <Marked bold={run.bold} italic={run.italic} text={run.text} />;
        return run.kind === "link" ? (
          <button type="button" class="lk-article-link" onClick={() => props.onLink(run.url)}>
            {inner()}
          </button>
        ) : (
          inner()
        );
      }}
    </For>
  );
}

function Marked(props: { readonly bold: boolean; readonly italic: boolean; readonly text: string }): JSX.Element {
  if (props.bold && props.italic) {
    return (
      <strong>
        <em>{props.text}</em>
      </strong>
    );
  }
  if (props.bold) return <strong>{props.text}</strong>;
  if (props.italic) return <em>{props.text}</em>;
  return <>{props.text}</>;
}

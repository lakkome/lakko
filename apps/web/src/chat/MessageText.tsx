// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A message's text with its links live (`docs/ux/message-links.md`). Draws
 * inline, so the caller's own element keeps the wrapping and the "edited" mark.
 */
import { A } from "@solidjs/router";
import { createMemo, createSignal, For, Show, type JSX } from "solid-js";

import { isDesktop, publicOrigin } from "../platform";
import { openHostUrl } from "../platform/host";
import { loadWasm, type Wasm } from "../wasm";
import { linkOpenOf, messageTextPieces } from "./message-text";

const [rule, setRule] = createSignal<Pick<Wasm, "splitMessageLinks"> | null>(null);
let ruleAsked = false;

function askForRule(): void {
  if (ruleAsked) return;
  ruleAsked = true;
  void loadWasm().then((w) => setRule(w), () => undefined);
}

function ownOrigin(): string | null {
  try {
    return publicOrigin();
  } catch {
    return null;
  }
}

function LinkPiece(props: { readonly url: string }): JSX.Element {
  const target = createMemo(() => {
    const origin = ownOrigin();
    return origin === null ? null : linkOpenOf(props.url, origin, isDesktop());
  });
  const inApp = (): string | null => {
    const t = target();
    return t !== null && t.kind === "in-app" ? t.path : null;
  };

  return (
    <Show
      when={inApp()}
      fallback={
        <a
          class="lk-text-link"
          href={props.url}
          target="_blank"
          rel="noopener noreferrer"
          onClick={(e) => {
            // The row around a link has its own click.
            e.stopPropagation();
            if (target()?.kind !== "system-browser") return;
            e.preventDefault();
            void openHostUrl(props.url);
          }}
        >
          {props.url}
        </a>
      }
    >
      {(path) => (
        <A class="lk-text-link" href={path()} onClick={(e) => e.stopPropagation()}>
          {props.url}
        </A>
      )}
    </Show>
  );
}

export function MessageText(props: { readonly text: string }): JSX.Element {
  askForRule();
  const pieces = createMemo(() => {
    const loaded = rule();
    return messageTextPieces(props.text, loaded === null ? null : loaded.splitMessageLinks);
  });

  return (
    <For each={pieces()}>
      {(piece) => (
        <Show when={piece.isLink} fallback={piece.text}>
          <LinkPiece url={piece.text} />
        </Show>
      )}
    </For>
  );
}

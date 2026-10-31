// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** The Open a link dialog of the desktop app (`docs/ux/desktop.md` section 7). */
import { useNavigate } from "@solidjs/router";
import { createEffect, createSignal, Show, type JSX } from "solid-js";

import { t } from "../l10n";
import { ModalLayer } from "../ui/ModalLayer";
import { publicOrigin } from "./index";
import { appPathOf, parseLakkoLink } from "./lakko-link";
import { openLinkReason, setOpenLinkReason } from "./open-link-state";

export function OpenLinkDialog(): JSX.Element {
  const navigate = useNavigate();
  const [text, setText] = createSignal("");
  const [refused, setRefused] = createSignal(false);
  let field: HTMLInputElement | undefined;

  const close = (): void => {
    setOpenLinkReason(null);
    setText("");
    setRefused(false);
  };

  createEffect(() => {
    if (openLinkReason() !== null) queueMicrotask(() => field?.focus());
  });

  const open = (e: SubmitEvent): void => {
    e.preventDefault();
    const link = parseLakkoLink(text(), publicOrigin());
    if (link === null) {
      setRefused(true);
      return;
    }
    close();
    navigate(appPathOf(link));
  };

  return (
    <ModalLayer open={openLinkReason() !== null} title={t("chats.create.open_link")} onClose={close}>
      <form class="stack" onSubmit={open}>
        <p class="lk-row-sub">
          {openLinkReason() === "copied" ? t("open_link.hint_copied") : t("open_link.hint")}
        </p>
        <label>
          {t("open_link.field")}
          <input
            ref={field}
            type="text"
            autocomplete="off"
            spellcheck={false}
            value={text()}
            onInput={(e) => {
              setRefused(false);
              setText(e.currentTarget.value);
            }}
          />
        </label>
        <Show when={refused()}>
          <p class="form-error" role="alert">{t("open_link.not_lakko")}</p>
        </Show>
        <button class="primary" type="submit" disabled={text().trim().length === 0}>
          {t("open_link.open")}
        </button>
      </form>
    </ModalLayer>
  );
}

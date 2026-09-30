// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The owner's publish composer: the chat composer's shape (`lk-composer`)
 * with a growing text field and a send arrow. Enter publishes, Shift+Enter
 * breaks the line. A failed publish shows one line above the field.
 */
import { Show, type JSX } from "solid-js";

import { t } from "../l10n";
import { Icon } from "../ui/icons";

export function PublishComposer(props: {
  readonly value: string;
  readonly onInput: (value: string) => void;
  readonly onSubmit: () => void;
  readonly busy: boolean;
  readonly error: string | null;
}): JSX.Element {
  const empty = (): boolean => props.value.trim().length === 0;
  let field: HTMLTextAreaElement | undefined;
  const grow = (): void => {
    if (field === undefined) return;
    field.style.height = "auto";
    field.style.height = `${Math.min(field.scrollHeight, 130)}px`;
  };
  return (
    <div class="lk-channel-compose">
      <Show when={props.error}>
        {(msg) => (
          <div class="lk-alert danger lk-channel-compose-error" role="alert">
            {msg()}
          </div>
        )}
      </Show>
      <form
        class="lk-composer"
        onSubmit={(e): void => {
          e.preventDefault();
          if (props.busy || empty()) return;
          props.onSubmit();
          queueMicrotask(grow);
        }}
      >
        <textarea
          ref={field}
          class="lk-composer-input"
          rows={1}
          placeholder={t("channels.compose.placeholder")}
          aria-label={t("channels.compose.label")}
          autocomplete="off"
          value={props.value}
          disabled={props.busy}
          onInput={(e): void => {
            props.onInput(e.currentTarget.value);
            grow();
          }}
          onKeyDown={(e): void => {
            if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
              e.preventDefault();
              e.currentTarget.form?.requestSubmit();
            }
          }}
        />
        <button
          type="submit"
          class="lk-icon-button lk-send"
          title={props.busy ? t("channels.compose.publishing") : t("channels.compose.publish")}
          aria-label={props.busy ? t("channels.compose.publishing") : t("channels.compose.publish")}
          aria-busy={props.busy ? "true" : undefined}
          disabled={props.busy || empty()}
        >
          <Icon name="send" size={20} />
        </button>
      </form>
    </div>
  );
}

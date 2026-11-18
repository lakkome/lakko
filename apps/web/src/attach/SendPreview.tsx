// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/attachments.md` sections 3 and 4 — the send preview, one screen
 * for one or many picked items: a strip of thumbnails and the current item
 * large for photos and videos, file chips for files and audio, a caption, a
 * `Send as file` switch for media, a remove control per item, `Send`.
 * Cancel sends nothing. The conversation encodes and uploads what this
 * returns; here nothing leaves the device.
 */
import { createMemo, createSignal, For, onCleanup, Show, type JSX } from "solid-js";

import { formatAttachmentSize } from "../chat/attachments";
import { paintVideoBehind } from "../media/video-overlay";
import { t } from "../l10n";
import { Icon } from "../ui/icons";
import { AttachScreen, AttachSwitch, AttachSwitchGroup } from "./AttachScreen";
import { previewVerdict } from "./capture-rules";
import type { PreviewResult } from "./types";
import "./editors.css";

interface Item {
  readonly id: number;
  readonly file: File;
  readonly url: string | null;
}

export function SendPreview(props: {
  readonly files: readonly File[];
  readonly mode: "media" | "file" | "audio";
  readonly onCancel: () => void;
  readonly onSend: (r: PreviewResult) => Promise<void> | void;
  readonly onRetake?: () => void;
}): JSX.Element {
  let nextId = 0;
  const visual = (f: File): boolean => f.type.startsWith("image/") || f.type.startsWith("video/");
  const initial: Item[] = [];
  let removedTooBig = false;
  for (const file of props.files) {
    if (props.mode !== "media" && previewVerdict(file, props.mode, false) === "too-big") {
      removedTooBig = true;
      continue;
    }
    const url =
      props.mode === "media" && visual(file) && typeof URL.createObjectURL === "function"
        ? URL.createObjectURL(file)
        : null;
    initial.push({ id: nextId++, file, url });
  }
  const [items, setItems] = createSignal<Item[]>(initial);
  const [currentId, setCurrentId] = createSignal<number | null>(initial[0]?.id ?? null);
  const [asFile, setAsFile] = createSignal(false);
  const [caption, setCaption] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [tooBigNotice, setTooBigNotice] = createSignal(removedTooBig);

  onCleanup(() => {
    for (const it of items()) if (it.url !== null) URL.revokeObjectURL(it.url);
  });

  const verdictOf = (it: Item): ReturnType<typeof previewVerdict> => previewVerdict(it.file, props.mode, asFile());
  const current = createMemo(() => items().find((i) => i.id === currentId()) ?? items()[0]);
  const blocked = createMemo(() => items().some((i) => {
    const v = verdictOf(i);
    return v === "video-refused" || v === "too-big";
  }));

  function remove(id: number): void {
    const it = items().find((i) => i.id === id);
    if (it?.url !== null && it?.url !== undefined) URL.revokeObjectURL(it.url);
    const rest = items().filter((i) => i.id !== id);
    setItems(rest);
    if (currentId() === id) setCurrentId(rest[0]?.id ?? null);
    if (rest.length === 0) props.onCancel();
  }

  async function send(): Promise<void> {
    if (busy() || blocked() || items().length === 0) return;
    setBusy(true);
    try {
      await props.onSend({
        files: items().map((i) => i.file),
        asFile: props.mode === "media" ? asFile() : true,
        caption: caption().trim(),
      });
    } finally {
      setBusy(false);
    }
  }

  return (
    <AttachScreen
      title={t("attach.preview.title")}
      onClose={() => props.onCancel()}
      footer={
        <>
          <Show when={props.onRetake}>
            {(retake) => (
              <button type="button" onClick={() => retake()()}>
                {t("camera.retake")}
              </button>
            )}
          </Show>
          <button
            type="button"
            class="primary"
            disabled={busy() || blocked() || items().length === 0}
            onClick={() => void send()}
          >
            {t("attach.preview.send")}
          </button>
        </>
      }
    >
      <Show when={tooBigNotice()}>
        <p class="lk-attach-error" role="alert">
          {t("attach.preview.file_too_big")}
        </p>
      </Show>
      <Show when={props.mode === "media"}>
        <Show when={current()}>
          {(it) => (
            <div class="lk-attach-stage">
              <Show
                when={it().url !== null && !asFile()}
                fallback={<FileChip file={it().file} />}
              >
                <Show
                  when={it().file.type.startsWith("video/")}
                  fallback={<img src={it().url ?? ""} alt={it().file.name} />}
                >
                  <video ref={paintVideoBehind} src={it().url ?? ""} controls playsinline aria-label={it().file.name} />
                </Show>
              </Show>
            </div>
          )}
        </Show>
        <Show when={current() !== undefined && verdictOf(current() as Item) === "video-refused"}>
          <p class="lk-attach-error" role="alert">
            {t("attach.preview.video_refused")}
          </p>
        </Show>
        <Show when={props.onRetake === undefined && items().length > 1}>
          <div class="lk-attach-strip">
            <For each={items()}>
              {(it) => (
                <div class="lk-attach-thumb">
                  <button
                    type="button"
                    aria-current={current()?.id === it.id}
                    aria-label={it.file.name}
                    onClick={() => setCurrentId(it.id)}
                  >
                    <Show when={it.url !== null} fallback={<Icon name="folder" />}>
                      <Show when={it.file.type.startsWith("video/")} fallback={<img src={it.url ?? ""} alt="" />}>
                        <video ref={paintVideoBehind} src={it.url ?? ""} muted preload="metadata" />
                      </Show>
                    </Show>
                  </button>
                  <button
                    type="button"
                    class="lk-attach-remove"
                    aria-label={t("attach.preview.remove", { name: it.file.name })}
                    title={t("attach.preview.remove", { name: it.file.name })}
                    onClick={() => remove(it.id)}
                  >
                    <Icon name="xmark-circle-fill" size={14} />
                  </button>
                </div>
              )}
            </For>
          </div>
        </Show>
        <AttachSwitchGroup>
          <AttachSwitch title={t("attach.preview.send_as_file")} on={asFile()} onChange={setAsFile} />
        </AttachSwitchGroup>
      </Show>
      <Show when={props.mode !== "media"}>
        <div class="lk-attach-rows">
          <For each={items()}>
            {(it) => (
              <div class="lk-attach-chip">
                <FileChip file={it.file} />
                <button
                  type="button"
                  class="lk-icon-button"
                  aria-label={t("attach.preview.remove", { name: it.file.name })}
                  title={t("attach.preview.remove", { name: it.file.name })}
                  onClick={() => {
                    setTooBigNotice(false);
                    remove(it.id);
                  }}
                >
                  <Icon name="xmark-circle-fill" size={16} />
                </button>
              </div>
            )}
          </For>
        </div>
      </Show>
      <input
        class="lk-attach-field"
        type="text"
        placeholder={t("attach.preview.caption")}
        aria-label={t("attach.preview.caption")}
        value={caption()}
        onInput={(e) => setCaption(e.currentTarget.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && !e.shiftKey) {
            e.preventDefault();
            void send();
          }
        }}
      />
    </AttachScreen>
  );
}

function FileChip(props: { readonly file: File }): JSX.Element {
  return (
    <span class="lk-attach-chip-text">
      <span>{props.file.name}</span>
      <span class="lk-attach-note">
        {props.file.type.length > 0
          ? t("common.fact_pair", { first: formatAttachmentSize(props.file.size), second: props.file.type })
          : formatAttachmentSize(props.file.size)}
      </span>
    </span>
  );
}

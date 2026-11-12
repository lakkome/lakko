// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The composer of a channel, for the owner and publishers only (ADR-0087
 * sections 12 and 13; UX `docs/ux/group-and-channel-privacy.md` section 13):
 * the chat composer's shape (`lk-composer`), a growing text field, the attach
 * button, emoji, and send. A post's plaintext is the chat envelope, so a post
 * carries text and files exactly as a message does; attachments are
 * encrypted and uploaded first, then the envelope is published.
 *
 * Editing a post puts its text in the field under an "Edit" strip; send then
 * publishes the edit op naming the post's cursor.
 *
 * A voice message and a video note are recorded with the chat's own
 * recorder (`media/recording-controller.ts` and the `MediaComposer` pieces),
 * the record button standing where Send stands while the field is empty.
 * Every file a post carries is uploaded with the channel file life
 * (ADR-0087 section 13: 45 idle days).
 */
import { createSignal, For, onCleanup, Show, type JSX } from "solid-js";

import { useAuth } from "../auth/AuthContext";
import {
  AttachmentSizeError,
  composeEditEnvelope,
  composeEnvelope,
  encryptAndUploadBytes,
  encryptAndUploadFile,
  formatAttachmentSize,
  type AttachmentRefBytes,
} from "../chat/attachments";
import { EmojiPicker } from "../media/EmojiPicker";
import { useCallHost } from "../calls/CallHostContext";
import { callOwnsCaptureNow } from "../calls/capture-rule";
import { uiRules } from "../chat/ui-rules";
import type { CaptureResult } from "../media/media-capture";
import { RecordButton, RecorderNotices, RecordingBar, VideoNoteOverlay, recorderBusy } from "../media/MediaComposer";
import { createRecorder } from "../media/recording-controller";
import type { MediaHub } from "../media/media-hub";
import { CHANNEL_FILE_IDLE_DAYS, DEFAULT_BASE_URL, StorageClient } from "../storage/client";
import { fetchGifMedia, MAX_GIF_FULL_BYTES, type GifItem } from "../media/gif-client";
import { t } from "../l10n";
import { Icon } from "../ui/icons";
import { channelErrorText } from "./channel-error-copy";

export interface ChannelComposerProps {
  /** Publishes one plaintext; resolves once the post is on the server. */
  readonly publish: (plaintext: Uint8Array) => Promise<void>;
  /** A post being edited: its cursor and current text. */
  readonly editing: { readonly cursor: number; readonly text: string } | null;
  readonly onCancelEdit: () => void;
  /** The timeline's media hub: a take's object URLs are filed there. */
  readonly hub: MediaHub;
}

export function ChannelComposer(props: ChannelComposerProps): JSX.Element {
  const auth = useAuth();
  const [text, setText] = createSignal("");
  const [files, setFiles] = createSignal<readonly File[]>([]);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [emojiOpen, setEmojiOpen] = createSignal(false);
  let field: HTMLTextAreaElement | undefined;
  let picker: HTMLInputElement | undefined;
  let lastEditCursor: number | null = null;
  // ADR-0087 section 13 — a channel file asks for its shorter idle life.
  const storage = new StorageClient(DEFAULT_BASE_URL, CHANNEL_FILE_IDLE_DAYS);
  const callHost = useCallHost();

  /** A finished take the person chose to send: its poster first (a video
   *  note's still), then the take, as one post. */
  async function sendTake(take: CaptureResult): Promise<void> {
    setBusy(true);
    setError(null);
    try {
      let thumb: AttachmentRefBytes["thumb"];
      if (take.poster !== undefined) {
        try {
          const posterBytes = new Uint8Array(await take.poster.blob.arrayBuffer());
          const tref = await encryptAndUploadBytes(
            posterBytes,
            { mime: take.poster.mime, name: undefined },
            {},
            storage,
          );
          thumb = {
            blobId: tref.blobId,
            key: tref.key,
            nonce: tref.nonce,
            mime: take.poster.mime,
            size: posterBytes.length,
            width: take.poster.width,
            height: take.poster.height,
          };
        } catch {
          // As in a chat: a failed poster upload sends the take without one.
          thumb = undefined;
        }
      }
      const stamp = new Date().toISOString().replace(/[:.]/g, "-");
      const ref = await encryptAndUploadBytes(
        new Uint8Array(await take.blob.arrayBuffer()),
        { mime: take.mime, name: take.kind === "voice" ? `voice-${stamp}.m4a` : `video-${stamp}.mp4` },
        {
          kind: take.kind,
          durationMs: take.durationMs,
          ...(take.waveform === undefined ? {} : { waveform: take.waveform }),
          ...(take.width === undefined ? {} : { width: take.width }),
          ...(take.height === undefined ? {} : { height: take.height }),
          ...(thumb === undefined ? {} : { thumb }),
        },
        storage,
      );
      await props.publish(await composeEnvelope("", [ref]));
    } catch (err) {
      setError(channelErrorText(err, () => t("channels.publish.error.generic")));
    } finally {
      setBusy(false);
    }
  }

  const recorder = createRecorder({
    onSend: (take) => void sendTake(take),
    inCall: () => callOwnsCaptureNow(uiRules(), callHost),
    createUrl: (blob) => props.hub.registry.set(`take:${Date.now()}:${Math.random()}`, blob),
    revokeUrl: (url) => {
      for (const [k, u] of props.hub.registry.entries()) if (u === url) props.hub.registry.revoke(k);
    },
  });
  onCleanup(() => recorder.dispose());

  const grow = (): void => {
    if (field === undefined) return;
    field.style.height = "auto";
    field.style.height = `${Math.min(field.scrollHeight, 130)}px`;
  };

  // Entering edit mode fills the field once with the post's text.
  const editing = (): { readonly cursor: number; readonly text: string } | null => {
    const e = props.editing;
    if (e !== null && e.cursor !== lastEditCursor) {
      lastEditCursor = e.cursor;
      queueMicrotask(() => {
        setText(e.text);
        setFiles([]);
        grow();
        field?.focus();
      });
    }
    if (e === null) lastEditCursor = null;
    return e;
  };

  const empty = (): boolean => text().trim().length === 0 && (editing() !== null || files().length === 0);

  async function onSend(): Promise<void> {
    if (busy() || empty()) return;
    setBusy(true);
    setError(null);
    try {
      const edit = editing();
      if (edit !== null) {
        await props.publish(await composeEditEnvelope(edit.cursor, text().trim()));
        props.onCancelEdit();
      } else {
        const refs: AttachmentRefBytes[] = [];
        for (const f of files()) refs.push(await encryptAndUploadFile(f, storage));
        await props.publish(await composeEnvelope(text().trim(), refs));
        setFiles([]);
      }
      setText("");
      queueMicrotask(grow);
    } catch (err) {
      if (err instanceof AttachmentSizeError) {
        setError(
          t("chat.attachment.error.too_large", {
            size: formatAttachmentSize(err.got),
            limit: formatAttachmentSize(err.max),
          }),
        );
      } else {
        setError(channelErrorText(err, () => t("channels.publish.error.generic")));
      }
    } finally {
      setBusy(false);
    }
  }

  /** ADR-0047 — a GIF picked in the composer goes out as its own post: the
   *  proxy's bytes, re-encrypted as an ordinary attachment of kind gif. */
  async function sendGif(item: GifItem): Promise<void> {
    if (busy()) return;
    setBusy(true);
    setError(null);
    try {
      const token = auth.identitySecrets()?.sessionToken ?? null;
      if (token === null) throw new Error("no session for the GIF proxy");
      const r = await fetchGifMedia(item.fullUrl, token, MAX_GIF_FULL_BYTES);
      if (r.kind !== "ok") throw new Error("GIF download failed");
      const ref = await encryptAndUploadBytes(
        r.value.bytes,
        { mime: r.value.mime, name: `gif-${item.id}.gif` },
        { kind: "gif", width: item.width, height: item.height },
        storage,
      );
      await props.publish(await composeEnvelope("", [ref]));
    } catch (err) {
      setError(channelErrorText(err, () => t("channels.publish.error.generic")));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div class="lk-channel-compose">
      <RecorderNotices recorder={recorder} />
      <VideoNoteOverlay recorder={recorder} />
      <Show when={error()}>
        {(msg) => (
          <div class="lk-alert danger lk-channel-compose-error" role="alert">
            {msg()}
          </div>
        )}
      </Show>
      <Show when={editing()}>
        <div class="lk-reply-strip">
          <span class="lk-reply-strip-rule" />
          <div class="lk-reply-strip-body">
            <span class="lk-reply-strip-name">{t("chat.message.edit")}</span>
            <span class="lk-reply-strip-excerpt">{props.editing?.text ?? ""}</span>
          </div>
          <button
            type="button"
            class="lk-icon-button lk-reply-strip-close"
            aria-label={t("common.cancel")}
            title={t("common.cancel")}
            onClick={() => {
              setText("");
              props.onCancelEdit();
            }}
          >
            <Icon name="xmark" size={20} />
          </button>
        </div>
      </Show>
      <Show when={files().length > 0}>
        <ul class="lk-channel-files">
          <For each={files()}>
            {(f) => (
              <li class="lk-channel-file">
                <Icon name="doc" size={14} />
                <span>{f.name}</span>
                <span class="lk-meta">{formatAttachmentSize(f.size)}</span>
                <button
                  type="button"
                  class="lk-icon-button"
                  aria-label={t("chat.composer.remove_attachment")}
                  disabled={busy()}
                  onClick={() => setFiles(files().filter((x) => x !== f))}
                >
                  <Icon name="xmark-circle-fill" size={14} />
                </button>
              </li>
            )}
          </For>
        </ul>
      </Show>
      <form
        class="lk-composer"
        onSubmit={(e): void => {
          e.preventDefault();
          void onSend();
        }}
      >
        <Show when={!recorderBusy(recorder)} fallback={<RecordingBar recorder={recorder} />}>
        <Show when={editing() === null}>
          <button
            type="button"
            class="lk-icon-button"
            title={t("chat.composer.attach")}
            aria-label={t("chat.composer.attach")}
            disabled={busy()}
            onClick={() => picker?.click()}
          >
            <Icon name="paperclip" size={20} />
          </button>
          <input
            ref={picker}
            type="file"
            multiple
            hidden
            onChange={(e) => {
              const picked = Array.from(e.currentTarget.files ?? []);
              e.currentTarget.value = "";
              if (picked.length > 0) setFiles([...files(), ...picked]);
            }}
          />
        </Show>
        <textarea
          ref={field}
          class="lk-composer-input"
          rows={1}
          placeholder={t("channels.compose.placeholder")}
          aria-label={t("channels.compose.label")}
          autocomplete="off"
          value={text()}
          disabled={busy()}
          onInput={(e): void => {
            setText(e.currentTarget.value);
            grow();
          }}
          onKeyDown={(e): void => {
            // Enter publishes, Shift+Enter breaks the line, as in a chat.
            // The value is taken from the field itself, so a key pressed
            // before the input event was handled still sends what is shown.
            if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
              e.preventDefault();
              if (e.currentTarget.value !== text()) setText(e.currentTarget.value);
              void onSend();
            }
            if (e.key === "Escape" && editing() !== null) {
              setText("");
              props.onCancelEdit();
            }
          }}
        />
        <div class="lk-menu-anchor">
          <button
            type="button"
            class="lk-icon-button"
            title={t("chat.composer.emoji_gifs")}
            aria-label={t("chat.composer.emoji_gifs")}
            disabled={busy()}
            onClick={() => setEmojiOpen((v) => !v)}
          >
            <Icon name="face-smiling" size={20} />
          </button>
          <Show when={emojiOpen()}>
            <EmojiPicker
              mode="compose"
              align="right"
              sessionToken={() => auth.identitySecrets()?.sessionToken ?? null}
              onEmoji={(emoji) => {
                setText(text() + emoji);
                field?.focus();
              }}
              onGif={(item) => {
                setEmojiOpen(false);
                void sendGif(item);
              }}
              onClose={() => setEmojiOpen(false)}
            />
          </Show>
        </div>
        </Show>
        {/* UX media-messages section 1 — Send while there is something to
            send, otherwise the record button; it stays mounted through a held
            take because it holds the pointer capture. */}
        <Show
          when={(!empty() || editing() !== null) && !recorderBusy(recorder)}
          fallback={<RecordButton recorder={recorder} />}
        >
        <button
          type="submit"
          class="lk-icon-button lk-send"
          title={busy() ? t("channels.compose.publishing") : t("channels.compose.publish")}
          aria-label={busy() ? t("channels.compose.publishing") : t("channels.compose.publish")}
          aria-busy={busy() ? "true" : undefined}
          disabled={busy() || empty()}
        >
          <Icon name="send-fill" size={20} />
        </button>
        </Show>
      </form>
    </div>
  );
}

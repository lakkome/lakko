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
import { createSignal, onCleanup, Show, type JSX } from "solid-js";

import { useAuth } from "../auth/AuthContext";
import {
  AttachmentSizeError,
  composeArticleEditEnvelope,
  composeContentEnvelope,
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
import { AttachComposer, type AttachEditRequest } from "../attach/AttachComposer";
import { checklistBodyOf, pollBodyOf } from "../attach/actions";
import { reencodePhoto } from "../attach/photo-encode";
import { ComposerTray } from "../chat/ComposerTray";
import { backspaceRemovesLast, messageRefs, traySendable } from "../chat/compose-tray";
import { release as releaseTrayItem, trayFor } from "../chat/tray-store";
import { mediaKey } from "../media/media-hub";
import { deleteBlobs } from "../storage/blob-deletes";
import { blobIdsOfRefs } from "../chat/blob-ids";
import type { MessageContent } from "../../wasm/lakko_ffi_wasm";
import { loadWasm } from "../wasm";

export interface ChannelComposerProps {
  /** Publishes one plaintext; resolves once the post is on the server. */
  readonly publish: (plaintext: Uint8Array) => Promise<void>;
  /** A post being edited: its cursor and current text. */
  readonly editing: { readonly cursor: number; readonly text: string } | null;
  readonly onCancelEdit: () => void;
  /** The timeline's media hub: a take's object URLs are filed there. */
  readonly hub: MediaHub;
  /** ADR-0092 — an `Edit checklist` / article `Edit` the timeline asked for. */
  readonly contentEdit?: () => AttachEditRequest | null;
  readonly clearContentEdit?: () => void;
  /** Names this channel's composer tray, which outlives the view
   *  (`docs/ux/media-messages.md` section 11). */
  readonly trayKey: string;
}

export function ChannelComposer(props: ChannelComposerProps): JSX.Element {
  const auth = useAuth();
  const [text, setText] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [emojiOpen, setEmojiOpen] = createSignal(false);
  let field: HTMLTextAreaElement | undefined;
  let lastEditCursor: number | null = null;
  // ADR-0087 section 13 — a channel file asks for its shorter idle life.
  const storage = new StorageClient(DEFAULT_BASE_URL, CHANNEL_FILE_IDLE_DAYS);
  const callHost = useCallHost();
  // Section 11 — what the next post carries, uploaded as it is added, with
  // the channel file life.
  const accountOf = (): string | null => {
    const st = auth.state();
    return st.kind === "session" ? st.account.accountId : null;
  };
  const tray = trayFor(`${accountOf() ?? "none"}:channel:${props.trayKey}`, {
    storage: () => storage,
    inlineType: (mime) => props.hub.inlineType(mime),
    discard: (refs) => {
      const account = accountOf();
      if (account !== null) void deleteBlobs(account, blobIdsOfRefs(refs));
    },
    onError: (msg) => setError(msg),
  });

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
        grow();
        field?.focus();
      });
    }
    if (e === null) lastEditCursor = null;
    return e;
  };

  const empty = (): boolean => text().trim().length === 0 && (editing() !== null || tray.items().length === 0);
  /** Every tray item is uploaded (section 11: Send waits for them). */
  const sendable = (): boolean =>
    editing() !== null ? text().trim().length > 0 : traySendable(text().trim(), tray.items().map((i) => i.status));

  async function onSend(): Promise<void> {
    if (busy() || empty() || !sendable()) return;
    setBusy(true);
    setError(null);
    try {
      const edit = editing();
      if (edit !== null) {
        await props.publish(await composeEditEnvelope(edit.cursor, text().trim()));
        props.onCancelEdit();
      } else {
        // Taken out of the tray only once the post is published: a refused
        // publish leaves the tray as it was, to send again.
        // ADR-0047 section 6 — the micro thumbnail stays on the first photo
        // or video (by type, however it was added) only.
        const refs: AttachmentRefBytes[] = messageRefs(tray.items(), (mime) => props.hub.inlineType(mime));
        await props.publish(await composeEnvelope(text().trim(), refs));
        for (const item of tray.take()) {
          if (item.ref !== null && item.kind !== "file") {
            props.hub.fileLocal(mediaKey(item.ref.blobId), item.file, item.ref.mime);
          }
          releaseTrayItem(item);
        }
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

  /** The draft key of this composer (one channel per mounted composer). */
  const draftKeyOf = (): string => "post";

  /** ADR-0092 — a poll, checklist, location, contact card or article post. */
  async function publishContent(content: MessageContent, refs: AttachmentRefBytes[] = []): Promise<void> {
    setBusy(true);
    setError(null);
    try {
      await props.publish(await composeContentEnvelope(content, refs));
    } catch (err) {
      setError(channelErrorText(err, () => t("channels.publish.error.generic")));
      throw err;
    } finally {
      setBusy(false);
    }
  }

  /** An op post (an edit of a content post). */
  async function publishOp(bytes: Uint8Array): Promise<void> {
    setBusy(true);
    setError(null);
    try {
      await props.publish(bytes);
    } catch (err) {
      setError(channelErrorText(err, () => t("channels.publish.error.generic")));
      throw err;
    } finally {
      setBusy(false);
    }
  }

  /** One image of an article, uploaded with the channel file life. */
  async function uploadOne(original: File): Promise<AttachmentRefBytes | null> {
    let file = original;
    try {
      if (file.type.startsWith("image/") && file.type !== "image/gif") {
        const w = await loadWasm();
        try {
          file = await reencodePhoto(file, w.photoMaxLongestSidePx());
        } catch {
          setError(t("attach.preview.photo_unreadable"));
          return null;
        }
      }
      return await encryptAndUploadFile(file, storage);
    } catch (err) {
      setError(
        err instanceof AttachmentSizeError
          ? t("attach.preview.file_too_big")
          : channelErrorText(err, () => t("channels.publish.error.generic")),
      );
      return null;
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
      <form
        class="lk-composer"
        classList={{ "with-tray": tray.items().length > 0 || tray.notice() !== null }}
        onSubmit={(e): void => {
          e.preventDefault();
          void onSend();
        }}
      >
        <Show when={editing() === null}>
          <ComposerTray tray={tray} disabled={busy()} />
        </Show>
        <Show when={!recorderBusy(recorder)} fallback={<RecordingBar recorder={recorder} />}>
        <Show when={editing() === null}>
          {/* docs/ux/attachments.md section 2 — the owner's and publishers'
              paperclip: the same menu as a chat (the core gives a channel's
              publishers all nine rows; a subscriber has no composer). */}
          <AttachComposer
            conversation={() => "channel"}
            role={() => "admin"}
            accounts={() => 0}
            callHoldsCamera={() => callOwnsCaptureNow(uiRules(), callHost)}
            hidden={() => busy()}
            draftKey={`channel:${draftKeyOf()}`}
            contacts={() => []}
            editRequest={() => props.contentEdit?.() ?? null}
            clearEditRequest={() => props.clearContentEdit?.()}
            addFiles={(files, source) => tray.add(files, source)}
            acceptPaste={() => true}
            sendPoll={async (d) => {
              const body = await pollBodyOf({ ...d, anonymous: false }, [], Date.now());
              await publishContent({ kind: "poll", poll: { body, authorSig: null } });
            }}
            sendChecklist={async (d, editTarget) => {
              const body = checklistBodyOf({ ...d, othersAdd: false, othersMark: false });
              if (editTarget === undefined) {
                await publishContent({ kind: "checklist", checklist: { body, authorSig: null } });
                return;
              }
              // UX 8.2 — the edit is a post naming the checklist's cursor.
              const w = await loadWasm();
              await publishOp(w.composeChecklistEditEnvelope(BigInt(editTarget), { body, authorSig: null }));
            }}
            sendLocation={(l) => publishContent({ kind: "location", location: l })}
            sendContact={(c) => publishContent({ kind: "contactCard", card: c })}
            sendArticle={async (out, editTarget) => {
              const refs: AttachmentRefBytes[] = [];
              for (const image of out.images) {
                const file = image instanceof File ? image : new File([image], "image.jpg", { type: image.type || "image/jpeg" });
                const ref = await uploadOne(file);
                if (ref === null) return;
                refs.push(ref);
              }
              if (editTarget === undefined) {
                await publishContent({ kind: "article", article: out.article }, refs);
                return;
              }
              // UX 11.3 — the edit replaces the whole article.
              await publishOp(await composeArticleEditEnvelope(editTarget, out.article, refs));
            }}
          />
        </Show>
        <textarea
          ref={field}
          data-paste-files=""
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
            if (
              editing() === null &&
              backspaceRemovesLast({
                key: e.key,
                fieldEmpty: e.currentTarget.value.length === 0,
                modified: e.altKey || e.ctrlKey || e.metaKey || e.shiftKey,
                composing: e.isComposing,
                items: tray.items().length,
              })
            ) {
              e.preventDefault();
              tray.removeLast();
              return;
            }
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
          disabled={busy() || empty() || !sendable()}
        >
          <Icon name="send-fill" size={20} />
        </button>
        </Show>
      </form>
    </div>
  );
}

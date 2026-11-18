// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/attachments.md` — the paperclip of a composer and every screen it
 * opens: the system pickers (Photo or Video, File, Audio) behind the send
 * preview, Lakko's camera, and the poll, checklist, location, contact and
 * article screens. The conversation that mounts it does the sending; this
 * component only decides which screen is up and hands back drafts.
 *
 * Used by the chat composer and by the channel composer (owner and
 * publishers); a channel subscriber gets no paperclip (the core's rows are
 * empty for it).
 */
import { createEffect, createMemo, createResource, createSignal, Match, on, Show, Switch, type JSX } from "solid-js";
import { Portal } from "solid-js/web";

import { wasmSync } from "../wasm";
import { ConversationKind } from "../../wasm/lakko_ffi_wasm";
import type { Article, MessageContent, SharedContactCard, SharedLocation } from "../../wasm/lakko_ffi_wasm";
import { AttachMenu } from "./AttachMenu";
import { deviceHasCamera, deviceHasLocation, menuEntriesOf } from "./attach-menu";
import { SendPreview } from "./SendPreview";
import { CameraScreen } from "./CameraScreen";
import { PollEditor } from "./PollEditor";
import { ChecklistEditor } from "./ChecklistEditor";
import { LocationScreen } from "./LocationScreen";
import { ContactPicker } from "./ContactPicker";
import { ArticleEditor } from "./ArticleEditor";
import { deleteArticleDraft, loadArticleDraft, saveArticleDraft } from "./article-drafts";
import { boundContactsAccount } from "../contacts/local-store";
import type {
  ArticleDraftOut,
  AttachItem,
  AttachMenuEntry,
  ChecklistDraft,
  PollAnonymityMode,
  PollDraft,
  PreviewResult,
  ShareableContact,
} from "./types";

export type AttachConversation = "chat" | "group" | "channel";

/** An edit the conversation asks the composer to open (`Edit checklist`,
 *  `Edit` on an article). */
export interface AttachEditRequest {
  readonly messageId: number;
  readonly content: MessageContent;
  /** An article's images, already downloaded by the caller. */
  readonly images?: readonly Blob[];
}

export interface AttachComposerProps {
  readonly conversation: () => AttachConversation;
  readonly role: () => "owner" | "admin" | "member";
  /** Distinct accounts in the conversation (the poll editor's anonymity). */
  readonly accounts: () => number;
  /** A live call holds the camera. */
  readonly callHoldsCamera: () => boolean;
  /** The composer is busy or editing a message: the paperclip is hidden. */
  readonly hidden: () => boolean;
  readonly draftKey: string;
  readonly contacts: () => readonly ShareableContact[];
  readonly editRequest: () => AttachEditRequest | null;
  readonly clearEditRequest: () => void;
  sendFiles(r: PreviewResult, mode: "media" | "file" | "audio"): Promise<void>;
  sendPoll(d: PollDraft): Promise<void>;
  sendChecklist(d: ChecklistDraft, editTarget: number | undefined): Promise<void>;
  sendLocation(l: SharedLocation): Promise<void>;
  sendContact(c: SharedContactCard): Promise<void>;
  sendArticle(out: ArticleDraftOut, editTarget: number | undefined): Promise<void>;
}

type Screen =
  | { readonly kind: "none" }
  | { readonly kind: "preview"; readonly files: readonly File[]; readonly mode: "media" | "file" | "audio"; readonly fromCamera: boolean }
  | { readonly kind: "camera" }
  | { readonly kind: "poll" }
  | { readonly kind: "checklist"; readonly target?: number; readonly initial?: ChecklistDraft }
  | { readonly kind: "location" }
  | { readonly kind: "contact" }
  | { readonly kind: "article"; readonly target?: number; readonly initial?: { article: Article; images: readonly Blob[] } };


function kindOf(c: AttachConversation): ConversationKind {
  return c === "chat" ? ConversationKind.Chat : c === "group" ? ConversationKind.Group : ConversationKind.Channel;
}

export function AttachComposer(props: AttachComposerProps): JSX.Element {
  const [screen, setScreen] = createSignal<Screen>({ kind: "none" });
  const [hasCamera] = createResource(deviceHasCamera, { initialValue: true });
  let photoInput: HTMLInputElement | undefined;
  let fileInput: HTMLInputElement | undefined;
  let audioInput: HTMLInputElement | undefined;

  const entries = createMemo<AttachMenuEntry[]>(() => {
    if (props.hidden()) return [];
    try {
      const rows = wasmSync().attachmentMenu(
        kindOf(props.conversation()),
        props.role(),
        hasCamera() ?? true,
        deviceHasLocation(),
        props.callHoldsCamera(),
      );
      const plain = rows.map((r) => {
        const v = { item: r.item, section: r.section, disabled: r.disabled };
        r.free();
        return v;
      });
      return menuEntriesOf(plain);
    } catch {
      return [];
    }
  });

  const anonymity = (): PollAnonymityMode => {
    try {
      const m = wasmSync().pollAnonymity(kindOf(props.conversation()), props.accounts());
      return m === "switch-default-on" || m === "always-anonymous" ? m : "not-offered";
    } catch {
      return "not-offered";
    }
  };

  createEffect(
    on(props.editRequest, (req) => {
      if (req === null) return;
      props.clearEditRequest();
      if (req.content.kind === "checklist") {
        const body = req.content.checklist.body;
        setScreen({
          kind: "checklist",
          target: req.messageId,
          initial: {
            title: body.title,
            othersAdd: body.othersAdd,
            othersMark: body.othersMark,
            items: body.items.map((i) => ({ id: i.id, text: i.text })),
          },
        });
      } else if (req.content.kind === "article") {
        setScreen({ kind: "article", target: req.messageId, initial: { article: req.content.article, images: req.images ?? [] } });
      }
    }),
  );

  const pick = (item: AttachItem): void => {
    switch (item) {
      case "photo-or-video":
        photoInput?.click();
        return;
      case "file":
        fileInput?.click();
        return;
      case "audio":
        audioInput?.click();
        return;
      case "camera":
        setScreen({ kind: "camera" });
        return;
      case "poll":
        setScreen({ kind: "poll" });
        return;
      case "checklist":
        setScreen({ kind: "checklist" });
        return;
      case "location":
        setScreen({ kind: "location" });
        return;
      case "contact":
        setScreen({ kind: "contact" });
        return;
      case "article": {
        // UX 11.1 — the conversation's saved draft, when there is one.
        const account = boundContactsAccount();
        void (async () => {
          const draft = account === null ? null : await loadArticleDraft(account, props.draftKey);
          setScreen(draft === null ? { kind: "article" } : { kind: "article", initial: { article: draft.article, images: draft.images } });
        })();
        return;
      }
    }
  };

  const picked = (e: Event, mode: "media" | "file" | "audio"): void => {
    const input = e.currentTarget as HTMLInputElement;
    const files = input.files === null ? [] : Array.from(input.files);
    input.value = "";
    if (files.length > 0) setScreen({ kind: "preview", files, mode, fromCamera: false });
  };
  const close = (): void => {
    setScreen({ kind: "none" });
  };
  const run = async (f: () => Promise<void>): Promise<void> => {
    await f();
    close();
  };

  const isChannel = (): boolean => props.conversation() === "channel";
  const limits = () => {
    try {
      return wasmSync().contentLimits();
    } catch {
      return null;
    }
  };

  return (
    <>
      <AttachMenu entries={entries} onPick={pick} />
      <input ref={photoInput} type="file" accept="image/*,video/*" multiple hidden onChange={(e) => picked(e, "media")} />
      <input ref={fileInput} type="file" multiple hidden onChange={(e) => picked(e, "file")} />
      <input ref={audioInput} type="file" accept="audio/*" multiple hidden onChange={(e) => picked(e, "audio")} />
      <Show when={screen().kind !== "none"}>
        <Portal>
          <Switch>
            <Match when={screen().kind === "preview" ? (screen() as Extract<Screen, { kind: "preview" }>) : null}>
              {(s) => (
                <SendPreview
                  files={s().files}
                  mode={s().mode}
                  onCancel={close}
                  onSend={(r) => run(() => props.sendFiles(r, s().mode))}
                  {...(s().fromCamera ? { onRetake: () => setScreen({ kind: "camera" }) } : {})}
                />
              )}
            </Match>
            <Match when={screen().kind === "camera"}>
              <CameraScreen
                onCancel={close}
                onCaptured={(files) => setScreen({ kind: "preview", files, mode: "media", fromCamera: true })}
              />
            </Match>
            <Match when={screen().kind === "poll"}>
              <PollEditor anonymity={anonymity()} onCancel={close} onCreate={(d) => run(() => props.sendPoll(d))} />
            </Match>
            <Match when={screen().kind === "checklist" ? (screen() as Extract<Screen, { kind: "checklist" }>) : null}>
              {(s) => (
                <ChecklistEditor
                  showOthersAdd={!isChannel()}
                  showOthersMark={!isChannel()}
                  {...(s().initial === undefined ? {} : { initial: s().initial })}
                  onCancel={close}
                  onSubmit={(d) => run(() => props.sendChecklist(d, s().target))}
                />
              )}
            </Match>
            <Match when={screen().kind === "location"}>
              <LocationScreen onCancel={close} onSend={(l) => run(() => props.sendLocation(l))} />
            </Match>
            <Match when={screen().kind === "contact"}>
              <ContactPicker contacts={props.contacts()} onCancel={close} onSend={(c) => run(() => props.sendContact(c))} />
            </Match>
            <Match when={screen().kind === "article" ? (screen() as Extract<Screen, { kind: "article" }>) : null}>
              {(s) => (
                <ArticleEditor
                  {...(s().initial === undefined ? {} : { initial: s().initial })}
                  draftKey={props.draftKey}
                  maxImages={limits()?.maxArticleImages ?? 20}
                  maxTextBytes={limits()?.maxArticleTextBytes ?? 65_536}
                  urlValid={(url) => {
                    try {
                      return wasmSync().articleUrlValid(url);
                    } catch {
                      return false;
                    }
                  }}
                  editing={s().target !== undefined}
                  onCancel={close}
                  onPublish={(out) =>
                    run(async () => {
                      await props.sendArticle(out, s().target);
                      const account = boundContactsAccount();
                      if (s().target === undefined && account !== null) {
                        await deleteArticleDraft(account, props.draftKey).catch(() => undefined);
                      }
                    })
                  }
                  onSaveDraft={(out) => {
                    const account = boundContactsAccount();
                    if (account !== null) void saveArticleDraft(account, props.draftKey, out).catch(() => undefined);
                    close();
                  }}
                />
              )}
            </Match>
          </Switch>
        </Portal>
      </Show>
    </>
  );
}

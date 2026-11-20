// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * One report, opened in the browser with the moderator's report key, and its
 * included evidence. Each message is marked `Written by this account` or
 * `Not verified` (greyed); photos and videos stay blurred until clicked; a
 * file opens only on an explicit click. What was verified is posted back so
 * the queue can put checked cases first.
 */

import { For, type JSX, Match, Show, Switch, createResource, createSignal, onCleanup } from "solid-js";

import { api } from "../api/client";
import { ApiError, blobErrorMessage, coreErrorMessage, errorMessage } from "../api/errors";
import type { ReportRow } from "../api/types";
import { b64ToBytes, bytesEqual, bytesToHex, toBuffer } from "../model/bytes";
import {
  attachmentGate,
  attachmentLabel,
  blobContentType,
  fileProblem,
  saveName,
  checksNeedPosting,
  countChecks,
  evidenceMark,
  isGreyed,
  markLabel,
  type EvidenceMark,
} from "../model/evidence";
import { conversationKindLabel, reasonLabel, reportStatusLabel } from "../model/names";
import { requireSession } from "../session";
import { BusyButton, Notice } from "../ui";
import { loadWasm, type Wasm } from "../wasm";

interface AttachmentView {
  readonly label: string;
  readonly gate: "blurred" | "on-request";
  readonly inline: "image" | "video" | "audio" | null;
  readonly blobId: Uint8Array;
  readonly key: Uint8Array;
  readonly nonce: Uint8Array;
  readonly mime: string | undefined;
  readonly name: string | undefined;
}

type ItemBody =
  | { readonly kind: "message"; readonly text: string; readonly note: string | null; readonly attachments: readonly AttachmentView[] }
  | { readonly kind: "other"; readonly title: string; readonly text: string };

interface ItemView {
  readonly mark: EvidenceMark;
  readonly body: ItemBody;
}

interface OpenedView {
  readonly comment: string;
  readonly items: readonly ItemView[];
}

function inlineCategory(w: Wasm, mime: string | undefined): "image" | "video" | "audio" | null {
  if (mime === undefined) return null;
  const t = w.inlineMediaType(mime);
  if (t === w.InlineMediaType.Image) return "image";
  if (t === w.InlineMediaType.Video) return "video";
  if (t === w.InlineMediaType.Audio) return "audio";
  return null;
}

function messageBody(w: Wasm, content: Uint8Array): ItemBody {
  const parsed = w.parseBodyEnvelope(content);
  try {
    const attachments: AttachmentView[] = [];
    for (let i = 0; i < parsed.attachmentCount; i += 1) {
      const a = parsed.attachment(i);
      const inline = inlineCategory(w, a.mime);
      attachments.push({
        label: attachmentLabel({ kind: a.kind, inline, name: a.name, size: a.size }),
        gate: attachmentGate(inline),
        inline,
        blobId: new Uint8Array(a.blobId),
        key: new Uint8Array(a.key),
        nonce: new Uint8Array(a.nonce),
        mime: a.mime,
        name: a.name,
      });
      a.free();
    }
    let note: string | null = null;
    if (parsed.forwardedFrom !== undefined) note = `Forwarded from ${parsed.forwardedFrom}`;
    if (parsed.replyExcerpt !== undefined) note = `In reply to: ${parsed.replyExcerpt}`;
    switch (parsed.kind) {
      case "chat":
        return { kind: "message", text: parsed.text, note, attachments };
      case "edit":
        return { kind: "message", text: parsed.text, note: "An edit of an earlier message", attachments };
      case "delete":
        return { kind: "message", text: "", note: "Deleted an earlier message", attachments };
      case "reaction":
        return { kind: "message", text: parsed.emoji ?? "", note: "A reaction", attachments };
      default:
        return { kind: "message", text: "", note: "A message the panel can't show", attachments };
    }
  } finally {
    parsed.free();
  }
}

function otherBody(w: Wasm, kind: string | undefined, content: Uint8Array): ItemBody {
  switch (kind) {
    case "contact-request": {
      try {
        w.ContactRequest.decodeAndVerify(content).free();
        return {
          kind: "other",
          title: "Contact request",
          text: "Its introduction is sealed to the person it was sent to, so it can't be read here.",
        };
      } catch {
        return { kind: "other", title: "Contact request", text: "This request is damaged: its signature doesn't check out." };
      }
    }
    case "channel-post":
      return { kind: "other", title: "Channel post", text: "Posts open only with the channel's key, so it can't be read here." };
    case "listing": {
      try {
        const l = w.decodeChannelListing(content);
        const text = `@${l.handle} · ${l.name}${l.description === "" ? "" : `\n${l.description}`}`;
        l.free();
        return { kind: "other", title: "Discover listing", text };
      } catch {
        return { kind: "other", title: "Discover listing", text: "This listing is damaged." };
      }
    }
    default:
      return { kind: "other", title: "Item", text: "The panel doesn't know this kind of item." };
  }
}

async function openReport(report: ReportRow): Promise<OpenedView> {
  const w = await loadWasm();
  const s = requireSession();
  let opened;
  try {
    opened = w.openModerationReport(b64ToBytes(report.report_b64), s.moderatorId, s.reportSecret);
  } catch (e) {
    throw new Error(coreErrorMessage(e, "report"));
  }
  try {
    const items: ItemView[] = [];
    const marks: { kind: string | undefined; isByTarget: boolean }[] = [];
    for (let i = 0; i < opened.evidenceCount; i += 1) {
      const kind = opened.evidenceKind(i);
      const isByTarget = opened.evidenceIsByTarget(i);
      marks.push({ kind, isByTarget });
      const content = opened.evidenceContent(i) ?? new Uint8Array();
      let body: ItemBody;
      try {
        body = kind === "message" ? messageBody(w, content) : otherBody(w, kind, content);
      } catch {
        body = { kind: "other", title: "Item", text: "This item couldn't be read." };
      }
      items.push({ mark: evidenceMark(kind, isByTarget), body });
    }
    const computed = countChecks(marks);
    if (checksNeedPosting(report.checks, computed)) {
      // Best effort: the queue order uses it, the case does not.
      void api.postChecks(report.receipt_hex, computed).catch(() => undefined);
    }
    return { comment: opened.comment, items };
  } finally {
    opened.free();
  }
}

type FileState =
  | { readonly kind: "idle" }
  | { readonly kind: "loading" }
  | { readonly kind: "ready"; readonly url: string }
  | { readonly kind: "failed"; readonly message: string };

/** Fetch the ciphertext, check it is the named blob, decrypt it here. */
async function loadFile(view: AttachmentView): Promise<string> {
  const ciphertext = await api.blob(bytesToHex(view.blobId));
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", toBuffer(ciphertext)));
  if (!bytesEqual(digest, view.blobId)) throw new Error(fileProblem("mismatch"));
  const w = await loadWasm();
  let plain: Uint8Array;
  try {
    plain = w.decryptAttachment(ciphertext, view.key, view.nonce);
  } catch {
    throw new Error(fileProblem("decrypt"));
  }
  return URL.createObjectURL(new Blob([toBuffer(plain)], { type: blobContentType(view.inline, view.mime) }));
}

/**
 * One attachment. Photos and videos show a blurred placeholder until the
 * moderator clicks Show; audio and files are fetched only on an explicit
 * click, and a file is then offered to save, never opened by the browser.
 */
function Attachment(props: { readonly view: AttachmentView }): JSX.Element {
  const [state, setState] = createSignal<FileState>({ kind: "idle" });
  onCleanup(() => {
    const st = state();
    if (st.kind === "ready") URL.revokeObjectURL(st.url);
  });
  const fetchIt = async (): Promise<void> => {
    setState({ kind: "loading" });
    try {
      setState({ kind: "ready", url: await loadFile(props.view) });
    } catch (e) {
      setState({ kind: "failed", message: e instanceof ApiError ? blobErrorMessage(e) : e instanceof Error ? e.message : errorMessage(e) });
    }
  };
  const ready = (): string | null => {
    const st = state();
    return st.kind === "ready" ? st.url : null;
  };
  return (
    <div class="mp-attachment">
      <Show when={props.view.gate === "blurred" && ready() === null}>
        <div class="mp-attachment-blur" aria-hidden="true" />
      </Show>
      <div style={{ flex: "1", "min-width": "0", display: "flex", "flex-direction": "column", gap: "6px" }}>
        <div>{props.view.label}</div>
        <Show when={ready()}>
          {(url) => (
            <Switch
              fallback={
                <a href={url()} download={saveName(props.view.name)}>
                  Save the file
                </a>
              }
            >
              <Match when={props.view.inline === "image"}>
                <img class="mp-attachment-media" src={url()} alt={props.view.label} />
              </Match>
              <Match when={props.view.inline === "video"}>
                <video class="mp-attachment-media" src={url()} controls preload="metadata" />
              </Match>
              <Match when={props.view.inline === "audio"}>
                <audio src={url()} controls preload="metadata" />
              </Match>
            </Switch>
          )}
        </Show>
        <Show when={state().kind === "failed"}>
          <div class="mp-row-sub" role="alert">
            {(state() as Extract<FileState, { kind: "failed" }>).message}
          </div>
        </Show>
      </div>
      <Show when={state().kind !== "ready"}>
        <BusyButton class="small" busy={state().kind === "loading"} onClick={() => void fetchIt()}>
          {state().kind === "failed" ? "Try again" : props.view.gate === "blurred" ? "Show" : "Open file"}
        </BusyButton>
      </Show>
    </div>
  );
}

function Item(props: { readonly item: ItemView; readonly index: number }): JSX.Element {
  return (
    <li class="mp-evidence-item" data-greyed={isGreyed(props.item.mark) ? "true" : "false"}>
      <span class="mp-mark" data-mark={props.item.mark}>
        {props.index + 1}. {markLabel(props.item.mark)}
      </span>
      <Switch>
        <Match when={props.item.body.kind === "message" && props.item.body}>
          {(b) => {
            const body = b() as Extract<ItemBody, { kind: "message" }>;
            return (
              <>
                <Show when={body.note}>{(n) => <span class="mp-row-sub">{n()}</span>}</Show>
                <Show when={body.text !== ""}>
                  <div class="mp-evidence-text">{body.text}</div>
                </Show>
                <For each={body.attachments}>{(a) => <Attachment view={a} />}</For>
              </>
            );
          }}
        </Match>
        <Match when={props.item.body.kind === "other" && props.item.body}>
          {(b) => {
            const body = b() as Extract<ItemBody, { kind: "other" }>;
            return (
              <>
                <strong style={{ "font-size": "var(--text-sm)" }}>{body.title}</strong>
                <div class="mp-evidence-text">{body.text}</div>
              </>
            );
          }}
        </Match>
      </Switch>
    </li>
  );
}

export function ReportCard(props: { readonly report: ReportRow; readonly index: number }): JSX.Element {
  const [opened] = createResource(() => props.report, openReport);
  return (
    <section class="lk-card">
      <div style={{ display: "flex", "justify-content": "space-between", gap: "12px", "flex-wrap": "wrap" }}>
        <strong>
          Report {props.index + 1}: {reasonLabel(props.report.reason)}
        </strong>
        <span class="mp-row-sub">
          {props.report.day} · {conversationKindLabel(props.report.conversation_kind)} ·{" "}
          {reportStatusLabel(props.report.status)}
        </span>
      </div>
      <Switch>
        <Match when={opened.error !== undefined && opened.error !== null}>
          <Notice tone="warning">
            {opened.error instanceof Error ? opened.error.message : "This report couldn't be opened."}
          </Notice>
        </Match>
        <Match when={opened.loading}>
          <p class="form-info">Opening the report...</p>
        </Match>
        <Match when={opened()}>
          {(view) => (
            <>
              <Show
                when={view().comment !== ""}
                fallback={<p class="mp-row-sub" style={{ margin: "0" }}>No comment.</p>}
              >
                <p class="mp-evidence-text" style={{ margin: "0" }}>
                  {view().comment}
                </p>
              </Show>
              <Show
                when={view().items.length > 0}
                fallback={<p class="mp-row-sub" style={{ margin: "0" }}>Nothing included.</p>}
              >
                <ol class="mp-evidence">
                  <For each={view().items}>{(item, i) => <Item item={item} index={i()} />}</For>
                </ol>
              </Show>
            </>
          )}
        </Match>
      </Switch>
    </section>
  );
}

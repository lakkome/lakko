// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `/saved` — the account's saved messages (ADR-0065 section 4,
 * docs/ux/saved-messages-and-file-lifetime.md section 3), reached from the
 * Chats screen's menu.
 *
 * Most recently saved first. A row names the chat, the author, and the
 * message text or its media kind, with the message date; a held photo or
 * video shows its thumbnail. Tap opens the chat at the message;
 * long-press or right-click offers `Remove from saved`. A message this
 * device does not hold yet is drawn from what was saved with it and does
 * not open. The rules are `chat/saved-messages.ts`.
 */
import { createMemo, createResource, createSignal, For, onMount, Show, type JSX } from "solid-js";
import { useNavigate } from "@solidjs/router";

import { useAuth } from "../auth/AuthContext";
import { base64Encode, base64UrlEncodeNoPad } from "../auth/account-id";
import { getDeviceKey } from "../auth/key-store";
import { downloadAndDecryptRef } from "../chat/attachments";
import { useChats } from "../chat/ChatContext";
import { conversationKindOf } from "../chat/conversation-kind";
import { conversationTitle } from "../chat/conversation-title";
import { useGroupNames } from "../chat/GroupNameContext";
import { groupTitle } from "../chat/group-title";
import { formatListTime } from "../chat/list-time";
import { loadMessageLog, type PersistedGroup, type PersistedMessage } from "../chat/message-log-store";
import { ownPersonKeyB64, personKeysOf } from "../chat/person-key";
import { channelRoleOf } from "../channels/channel-access";
import { channelFacts } from "../channels/channel-facts";
import { savedKindLabel, savedOpenRow, savedRowDateMs, savedRows, unsaveEdit, type SavedRow } from "../chat/saved-messages";
import { useLineage } from "../chat/use-lineage";
import type { ActiveGroup } from "../chat/ChatContext";
import { bytesToHex } from "../chat/blob-ids";
import { useDeviceGroupSync } from "../devices/sync/DeviceGroupSyncContext";
import { t } from "../l10n";
import { memberNameOf } from "../profile/member-label";
import { useProfiles } from "../profile/ProfileContext";
import { createLongPress } from "../ui/long-press";
import { Icon } from "../ui/icons";
import { paths } from "./paths";
import { displayLocale } from "../l10n/display-locale";
import { MenuBackdrop } from "../ui/MenuBackdrop";

/** The stored message a held row is, or `undefined`. */
function heldMessage(
  groups: Readonly<Record<string, PersistedGroup>>,
  refId: Uint8Array,
  messageId: number,
): PersistedMessage | undefined {
  return groups[bytesToHex(refId)]?.messages.find((m) => Number(m.messageId) === messageId);
}

function b64(b: string): Uint8Array {
  const bin = atob(b);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

/** A held photo's or video's thumbnail, loaded once. */
function SavedThumb(props: { readonly message: PersistedMessage }): JSX.Element {
  const [url] = createResource(async (): Promise<string | null> => {
    const a = props.message.attachments[0];
    const thumb = a?.thumb ?? null;
    const source = thumb ?? (a !== undefined && (a.mime ?? "").startsWith("image/") ? a : null);
    if (source === null) return null;
    try {
      const bytes = await downloadAndDecryptRef({
        blobId: b64(source.blobIdB64),
        key: b64(source.keyB64),
        nonce: b64(source.nonceB64),
        mime: source.mime ?? undefined,
        size: 0,
        name: undefined,
      });
      return URL.createObjectURL(new Blob([bytes.slice()], { type: source.mime ?? "image/jpeg" }));
    } catch {
      return null;
    }
  });
  return (
    <Show when={url()}>{(u) => <img class="lk-saved-thumb" src={u()} alt="" draggable={false} />}</Show>
  );
}

export default function Saved(): JSX.Element {
  const auth = useAuth();
  const chats = useChats();
  const sync = useDeviceGroupSync();
  const profiles = useProfiles();
  const groupNames = useGroupNames();
  const lineageView = useLineage();
  const navigate = useNavigate();
  const [groups, setGroups] = createSignal<Readonly<Record<string, PersistedGroup>>>({});
  const [menu, setMenu] = createSignal<{ row: SavedRow; x: number; y: number } | null>(null);
  const [error, setError] = createSignal<string | null>(null);

  onMount(() => {
    void (async () => {
      const s = auth.state();
      if (s.kind !== "session") return;
      const key = await getDeviceKey(s.account.accountId);
      if (key === null) return;
      const log = await loadMessageLog(s.account.accountId, key);
      if (log !== null) setGroups(log.groups);
    })();
  });

  /** The conversation that holds the segment `segmentIdB64` (its head, and
   *  its older segments oldest first), through the lineage register; `null`
   *  when this device does not know the segment. */
  const conversationOf = (segmentIdB64: string): { head: ActiveGroup; historyIdsB64: string[] } | null => {
    const view = lineageView.resolve(segmentIdB64);
    if (view !== undefined) return { head: view.head, historyIdsB64: view.history.map((g) => g.groupIdB64) };
    const entry = chats.get(segmentIdB64);
    return entry === undefined ? null : { head: entry, historyIdsB64: [] };
  };

  const titleOf = (segmentIdB64: string): string | null => {
    const entry = conversationOf(segmentIdB64)?.head;
    if (entry === undefined) return null;
    const groupIdB64 = entry.groupIdB64;
    const ownB64 = ownPersonKeyB64(chats.identity());
    const memberKeysB64 = personKeysOf(entry.group);
    return conversationTitle({
      kind: conversationKindOf(entry.group),
      memberKeysB64,
      ownKeyB64: ownB64,
      fallbackLabel: entry.label,
      sharedName: groupNames.nameOf(groupIdB64),
      contactNameById: undefined,
      contactNameByMember: undefined,
      nameOf: (k) => memberNameOf(profiles.resolve(k), "a Saved row title"),
      groupTitle,
    });
  };

  const rows = createMemo(() =>
    savedRows(
      sync.saved(),
      // ADR-0087 section 12 — a saved post of a channel (ref kind 3) is
      // titled by the channel and opens it while this account follows it.
      (ref) => (ref.kind === 3 ? channelFacts(bytesToHex(ref.id)).name : titleOf(base64Encode(ref.id))),
      (ref, id) =>
        ref.kind === 3 ? channelRoleOf(bytesToHex(ref.id)) !== null : heldMessage(groups(), ref.id, id) !== undefined,
    ),
  );

  function open(row: SavedRow): void {
    if (!row.held) return;
    if (row.ref.kind === 3) {
      navigate(paths.channel(bytesToHex(row.ref.id)));
      return;
    }
    const segmentIdB64 = base64Encode(row.ref.id);
    const conversation = conversationOf(segmentIdB64);
    if (conversation === null) return;
    const entry = conversation.head;
    // The head's own DS id, or the history id an older segment's row is
    // drawn under in the conversation.
    const at = savedOpenRow(segmentIdB64, row.messageId, entry.groupIdB64, conversation.historyIdsB64);
    if (at === null) return;
    const slug = base64UrlEncodeNoPad(entry.group.groupId);
    const href = conversationKindOf(entry.group) === "chat" ? paths.chat(slug) : paths.groupChat(slug);
    navigate(`${href}?at=${String(at)}`);
  }

  async function remove(row: SavedRow): Promise<void> {
    setMenu(null);
    setError(null);
    try {
      await sync.applyFolderEdits([unsaveEdit(row.ref, row.messageId)]);
    } catch {
      setError(t("chat.error.save"));
    }
  }

  const lineOf = (row: SavedRow): string => {
    const label = savedKindLabel(row.kind, row.excerpt);
    if (label === null) return row.excerpt;
    const [key, args] = label;
    return row.kind !== "file" && row.excerpt.length > 0 ? `${t(key, args)} · ${row.excerpt}` : t(key, args);
  };

  return (
    <section class="lk-page lk-saved">
      {/* Saved opens from the Chats menu and is not a tab: without this it had
          no way back on any width (base scenario 30, 2026-11-11). */}
      <button
        type="button"
        class="ghost small"
        style={{ "align-self": "flex-start" }}
        onClick={() => navigate(paths.home)}
      >
        <Icon name="back" size={16} />
        {t("nav.chats")}
      </button>
      <div class="lk-page-head">
        <h1>{t("saved.title")}</h1>
      </div>
      <Show when={error()}>{(m) => <div class="form-error" role="alert">{m()}</div>}</Show>
      <Show when={rows().length > 0} fallback={<p class="lk-pane-nomatch">{t("saved.empty")}</p>}>
        <ul class="lk-saved-list">
          <For each={rows()}>
            {(row) => {
              const press = createLongPress((x, y) => setMenu({ row, x, y }));
              const message = (): PersistedMessage | undefined => heldMessage(groups(), row.ref.id, row.messageId);
              const date = (): string => {
                const held = message()?.createdAt;
                const heldMs = held === undefined ? null : new Date(held).getTime();
                return formatListTime(new Date(savedRowDateMs(row, heldMs)).toISOString(), new Date(), displayLocale());
              };
              return (
                <li>
                  <button
                    type="button"
                    class="lk-row lk-saved-row"
                    classList={{ unheld: !row.held }}
                    aria-disabled={!row.held}
                    onClick={(e) => {
                      if (!press.swallowClick(e)) open(row);
                    }}
                    onContextMenu={(e) => {
                      e.preventDefault();
                      setMenu({ row, x: e.clientX, y: e.clientY });
                    }}
                    onPointerDown={press.onPointerDown}
                    onPointerMove={press.onPointerMove}
                    onPointerUp={press.onPointerUp}
                    onPointerCancel={press.onPointerCancel}
                    onPointerLeave={press.onPointerLeave}
                  >
                    <Show when={row.held && (row.kind === "photo" || row.kind === "video") ? message() : undefined}>
                      {(m) => <SavedThumb message={m()} />}
                    </Show>
                    <div class="lk-row-body">
                      <div class="lk-row-line">
                        <span class="lk-row-title">
                          <span class="lk-ellipsis">{row.chatTitle ?? row.author}</span>
                        </span>
                        <span class="lk-row-time">{date()}</span>
                      </div>
                      <div class="lk-row-line">
                        <span class="lk-row-sub lk-ellipsis">
                          {row.author.length > 0 ? t("chats.preview.sender", { name: row.author, text: lineOf(row) }) : lineOf(row)}
                        </span>
                      </div>
                    </div>
                  </button>
                </li>
              );
            }}
          </For>
        </ul>
      </Show>
      <Show when={menu()}>
        {(m) => (
          <>
            <MenuBackdrop onClose={() => setMenu(null)} />
            <div
              class="lk-menu lk-menu-floating"
              role="menu"
              style={{ left: `${m().x}px`, top: `${m().y}px` }}
            >
              <button type="button" role="menuitem" onClick={() => void remove(m().row)}>
                <Icon name="bookmark" size={16} />
                {t("chat.message.unsave")}
              </button>
            </div>
          </>
        )}
      </Show>
    </section>
  );
}

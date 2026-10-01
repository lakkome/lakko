// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Import a shared feed (`docs/ux/feed.md` section 4.1): from a link, whose
 * fragment carries the rule (no server sees it), or from a `.lakkofeed`
 * file. The person sees a preview of the rules before anything is saved;
 * anything the core refuses is refused whole: "This file isn't a feed Lakko
 * can open."
 *
 * A file opened from another app (Files, mail, a messenger) reaches the same
 * screen through the installed app's file handler and the launch queue.
 */

import { createSignal, For, onMount, Show, type JSX } from "solid-js";
import { useNavigate } from "@solidjs/router";

import { useAuth } from "../auth/AuthContext";
import { getDeviceKey } from "../auth/key-store";
import { bytesFromFragment, decodeDraft, draftSummary, encodeDraft, type FeedDraft } from "../feed/feed-draft";
import { feedIconName } from "../feed/feed-icons";
import { loadFeedState, newFeedId, saveFeedState, upsertFeed } from "../feed/feed-store";
import { Icon } from "../ui/icons";
import { loadWasm, type Wasm } from "../wasm";
import { writeFeedSelection } from "../feed/feed-selection-store";
import { paths } from "./paths";

const REFUSED = "This file isn't a feed Lakko can open.";

/** The slice of the File Handling API's launch queue this route uses (not in
 *  the TypeScript DOM library yet). */
interface LaunchQueueLike {
  setConsumer(consumer: (params: { readonly files?: ReadonlyArray<{ getFile(): Promise<File> }> }) => void): void;
}

export default function FeedImport(): JSX.Element {
  const auth = useAuth();
  const navigate = useNavigate();
  const [core, setCore] = createSignal<Wasm | null>(null);
  const [draft, setDraft] = createSignal<FeedDraft | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);

  const open = (w: Wasm, bytes: Uint8Array): void => {
    try {
      setDraft(decodeDraft(w, bytes));
      setError(null);
    } catch {
      setDraft(null);
      setError(REFUSED);
    }
  };

  onMount(() => {
    void loadWasm().then((w) => {
      setCore(() => w);
      const bytes = bytesFromFragment(window.location.hash);
      if (bytes !== null) open(w, bytes);
      else if (window.location.hash.length > 1) setError(REFUSED);
      // UX spec section 5 — a `.lakkofeed` opened from another app lands
      // here: the installed app's file handler (`manifest.webmanifest`
      // `file_handlers`) launches this route and hands the file over through
      // the launch queue. Browsers without it never call the consumer.
      const queue = (window as unknown as { launchQueue?: LaunchQueueLike }).launchQueue;
      queue?.setConsumer((params) => {
        const handle = params.files?.[0];
        if (handle === undefined) return;
        void handle
          .getFile()
          .then((file) => onFile(file))
          .catch(() => {
            setDraft(null);
            setError(REFUSED);
          });
      });
    });
  });

  const onFile = async (file: File | undefined): Promise<void> => {
    const w = core();
    if (w === null || file === undefined) return;
    // The core refuses anything larger than a rule can be; reading far more
    // than that is pointless, so the file is cut off well above it.
    if (file.size > w.feedRuleMaxLen() * 2) {
      setDraft(null);
      setError(REFUSED);
      return;
    }
    open(w, new Uint8Array(await file.arrayBuffer()));
  };

  const save = async (): Promise<void> => {
    const w = core();
    const d = draft();
    const s = auth.state();
    if (w === null || d === null || s.kind !== "session") return;
    setBusy(true);
    try {
      const deviceKey = await getDeviceKey(s.account.accountId);
      if (deviceKey === null) throw new Error("no device key");
      const state = await loadFeedState(s.account.accountId, deviceKey);
      const id = newFeedId();
      await saveFeedState(
        s.account.accountId,
        deviceKey,
        upsertFeed(state, { id, rule: encodeDraft(w, d) }),
      );
      // The feed opens on its default when storage refuses; the saved feed
      // is still in the row.
      writeFeedSelection(s.account.accountId, `custom:${id}`);
      navigate(paths.feed);
    } catch {
      setError("Couldn't save the feed. Try again.");
    } finally {
      setBusy(false);
    }
  };

  return (
    <section class="lk-feed-import" aria-label="Import feed">
      <h1>Add a shared feed</h1>
      <Show
        when={draft()}
        fallback={
          <>
            <p class="lk-feed-hint">Open a feed someone shared with you as a .lakkofeed file.</p>
            <input
              type="file"
              accept=".lakkofeed,application/octet-stream"
              onChange={(e) => void onFile(e.currentTarget.files?.[0])}
            />
          </>
        }
      >
        {(d) => (
          <>
            <h2 class="lk-feed-import-title">
              <Icon name={feedIconName(d().icon)} size={22} />
              <span>{d().name}</span>
            </h2>
            <ul class="lk-feed-import-rules">
              <For each={draftSummary(d())}>{(line) => <li>{line}</li>}</For>
            </ul>
            <div class="lk-feed-share-actions">
              <button type="button" class="primary" disabled={busy()} onClick={() => void save()}>
                Add to my feeds
              </button>
              <button type="button" class="ghost" onClick={() => navigate(paths.feed)}>
                Cancel
              </button>
            </div>
          </>
        )}
      </Show>
      <Show when={error()}>
        {(msg) => (
          <p class="lk-feed-problem" role="alert">
            {msg()}
          </p>
        )}
      </Show>
    </section>
  );
}

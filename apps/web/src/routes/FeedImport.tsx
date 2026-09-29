// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Import a shared feed (`docs/ux/feed.md` section 4.1): from a link, whose
 * fragment carries the rule (no server sees it), or from a `.lakkofeed`
 * file. The person sees a preview of the rules before anything is saved;
 * anything the core refuses is refused whole: "This file isn't a feed Lakko
 * can open."
 */

import { createSignal, For, onMount, Show, type JSX } from "solid-js";
import { useNavigate } from "@solidjs/router";

import { useAuth } from "../auth/AuthContext";
import { getDeviceKey } from "../auth/key-store";
import { bytesFromFragment, decodeDraft, draftSummary, encodeDraft, type FeedDraft } from "../feed/feed-draft";
import { loadFeedState, newFeedId, saveFeedState, upsertFeed } from "../feed/feed-store";
import { loadWasm, type Wasm } from "../wasm";
import { FEED_SELECTION_STORAGE_KEY } from "./Feed";
import { paths } from "./paths";

const REFUSED = "This file isn't a feed Lakko can open.";

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
      try {
        localStorage.setItem(FEED_SELECTION_STORAGE_KEY, `custom:${id}`);
      } catch {
        // The feed opens on its default; the saved feed is still in the row.
      }
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
            <h2>{d().name}</h2>
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

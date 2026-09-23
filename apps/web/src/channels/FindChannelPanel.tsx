// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Find a channel by its `@handle`.
 *
 * It used to live only on the Channels screen, which is why that destination
 * outlived its list: channels are in the conversation list now
 * (`docs/ux/navigation.md` section 2), and the only thing left holding the
 * tab up was this one field. A lookup is a function; it goes where a person
 * looks for something, not where a tab happens to be.
 *
 * That place is Contacts, beside "Add by username", because the two are the
 * same act: an exact, blinded lookup of a name someone gave you. Neither can
 * be a partial search - the server evaluates without learning what was asked
 * (invariant #5) - so both take a whole name and both say so.
 */

import { createSignal, Show, type JSX } from "solid-js";
import { useNavigate } from "@solidjs/router";

import { paths } from "../routes/paths";
import { resolveChannelIdOrHandle } from "./channel-discovery";

export default function FindChannelPanel(): JSX.Element {
  const navigate = useNavigate();
  const [query, setQuery] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  async function find(event: Event): Promise<void> {
    event.preventDefault();
    if (busy()) return;
    const raw = query().trim();
    if (raw.length === 0) return;
    setBusy(true);
    setError(null);
    try {
      const idHex = await resolveChannelIdOrHandle(raw);
      if (idHex === null) {
        setError(`No channel found for ${raw.startsWith("@") ? raw : `@${raw}`}.`);
        return;
      }
      const handle = raw.replace(/^@/, "");
      setQuery("");
      navigate(paths.channelSubscribe(idHex), {
        // A pasted 32-character id carries no handle; only a real one is kept.
        state: /^[0-9a-f]{32}$/i.test(handle) ? {} : { handle },
      });
    } catch {
      setError("Could not look that up. Check your connection and try again.");
    } finally {
      setBusy(false);
    }
  }

  return (
    <section class="lk-card">
      <h2>Find a channel</h2>
      <form onSubmit={(e) => void find(e)} class="lk-stack" style={{ gap: "12px" }}>
        <label>
          Handle
          <input
            type="text"
            placeholder="@lakko"
            autocomplete="off"
            spellcheck={false}
            required
            value={query()}
            onInput={(e) => setQuery(e.currentTarget.value)}
            disabled={busy()}
          />
        </label>
        <Show when={error()}>
          {(message) => (
            <div class="form-error" role="status">
              {message()}
            </div>
          )}
        </Show>
        <button type="submit" class="primary" disabled={busy()}>
          {busy() ? "Looking up..." : "Find"}
        </button>
      </form>
    </section>
  );
}

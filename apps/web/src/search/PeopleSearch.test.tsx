// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The people half of the search field (`docs/ux/who-can-find-me.md` section
 * 6.1): no "Search people" row - the field asks the network by itself after
 * the person stops typing - and Add contact sends the request at once, with
 * no form, then reads "Request sent".
 *
 * The search and the send are stood in for: their rules are covered in
 * `people-search.test.ts`, `send-request.test.ts` and `auto-search.test.ts`.
 * What is under test here is the wiring between them and the row.
 */
import { createSignal } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { all, present, renderUi } from "../test/ui";
import { t } from "../l10n";
import type { PersonResult } from "./people";
import type { PeopleSearchOutcome } from "./people-search";
import type { SendOutcome } from "./send-request";

const { searchPeople, sendFoundRequest, recordSentRequest } = vi.hoisted(() => ({
  searchPeople: vi.fn<(query: string) => Promise<PeopleSearchOutcome>>(),
  sendFoundRequest: vi.fn<(person: PersonResult) => Promise<SendOutcome>>(),
  recordSentRequest: vi.fn(),
}));

vi.mock("../auth/AuthContext", () => ({
  useAuth: () => ({ identitySecrets: () => null, state: () => ({ kind: "anonymous" }) }),
}));
vi.mock("../chat/ChatContext", () => ({
  useChats: () => ({ identity: () => null, accountIdentity: () => null }),
}));
vi.mock("../chat/persist-hooks", () => ({
  durableSnapshotHook: () => async () => {},
  sessionAccountOf: () => null,
}));
vi.mock("../profile/ProfileContext", () => ({
  useProfiles: () => ({ version: () => 0, ownProfileEnvelope: async () => null, learnUsername: () => {} }),
}));
vi.mock("../reach/reach-service", () => ({
  reachSession: () => ({ accountIdB64: "ACC", accountKeyHex: "00" }),
}));
vi.mock("../contacts/client", () => ({ ContactsClient: class {} }));
vi.mock("../contacts/sent-requests", () => ({ recordSentRequest: (...a: unknown[]) => recordSentRequest(...a) }));
vi.mock("../wasm", () => ({
  loadWasm: async () => ({
    nameQueryTerm: (q: string) => q.split(" ").find((w) => w.length >= 3),
  }),
}));
vi.mock("./wiring", async () => {
  const { fallbackNormalize } = await import("./people");
  return {
    localCandidates: () => [],
    normalizerOf: () => fallbackNormalize,
    canonicalUsernameOf: (_w: unknown, typed: string) => {
      const bare = typed.replace(/^@+/, "");
      return /^[a-z0-9_]{3,32}$/.test(bare) ? bare : null;
    },
    peopleSearchDeps: () => ({}),
    sendDeps: () => ({}),
  };
});
vi.mock("./people-search", () => ({ searchPeople: (q: string) => searchPeople(q) }));
vi.mock("./send-request", () => ({ sendFoundRequest: (p: PersonResult) => sendFoundRequest(p) }));

import { PeopleSearch } from "./PeopleSearch";

const anna = (keyHex: string): PersonResult => ({
  keyB64: "AAAA",
  keyHex,
  accountIdHex: "0102",
  name: "Anna Weber",
  username: "anna",
  reach: "open",
  grantWireB64: null,
});

/** Let promises (the wasm load, the search, the send) land. */
async function flush(): Promise<void> {
  for (let i = 0; i < 10; i++) await Promise.resolve();
}

function mount(): (text: string) => void {
  const [query, setQuery] = createSignal("");
  renderUi(() => (
    <PeopleSearch
      scope="chats"
      query={query}
      conversationMatches={() => 0}
      groupPeopleKeys={() => []}
      onOpenChannel={() => {}}
    />
  ));
  return setQuery;
}

const buttonsLabelled = (label: string): HTMLButtonElement[] =>
  all<HTMLButtonElement>("button").filter((b) => b.textContent?.trim() === label);

beforeEach(() => {
  vi.useFakeTimers();
  searchPeople.mockReset();
  sendFoundRequest.mockReset();
  recordSentRequest.mockReset();
});
afterEach(() => {
  vi.useRealTimers();
});

describe("PeopleSearch (UX 6.1)", () => {
  it("has no Search people row and asks the network by itself after 500 ms", async () => {
    searchPeople.mockResolvedValue({ people: [anna("a1")], channel: null, state: { kind: "found" } });
    const type = mount();
    await flush();
    type("ann");
    await flush();
    expect(buttonsLabelled(t("search.people.search"))).toHaveLength(0);
    vi.advanceTimersByTime(499);
    expect(searchPeople).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(searchPeople).toHaveBeenCalledWith("ann");
    await flush();
    expect(document.body.textContent).toContain("Anna Weber");
    expect(document.body.textContent).toContain(t("search.people.heading"));
  });

  it("shows nothing under People under three characters", async () => {
    const type = mount();
    await flush();
    type("an");
    await flush();
    vi.advanceTimersByTime(5_000);
    expect(searchPeople).not.toHaveBeenCalled();
    expect(document.body.textContent).not.toContain(t("search.people.heading"));
  });

  it("says out of searches when the tokens ran out", async () => {
    searchPeople.mockResolvedValue({ people: [], channel: null, state: { kind: "allowance-used" } });
    const type = mount();
    await flush();
    type("zed");
    await flush();
    vi.advanceTimersByTime(500);
    await flush();
    expect(document.body.textContent).toContain(t("search.people.out_of_searches"));
    expect(document.body.textContent).not.toContain(t("search.people.not_found"));
    expect(document.body.textContent).not.toContain(t("search.people.failed"));
  });

  it("Add contact sends at once, with no form, then the row reads Request sent", async () => {
    searchPeople.mockResolvedValue({ people: [anna("a2")], channel: null, state: { kind: "found" } });
    sendFoundRequest.mockResolvedValue({ kind: "sent" });
    const type = mount();
    await flush();
    type("anna2");
    await flush();
    vi.advanceTimersByTime(500);
    await flush();
    const [add] = buttonsLabelled(t("search.people.add_contact"));
    expect(add).toBeDefined();
    add!.click();
    // Nothing to fill in: no text field, no Send button.
    expect(present("textarea")).toBe(false);
    expect(buttonsLabelled(t("search.people.send_request"))).toHaveLength(0);
    expect(sendFoundRequest).toHaveBeenCalledTimes(1);
    await flush();
    expect(document.body.textContent).toContain(t("search.people.request_sent"));
    expect(buttonsLabelled(t("search.people.add_contact"))).toHaveLength(0);
    expect(recordSentRequest).toHaveBeenCalledTimes(1);
  });

  it("a failed request says so and gives the button back", async () => {
    searchPeople.mockResolvedValue({ people: [anna("a3")], channel: null, state: { kind: "found" } });
    sendFoundRequest.mockResolvedValue({ kind: "failed", message: () => "x" });
    const type = mount();
    await flush();
    type("anna3");
    await flush();
    vi.advanceTimersByTime(500);
    await flush();
    buttonsLabelled(t("search.people.add_contact"))[0]!.click();
    await flush();
    expect(document.body.textContent).toContain(t("search.people.request_failed"));
    expect(buttonsLabelled(t("search.people.add_contact"))).toHaveLength(1);
  });

  it("a refused request says they may have changed who can find them, with no button", async () => {
    searchPeople.mockResolvedValue({ people: [anna("a4")], channel: null, state: { kind: "found" } });
    sendFoundRequest.mockResolvedValue({ kind: "refused" });
    const type = mount();
    await flush();
    type("anna4");
    await flush();
    vi.advanceTimersByTime(500);
    await flush();
    buttonsLabelled(t("search.people.add_contact"))[0]!.click();
    await flush();
    expect(document.body.textContent).toContain(t("search.people.request_refused"));
    expect(buttonsLabelled(t("search.people.add_contact"))).toHaveLength(0);
  });
});

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
import type { LocalCandidate, PersonResult } from "./people";
import type { PeopleSearchOutcome } from "./people-search";
import type { SendOutcome } from "./send-request";

const { searchPeople, sendFoundRequest, recordSentRequest, reachCheck, held, sentList, contactList } = vi.hoisted(() => ({
  searchPeople: vi.fn<(query: string) => Promise<PeopleSearchOutcome>>(),
  sendFoundRequest: vi.fn<(person: PersonResult) => Promise<SendOutcome>>(),
  recordSentRequest: vi.fn(),
  reachCheck: vi.fn<(key: Uint8Array, wire: string) => Promise<boolean>>(),
  held: { candidates: [] as LocalCandidate[] },
  sentList: { records: [] as Record<string, unknown>[], changed: (): void => {}, version: (): number => 0 },
  contactList: {
    rows: [] as { signaturePubkeyB64: string; accountIdHex?: string }[],
    listeners: new Set<() => void>(),
  },
}));

vi.mock("@solidjs/router", () => ({
  A: (p: { href: string; children: unknown }) => {
    const link = document.createElement("a");
    link.setAttribute("href", p.href);
    link.textContent = String(p.children);
    return link;
  },
}));
vi.mock("../auth/AuthContext", () => ({
  useAuth: () => ({ identitySecrets: () => null, state: () => ({ kind: "anonymous" }) }),
}));
vi.mock("../chat/ChatContext", () => ({
  useChats: () => ({ identity: () => null, accountIdentity: () => null, get: () => undefined }),
}));
vi.mock("../chat/persist-hooks", () => ({
  durableSnapshotHook: () => async () => {},
  sessionAccountOf: () => null,
}));
vi.mock("../profile/ProfileContext", () => ({
  useProfiles: () => ({
    version: () => 0,
    ownProfileEnvelope: async () => null,
    learnUsername: () => {},
    resolve: () => ({ username: "@boblind" }),
  }),
}));
vi.mock("../reach/reach-service", () => ({
  reachSession: () => ({ accountIdB64: "ACC", accountKeyHex: "00" }),
}));
vi.mock("../contacts/client", () => ({ ContactsClient: class {} }));
vi.mock("../contacts/local-store", () => ({
  loadAccepted: () => contactList.rows,
  subscribeAcceptedChanged: (listener: () => void) => {
    contactList.listeners.add(listener);
    return () => contactList.listeners.delete(listener);
  },
}));
vi.mock("../contacts/sent-requests", () => ({
  recordSentRequest: (...a: unknown[]) => recordSentRequest(...a),
  loadSentRequests: () => sentList.records,
  sentRequestsVersion: () => sentList.version(),
}));
vi.mock("../wasm", () => ({
  loadWasm: async () => ({
    nameQueryTerm: (q: string) => q.split(" ").find((w) => w.length >= 3),
  }),
}));
vi.mock("./wiring", async () => {
  const { fallbackNormalize } = await import("./people");
  return {
    localCandidates: () => held.candidates,
    reachCheckWith: (key: Uint8Array, wire: string) => reachCheck(key, wire),
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

function mount(
  over: {
    chatPeers?: () => ReadonlySet<string>;
    chatHref?: (key: string) => string | null;
    listed?: () => ReadonlySet<string>;
    noMatchLine?: () => string | null;
    scope?: "chats" | "contacts";
    standing?: () => ReadonlySet<string>;
  } = {},
) {
  const [query, setQuery] = createSignal("");
  renderUi(() => (
    <PeopleSearch
      scope={over.scope ?? "chats"}
      query={query}
      conversationMatches={() => 0}
      groupPeopleKeys={() => []}
      onOpenChannel={() => {}}
      {...(over.listed === undefined ? {} : { listedB64: over.listed })}
      {...(over.noMatchLine === undefined ? {} : { noMatchLine: over.noMatchLine })}
      {...(over.chatPeers === undefined ? {} : { standingAboveB64: over.chatPeers })}
      {...(over.chatHref === undefined ? {} : { chatHrefOf: over.chatHref })}
      {...(over.standing === undefined ? {} : { standingConversationIds: over.standing })}
    />
  ));
  return setQuery;
}

/** The address book took a row: what `upsertAccepted` does, listeners at once. */
function aContactCameIn(row: { signaturePubkeyB64: string; accountIdHex?: string }): void {
  contactList.rows = [...contactList.rows, row];
  for (const listener of contactList.listeners) listener();
}

const buttonsLabelled = (label: string): HTMLButtonElement[] =>
  all<HTMLButtonElement>("button").filter((b) => b.textContent?.trim() === label);

/** What the store does on a real send: the list holds the person. */
function theListChanged(records: Record<string, unknown>[]): void {
  sentList.records = records;
  sentList.changed();
}

beforeEach(() => {
  vi.useFakeTimers();
  searchPeople.mockReset();
  sendFoundRequest.mockReset();
  recordSentRequest.mockReset();
  recordSentRequest.mockImplementation((_account: string, record: Record<string, unknown>) =>
    theListChanged([...sentList.records, record]),
  );
  reachCheck.mockReset();
  held.candidates = [];
  contactList.rows = [];
  contactList.listeners.clear();
  sentList.records = [];
  const [version, setVersion] = createSignal(0);
  sentList.version = version;
  sentList.changed = () => setVersion((v) => v + 1);
});
afterEach(() => {
  vi.useRealTimers();
});

describe("one person, one row (UX 6.1)", () => {
  const found = { people: [anna("a1")], channel: null, state: { kind: "found" } } as PeopleSearchOutcome;

  it("a person a list row shows has no row under People, Request sent or not", async () => {
    searchPeople.mockResolvedValue(found);
    sentList.records = [{ recipientAccountIdHex: "0102", recipientKeyB64: "AAAA" }];
    const type = mount({ listed: () => new Set(["AAAA"]) });
    await flush();
    type("onea");
    await flush();
    vi.advanceTimersByTime(500);
    await flush();
    expect(document.body.textContent).not.toContain("Anna Weber");
    expect(document.body.textContent).not.toContain(t("search.people.heading"));
  });

  it("a person who is not listed keeps the row", async () => {
    searchPeople.mockResolvedValue(found);
    const type = mount({ listed: () => new Set(["BBBB"]) });
    await flush();
    type("oneb");
    await flush();
    vi.advanceTimersByTime(500);
    await flush();
    expect(document.body.textContent).toContain("Anna Weber");
  });
});

describe("one conversation, one row (UX 6.1 One list)", () => {
  const group = {
    kind: "group",
    idB64: "R1JPVVBJRA==",
    handle: "qa_ios_walk",
    name: "QA walk group",
    description: "",
    sizeBand: 0,
    key: new Uint8Array(32),
    knockVersion: 0,
    avatar: null,
  };
  const found = { people: [], channel: null, conversations: [group], state: { kind: "found" } } as unknown as PeopleSearchOutcome;

  it("a group that stands above as a conversation row is not listed again", async () => {
    searchPeople.mockResolvedValue(found);
    const type = mount({ standing: () => new Set([group.idB64]) });
    await flush();
    type("@qa_ios_walk");
    await flush();
    vi.advanceTimersByTime(500);
    await flush();
    expect(document.body.textContent).not.toContain("QA walk group");
  });

  it("a group not above keeps its directory row", async () => {
    searchPeople.mockResolvedValue(found);
    const type = mount({ standing: () => new Set(["T1RIRVI="]) });
    await flush();
    type("@qa_ios_walk");
    await flush();
    vi.advanceTimersByTime(500);
    await flush();
    expect(document.body.textContent).toContain("QA walk group");
  });
});

describe("the line that nothing matches (UX 6.1)", () => {
  const line = (): string => t("contacts.list.no_match");
  const contacts = {
    scope: "contacts" as const,
    listed: () => new Set<string>(),
    noMatchLine: line,
  };

  it("stands alone under three characters, when nothing is asked", async () => {
    const type = mount(contacts);
    await flush();
    type("zz");
    await flush();
    expect(document.body.textContent).toContain(line());
  });

  it("waits while a search is coming and while it is out", async () => {
    let answer: (o: PeopleSearchOutcome) => void = () => {};
    searchPeople.mockReturnValue(new Promise((resolve) => (answer = resolve)));
    const type = mount(contacts);
    await flush();
    type("zzz");
    await flush();
    expect(document.body.textContent).not.toContain(line());
    vi.advanceTimersByTime(500);
    await flush();
    expect(document.body.textContent).not.toContain(line());
    answer({ people: [], channel: null, state: { kind: "not-found" } });
    await flush();
    expect(document.body.textContent).toContain(line());
  });

  it("never stands above a person under People", async () => {
    searchPeople.mockResolvedValue({ people: [anna("a1")], channel: null, state: { kind: "found" } });
    const type = mount(contacts);
    await flush();
    type("ann");
    await flush();
    vi.advanceTimersByTime(500);
    await flush();
    expect(document.body.textContent).toContain("Anna Weber");
    expect(document.body.textContent).not.toContain(line());
  });

  it("never stands below a list row", async () => {
    const type = mount({ ...contacts, listed: () => new Set(["CCCC"]) });
    await flush();
    type("zz");
    await flush();
    expect(document.body.textContent).not.toContain(line());
  });

  it("is not said to an account with no list row at all", async () => {
    const type = mount({ ...contacts, noMatchLine: () => null });
    await flush();
    type("zz");
    await flush();
    expect(document.querySelector(".lk-pane-nomatch")).toBeNull();
  });

  it("is the one list's own line in Chats (UX 6.1 One list)", async () => {
    const type = mount();
    await flush();
    type("zz");
    await flush();
    expect(document.body.textContent).toContain(t("search.nothing_found"));
    expect(document.body.textContent).not.toContain(line());
  });
});

describe("PeopleSearch (UX 6.1)", () => {
  it("has no Search people row and asks the network by itself after 500 ms", async () => {
    searchPeople.mockResolvedValue({ people: [anna("a1")], channel: null, state: { kind: "found" } });
    const type = mount();
    await flush();
    type("ann");
    await flush();
    expect(buttonsLabelled("Search people")).toHaveLength(0);
    vi.advanceTimersByTime(499);
    expect(searchPeople).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(searchPeople).toHaveBeenCalledWith("ann");
    await flush();
    expect(document.body.textContent).toContain("Anna Weber");
    // One list (UX 6.1): the person stands with no People heading above.
    expect(document.body.textContent).not.toContain(t("search.people.heading"));
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

  describe("a kept grant is checked before it is offered", () => {
    const kept = (keyHex: string, over: Partial<LocalCandidate> = {}): LocalCandidate => ({
      keyB64: "AAAA",
      keyHex,
      name: "Bob Lind",
      verifiedUsername: "boblind",
      isContact: false,
      accountIdHex: "0102",
      keptGrantWires: ["STALE", "LIVE"],
      ...over,
    });
    const addButtons = () => buttonsLabelled(t("search.people.add_contact"));

    it("shows the name alone until a grant passes, then offers Add contact with it", async () => {
      held.candidates = [kept("b1")];
      let answerLive: (ok: boolean) => void = () => {};
      reachCheck.mockImplementation(async (_key, wire) => {
        if (wire === "STALE") return false;
        return new Promise((resolve) => {
          answerLive = resolve;
        });
      });
      sendFoundRequest.mockResolvedValue({ kind: "sent" });
      const type = mount();
      await flush();
      type("bo");
      await flush();
      expect(document.body.textContent).toContain("Bob Lind");
      expect(document.body.textContent).not.toContain("@boblind");
      expect(addButtons()).toHaveLength(0);

      answerLive(true);
      await flush();
      expect(document.body.textContent).toContain("@boblind");
      addButtons()[0]!.click();
      await flush();
      expect(sendFoundRequest.mock.calls[0]?.[0]).toMatchObject({ grantWireB64: "LIVE", grantChecked: true });
    });

    it("runs one check per person at a time, and asks again on the next text", async () => {
      held.candidates = [kept("b2", { keptGrantWires: ["ONE"] })];
      reachCheck.mockResolvedValue(true);
      const type = mount();
      await flush();
      type("b");
      type("bo");
      await flush();
      expect(reachCheck).toHaveBeenCalledTimes(1);
      type("bob");
      await flush();
      expect(reachCheck).toHaveBeenCalledTimes(2);
    });

    it("a person who left between two texts stops being offered, in one session", async () => {
      held.candidates = [kept("c1", { keptGrantWires: ["ONE"] })];
      reachCheck.mockResolvedValue(true);
      const type = mount();
      await flush();
      type("bo");
      await flush();
      expect(addButtons()).toHaveLength(1);
      reachCheck.mockResolvedValue(false);
      type("bob");
      await flush();
      expect(addButtons()).toHaveLength(0);
      expect(document.body.textContent).toContain("Bob Lind");
      expect(document.body.textContent).not.toContain("@boblind");
    });

    it("a connection failure leaves a held verdict standing", async () => {
      held.candidates = [kept("c2", { keptGrantWires: ["ONE"] })];
      reachCheck.mockResolvedValue(false);
      const type = mount();
      await flush();
      type("bo");
      await flush();
      expect(addButtons()).toHaveLength(0);
      reachCheck.mockRejectedValue(new Error("offline"));
      type("bob");
      await flush();
      expect(addButtons()).toHaveLength(0);
    });

    it("leaves a name, no button and no match by username when every grant is refused", async () => {
      held.candidates = [kept("b3")];
      reachCheck.mockResolvedValue(false);
      const type = mount();
      await flush();
      type("bob");
      await flush();
      expect(reachCheck).toHaveBeenCalledTimes(2);
      expect(document.body.textContent).toContain("Bob Lind");
      expect(document.body.textContent).not.toContain("@boblind");
      expect(addButtons()).toHaveLength(0);

      type("boblind");
      await flush();
      expect(document.body.textContent).not.toContain("Bob Lind");
    });

    it("a check that throws changes nothing: the row is as it was, and the next text asks again", async () => {
      held.candidates = [kept("b4")];
      reachCheck.mockRejectedValue(new Error("offline"));
      const type = mount();
      await flush();
      type("bo");
      await flush();
      expect(document.body.textContent).toContain("@boblind");
      expect(addButtons()).toHaveLength(1);
      expect(reachCheck).toHaveBeenCalledTimes(1);

      type("bob");
      await flush();
      expect(reachCheck).toHaveBeenCalledTimes(2);
    });

    it("never checks a contact, who shows at once", async () => {
      held.candidates = [kept("b5", { isContact: true })];
      const type = mount();
      await flush();
      type("bo");
      await flush();
      expect(reachCheck).not.toHaveBeenCalled();
      expect(document.body.textContent).toContain("@boblind");
      expect(document.body.textContent).toContain(t("common.open"));
    });

    it("a send the reach check refused keeps the sentence and the username, and loses the button", async () => {
      held.candidates = [kept("b6", { keptGrantWires: ["ONE"] })];
      reachCheck.mockResolvedValue(true);
      sendFoundRequest.mockResolvedValue({ kind: "refused", byReachCheck: true });
      const type = mount();
      await flush();
      type("bo");
      await flush();
      addButtons()[0]!.click();
      await flush();
      expect(document.body.textContent).toContain(t("search.people.request_refused"));
      expect(addButtons()).toHaveLength(0);
      expect(document.body.textContent).toContain("@boblind");
    });

    it("the refused line lasts until the text changes, and the row then says what the check says", async () => {
      held.candidates = [kept("b8", { keptGrantWires: ["ONE"] })];
      reachCheck.mockResolvedValue(true);
      sendFoundRequest.mockResolvedValue({ kind: "refused", byReachCheck: true });
      const type = mount();
      await flush();
      type("bo");
      await flush();
      addButtons()[0]!.click();
      await flush();
      expect(document.body.textContent).toContain(t("search.people.request_refused"));

      // They widened who can find them.
      type("bob");
      await flush();
      expect(document.body.textContent).not.toContain(t("search.people.request_refused"));
      expect(addButtons()).toHaveLength(1);
    });

    it("the refused line goes with the check: a person still out of reach is a name alone", async () => {
      held.candidates = [kept("b9", { keptGrantWires: ["ONE"] })];
      reachCheck.mockResolvedValue(true);
      sendFoundRequest.mockResolvedValue({ kind: "refused", byReachCheck: true });
      const type = mount();
      await flush();
      type("bo");
      await flush();
      addButtons()[0]!.click();
      await flush();
      reachCheck.mockResolvedValue(false);
      type("bob");
      await flush();
      expect(document.body.textContent).not.toContain(t("search.people.request_refused"));
      expect(document.body.textContent).not.toContain("@boblind");
      expect(addButtons()).toHaveLength(0);
    });

    it("a person found by name alone goes in the Sent list too, or the row would offer Add contact again", async () => {
      held.candidates = [kept("d1", { keptGrantWires: ["ONE"], verifiedUsername: null })];
      reachCheck.mockResolvedValue(true);
      sendFoundRequest.mockResolvedValue({ kind: "sent" });
      const type = mount();
      await flush();
      type("bo");
      await flush();
      addButtons()[0]!.click();
      await flush();
      expect(recordSentRequest.mock.calls[0]?.[1]).toMatchObject({
        recipientUsername: "",
        recipientName: "Bob Lind",
      });
      expect(document.body.textContent).toContain(t("search.people.request_sent"));
      expect(addButtons()).toHaveLength(0);
    });

    it("Request sent comes from the list: declined and dismissed, the row offers Add contact again", async () => {
      held.candidates = [kept("d2", { keptGrantWires: ["ONE"] })];
      reachCheck.mockResolvedValue(true);
      sendFoundRequest.mockResolvedValue({ kind: "sent" });
      const type = mount();
      await flush();
      type("bo");
      await flush();
      addButtons()[0]!.click();
      await flush();
      expect(document.body.textContent).toContain(t("search.people.request_sent"));

      theListChanged([]);
      await flush();
      expect(document.body.textContent).not.toContain(t("search.people.request_sent"));
      expect(addButtons()).toHaveLength(1);
    });

    it("a request sent earlier reads Request sent on a fresh field", async () => {
      held.candidates = [kept("d3", { keptGrantWires: ["ONE"], accountIdHex: "0102" })];
      theListChanged([{ recipientAccountIdHex: "0102", recipientUsername: "", sentAtMs: 1 }]);
      reachCheck.mockResolvedValue(true);
      const type = mount();
      await flush();
      type("bo");
      await flush();
      expect(document.body.textContent).toContain(t("search.people.request_sent"));
      expect(addButtons()).toHaveLength(0);
    });

    it("acceptance with the text in the field: never Add contact between Sent and the contact", async () => {
      searchPeople.mockResolvedValue({ people: [anna("e1")], channel: null, state: { kind: "found" } });
      sendFoundRequest.mockResolvedValue({ kind: "sent" });
      const type = mount();
      await flush();
      type("anna9");
      await flush();
      vi.advanceTimersByTime(500);
      await flush();
      buttonsLabelled(t("search.people.add_contact"))[0]!.click();
      await flush();
      expect(document.body.textContent).toContain(t("search.people.request_sent"));

      // The ack: the contact first, then the Sent record goes.
      aContactCameIn({ signaturePubkeyB64: "AAAA", accountIdHex: "0102" });
      await flush();
      expect(buttonsLabelled(t("search.people.add_contact"))).toHaveLength(0);
      theListChanged([]);
      await flush();
      expect(buttonsLabelled(t("search.people.add_contact"))).toHaveLength(0);
      expect(document.body.textContent).toContain(t("common.open"));
    });

    it("a contact whose conversation matched leaves People, heading and line included", async () => {
      searchPeople.mockResolvedValue({ people: [], channel: null, state: { kind: "not-found" } });
      held.candidates = [kept("e2", { isContact: true })];
      const type = mount({ chatPeers: () => new Set(["AAAA"]) });
      await flush();
      type("bob");
      await flush();
      vi.advanceTimersByTime(500);
      await flush();
      expect(document.body.textContent).not.toContain("Bob Lind");
      expect(document.body.textContent).not.toContain(t("search.people.heading"));
      expect(document.body.textContent).not.toContain(t("search.people.not_found"));
    });

    it("Open under People opens the conversation", async () => {
      held.candidates = [kept("e3", { isContact: true })];
      const type = mount({ chatHref: (key) => (key === "AAAA" ? "/chat/abc" : null) });
      await flush();
      type("bob");
      await flush();
      expect(all<HTMLAnchorElement>("a").map((a) => a.getAttribute("href"))).toEqual(["/chat/abc"]);
    });

    it("Open with no conversation yet opens the contact, whose Message creates one", async () => {
      held.candidates = [kept("e4", { isContact: true })];
      const type = mount({ chatHref: () => null });
      await flush();
      type("bob");
      await flush();
      expect(all<HTMLAnchorElement>("a")[0]?.getAttribute("href")).toContain("/contacts");
    });

    it("no People heading stands alone: below 3 characters with no row it is gone", async () => {
      held.candidates = [];
      const type = mount();
      await flush();
      type("zz");
      await flush();
      vi.advanceTimersByTime(1000);
      await flush();
      expect(document.body.textContent).not.toContain(t("search.people.heading"));
      expect(document.body.textContent).not.toContain(t("search.people.not_found"));
    });

    it("says Nothing found only when the list is empty", async () => {
      held.candidates = [kept("b7")];
      reachCheck.mockResolvedValue(false);
      searchPeople.mockResolvedValue({ people: [], channel: null, state: { kind: "not-found" } });
      const type = mount();
      await flush();
      type("bob");
      await flush();
      vi.advanceTimersByTime(500);
      await flush();
      expect(document.body.textContent).toContain("Bob Lind");
      expect(document.body.textContent).not.toContain(t("search.people.not_found"));

      type("zed");
      await flush();
      vi.advanceTimersByTime(500);
      await flush();
      expect(document.body.textContent).not.toContain("Bob Lind");
      expect(document.body.textContent).toContain(t("search.nothing_found"));
      expect(document.body.textContent).not.toContain(t("search.people.not_found"));
    });
  });
});

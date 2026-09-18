// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * An incoming 1x1 call must reach the call host wherever the user is in the
 * app — including, and especially, when the caller's chat is not the one on
 * screen.
 *
 * ## The shape that was broken
 *
 * Observed 2026-09-18, iOS to web on test.lakko.me: the web account was signed
 * in, the window was in front of the user, and the app was showing a GROUP
 * chat. iOS placed a 1x1 voice call. Nothing rang, no notification appeared
 * (that tab has notifications denied), no `End{busy}` was published, and the
 * caller rang for its full 45 s and recorded a missed call.
 *
 * Every decision module on the path was right. What was wrong was upstream of
 * all of them: nothing fetched the offer row. Both receive loops park while
 * `document.visibilityState === "hidden"`, and the browser pane used for the
 * check reports `hidden` even when fronted; the only thing that un-parks the
 * background sweep is a service-worker push wake, which a tab with
 * notifications denied can never receive. So the tab sat there, awake to the
 * user and asleep to the DS.
 *
 * ## What this file drives
 *
 * The chain from a decrypted journal row to a ringing call, assembled from the
 * production modules rather than described:
 *
 *   - {@link runSweep} / {@link runBackgroundReceiveLoop} — who gets polled,
 *     and when the loop is allowed to stop polling;
 *   - {@link ReceiveCoordinator} — the foreground claim that keeps the open
 *     chat's group out of the sweep;
 *   - {@link planBackgroundEvents} — the split that sends call signaling to
 *     the call host rather than the timeline;
 *   - {@link SerialOnceQueue} — exactly once, in order;
 *   - {@link decideInboundSignaling} — ring, busy or drop;
 *   - {@link backgroundReceiveIsActive} — the park rule.
 *
 * The only stubs are the DS fetch, the MLS decrypt and the wasm body parse:
 * everything those produce is data, and none of them can be reached from
 * vitest, which runs in plain node.
 */
import { describe, expect, it } from "vitest";

import { createWakeLatch } from "../ds/long-poll-loop";
import { planBackgroundEvents } from "../chat/background-events";
import {
  runBackgroundReceiveLoop,
  runSweep,
  type BackgroundLoopDeps,
  type SweepDeps,
} from "../chat/background-receive-scheduler";
import type { IncomingChatEvent } from "../chat/client";
import { backgroundReceiveIsActive } from "../chat/receive-activity";
import { ReceiveCoordinator } from "../chat/receive-coordinator";
import type { PushWakeReach } from "../push/wake-reach";
import { decideInboundSignaling, type OneToOneAction } from "./call-host-routing";
import { SerialOnceQueue, type InboundSignalingRow } from "./call-signaling-dispatch";

const ONE_TO_ONE = "grp-1x1";
const GROUP_CHAT = "grp-team";

/** The body a `payloadBytes` carries in this file. The real one is a prost
 *  `CallSignalingPayload` decoded by wasm; what the routing rules consume is
 *  this much of it. */
interface FakeBody {
  readonly kind: string;
  readonly callIdHex: string;
}

function offerRow(over: Partial<{ messageId: number; createdAt: string; callIdHex: string }> = {}) {
  const body: FakeBody = { kind: "offer", callIdHex: over.callIdHex ?? "c0ffee" };
  const event: Extract<IncomingChatEvent, { kind: "callSignaling" }> = {
    kind: "callSignaling",
    messageId: over.messageId ?? 41,
    payloadBytes: new TextEncoder().encode(JSON.stringify(body)),
    senderLeaf: 1,
    createdAt: over.createdAt ?? new Date().toISOString(),
  };
  return event;
}

function chatRow(messageId: number): IncomingChatEvent {
  return {
    kind: "application",
    messageId,
    text: "hello",
    attachments: [],
    createdAt: new Date().toISOString(),
  } as unknown as IncomingChatEvent;
}

/** The call host, reduced to what decides whether a call rings. */
class CallHostStub {
  readonly outcomes: { groupIdB64: string; action: OneToOneAction }[] = [];
  private active: { callIdHex: string; groupIdB64: string } | null = null;
  private readonly endedCallIds = new Set<string>();
  private readonly queue: SerialOnceQueue<{ groupIdB64: string; row: InboundSignalingRow }>;

  constructor(private readonly nowMs: () => number = Date.now) {
    this.queue = new SerialOnceQueue(
      async (item) => {
        const body = JSON.parse(new TextDecoder().decode(item.row.payloadBytes)) as FakeBody;
        const decision = decideInboundSignaling({
          route: "oneToOne",
          kind: body.kind,
          callIdHex: body.callIdHex,
          groupIdB64: item.groupIdB64,
          rowCreatedAt: item.row.createdAt,
          nowMs: this.nowMs(),
          active: this.active,
          endedCallIds: this.endedCallIds,
          groupCallBusy: false,
          groupRecordKnowsCall: false,
        });
        this.outcomes.push({ groupIdB64: item.groupIdB64, action: decision.oneToOne });
        if (decision.oneToOne === "ring") {
          this.active = { callIdHex: body.callIdHex, groupIdB64: item.groupIdB64 };
        }
        if (decision.oneToOne === "busy") this.endedCallIds.add(body.callIdHex);
      },
      (item) => `${item.groupIdB64}:${item.row.messageId}`,
    );
  }

  ingestSignaling(groupIdB64: string, row: InboundSignalingRow): void {
    this.queue.enqueue({ groupIdB64, row });
  }

  settled(): Promise<void> {
    return this.queue.whenIdle();
  }

  /** `null` until something rang. */
  ringing(): { callIdHex: string; groupIdB64: string } | null {
    return this.active;
  }
}

/** The provider's `settle`, reduced to the call-signaling half. */
function settle(
  host: CallHostStub,
  groupIdB64: string,
  events: readonly IncomingChatEvent[],
): void {
  const plan = planBackgroundEvents(events);
  for (const row of plan.signaling) host.ingestSignaling(groupIdB64, row);
}

interface Journals {
  readonly [groupIdB64: string]: IncomingChatEvent[];
}

/** One background poll: hand over whatever the journal holds, once. */
function pollFrom(host: CallHostStub, journals: Journals, polled: string[]) {
  return async (groupIdB64: string): Promise<{ yieldedRows: boolean }> => {
    polled.push(groupIdB64);
    const pending = journals[groupIdB64] ?? [];
    if (pending.length === 0) return { yieldedRows: false };
    const batch = pending.splice(0, pending.length);
    settle(host, groupIdB64, batch);
    return { yieldedRows: true };
  };
}

function sweepDeps(over: Partial<SweepDeps> & Pick<SweepDeps, "poll" | "groups">): SweepDeps {
  return {
    isExcluded: () => false,
    concurrency: 2,
    maxImmediateRepolls: 4,
    ...over,
  };
}

describe("an incoming call for a chat whose route is not mounted", () => {
  it("reaches the call host and rings", async () => {
    // The reported shape: the user is reading the team chat, the call arrives
    // on the 1x1 journal, and the 1x1 route is nowhere.
    const host = new CallHostStub();
    const coordinator = new ReceiveCoordinator();
    coordinator.claimForeground(GROUP_CHAT);
    const journals: Journals = { [ONE_TO_ONE]: [offerRow()], [GROUP_CHAT]: [] };
    const polled: string[] = [];

    await runSweep(
      sweepDeps({
        groups: () => [GROUP_CHAT, ONE_TO_ONE],
        isExcluded: (id) => coordinator.isForeground(id),
        poll: pollFrom(host, journals, polled),
      }),
      new AbortController().signal,
    );
    await host.settled();

    // The 1x1 group is polled (twice — the scheduler re-polls a group that
    // yielded rows, to drain a burst) and the open chat never is: its own view
    // owns that journal.
    expect([...new Set(polled)]).toEqual([ONE_TO_ONE]);
    expect(host.ringing()).toEqual({ callIdHex: "c0ffee", groupIdB64: ONE_TO_ONE });
    expect(host.outcomes).toEqual([{ groupIdB64: ONE_TO_ONE, action: "ring" }]);
  });

  it("rings from a batch that also carries ordinary messages", async () => {
    // Call signaling and chat rows share one journal and one batch; the split
    // must not let a message row swallow the offer.
    const host = new CallHostStub();
    const journals: Journals = {
      [ONE_TO_ONE]: [chatRow(40), offerRow({ messageId: 41 }), chatRow(42)],
    };
    const polled: string[] = [];

    await runSweep(
      sweepDeps({ groups: () => [ONE_TO_ONE], poll: pollFrom(host, journals, polled) }),
      new AbortController().signal,
    );
    await host.settled();

    expect(host.ringing()?.groupIdB64).toBe(ONE_TO_ONE);
  });

  it("rings once even if the same row is handed over twice", async () => {
    // A held persist re-fetches its batch; the second hand-over must not open
    // a second answerer for the same call.
    const host = new CallHostStub();
    const row = offerRow();
    settle(host, ONE_TO_ONE, [row]);
    settle(host, ONE_TO_ONE, [row]);
    await host.settled();

    expect(host.outcomes).toHaveLength(1);
    expect(host.outcomes[0]?.action).toBe("ring");
  });
});

/** The loop, with the park rule wired exactly as the provider wires it. */
function loopDeps(
  over: Pick<SweepDeps, "poll" | "groups"> & {
    readonly documentHidden: boolean;
    readonly pushWake: PushWakeReach;
    readonly resume: ReturnType<typeof createWakeLatch>;
    readonly onSweptOnce: () => void;
  },
): BackgroundLoopDeps {
  return {
    ...sweepDeps(over),
    isActive: () =>
      backgroundReceiveIsActive({
        signedIn: true,
        documentHidden: over.documentHidden,
        callInProgress: false,
        wakeWindowOpen: false,
        pushWake: over.pushWake,
      }),
    waitUntilActive: (signal) => over.resume.wait(signal),
    sleep: async () => {
      over.onSweptOnce();
    },
    random: () => 0.5,
    intervalMs: 0,
    jitterMs: 0,
  };
}

describe("the receive loop and an incoming call in a hidden tab", () => {
  it("still rings when no push wake can reach the tab", async () => {
    // The regression. Notifications denied (or no VAPID key, or no Push API)
    // means the service worker can show nothing, so the in-app screen of
    // `docs/ux/calls.md` §2.2 is the only ring there is — and it needs the
    // sweep to have fetched the offer.
    const host = new CallHostStub();
    const journals: Journals = { [ONE_TO_ONE]: [offerRow()] };
    const abort = new AbortController();
    const polled: string[] = [];

    await runBackgroundReceiveLoop(
      abort.signal,
      loopDeps({
        groups: () => [ONE_TO_ONE],
        poll: pollFrom(host, journals, polled),
        documentHidden: true,
        pushWake: "unreachable",
        resume: createWakeLatch(),
        onSweptOnce: () => abort.abort(),
      }),
    );
    await host.settled();

    expect([...new Set(polled)]).toEqual([ONE_TO_ONE]);
    expect(host.ringing()?.groupIdB64).toBe(ONE_TO_ONE);
  });

  it("still rings while the push path is not established yet", async () => {
    const host = new CallHostStub();
    const journals: Journals = { [ONE_TO_ONE]: [offerRow()] };
    const abort = new AbortController();

    await runBackgroundReceiveLoop(
      abort.signal,
      loopDeps({
        groups: () => [ONE_TO_ONE],
        poll: pollFrom(host, journals, []),
        documentHidden: true,
        pushWake: "unknown",
        resume: createWakeLatch(),
        onSweptOnce: () => abort.abort(),
      }),
    );
    await host.settled();

    expect(host.ringing()?.groupIdB64).toBe(ONE_TO_ONE);
  });

  it("parks, as before, when a push wake can reach the tab", async () => {
    // Unchanged behaviour, kept honest: the wake is what brings such a tab
    // back, and the battery cost of polling it is not paid twice.
    const host = new CallHostStub();
    const journals: Journals = { [ONE_TO_ONE]: [offerRow()] };
    const abort = new AbortController();
    const polled: string[] = [];

    const loop = runBackgroundReceiveLoop(
      abort.signal,
      loopDeps({
        groups: () => [ONE_TO_ONE],
        poll: pollFrom(host, journals, polled),
        documentHidden: true,
        pushWake: "reachable",
        resume: createWakeLatch(),
        onSweptOnce: () => abort.abort(),
      }),
    );
    await Promise.resolve();
    await Promise.resolve();
    abort.abort();
    await loop;
    await host.settled();

    expect(polled).toEqual([]);
    expect(host.ringing()).toBeNull();
  });
});

describe("what still refuses to ring", () => {
  it("drops an offer older than the stale window instead of ringing", async () => {
    // A tab that comes back after a long sleep must not open onto a call that
    // ended long ago.
    const host = new CallHostStub(() => Date.parse("2026-09-18T10:05:00.000Z"));
    settle(host, ONE_TO_ONE, [offerRow({ createdAt: "2026-09-18T10:00:00.000Z" })]);
    await host.settled();

    expect(host.outcomes).toEqual([{ groupIdB64: ONE_TO_ONE, action: "drop" }]);
    expect(host.ringing()).toBeNull();
  });

  it("answers a second caller busy rather than ringing twice", async () => {
    const host = new CallHostStub();
    settle(host, ONE_TO_ONE, [offerRow({ messageId: 1, callIdHex: "aaaa" })]);
    settle(host, GROUP_CHAT, [offerRow({ messageId: 1, callIdHex: "bbbb" })]);
    await host.settled();

    expect(host.outcomes.map((o) => o.action)).toEqual(["ring", "busy"]);
    expect(host.ringing()?.callIdHex).toBe("aaaa");
  });
});

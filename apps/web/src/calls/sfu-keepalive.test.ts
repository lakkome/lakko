// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Signaling keepalive on the SFU call session (`sfu-keepalive.ts`).
 *
 * Two layers, headless (node env, no WebRTC / DOM):
 *   * the pure rule `sfuKeepaliveAction` — first tick past the interval pings,
 *     an earlier tick waits, past the deadline is dead even when a ping is also
 *     due, an inbound frame resets the deadline, and the three constants are
 *     pinned (they are shared word-for-word with iOS and Android);
 *   * the wiring in {@link SfuCallSession} under fake timers — the session
 *     pings on the interval, a Pong keeps it alive, silence past the deadline
 *     fails the call, and nothing pings after `hangUp()`.
 *
 * The signaling socket + wasm + SFrame transform are mocked (the scaffolding
 * of `sfu-media-state.test.ts`); `deferPublish` keeps `connect()` off the
 * `getUserMedia` / `RTCPeerConnection` path.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  SFU_KEEPALIVE_DEADLINE_MS,
  SFU_KEEPALIVE_PING_INTERVAL_MS,
  SFU_KEEPALIVE_TICK_MS,
  sfuKeepaliveAction,
} from "./sfu-keepalive";

// ---- Pure rule ----------------------------------------------------------

describe("sfuKeepaliveAction", () => {
  it("pins the shared constants", () => {
    expect(SFU_KEEPALIVE_PING_INTERVAL_MS).toBe(15_000);
    expect(SFU_KEEPALIVE_DEADLINE_MS).toBe(45_000);
    expect(SFU_KEEPALIVE_TICK_MS).toBe(5_000);
  });

  it("waits before the ping interval has elapsed", () => {
    expect(sfuKeepaliveAction(1_000, 0, 0)).toBe("wait");
    expect(sfuKeepaliveAction(14_999, 0, 0)).toBe("wait");
  });

  it("pings on the first tick at or past the interval", () => {
    expect(sfuKeepaliveAction(15_000, 0, 0)).toBe("ping");
    expect(sfuKeepaliveAction(20_000, 0, 0)).toBe("ping");
    // Once a ping was sent, the next one waits for another full interval.
    expect(sfuKeepaliveAction(20_000, 0, 15_000)).toBe("wait");
    expect(sfuKeepaliveAction(30_000, 0, 15_000)).toBe("ping");
  });

  it("is dead past the deadline even when a ping is also due (dead wins)", () => {
    // A ping is due (45 s since the last one) AND the deadline has passed.
    expect(sfuKeepaliveAction(45_000, 0, 0)).toBe("dead");
    expect(sfuKeepaliveAction(50_000, 0, 30_000)).toBe("dead");
  });

  it("an inbound frame resets the deadline", () => {
    // Silence since 0 would be dead at 45 s...
    expect(sfuKeepaliveAction(45_000, 0, 30_000)).toBe("dead");
    // ...but a Pong at 30 s moves the deadline to 75 s.
    expect(sfuKeepaliveAction(45_000, 30_000, 30_000)).toBe("ping");
    expect(sfuKeepaliveAction(74_999, 30_000, 60_000)).toBe("wait");
    expect(sfuKeepaliveAction(75_000, 30_000, 60_000)).toBe("dead");
  });
});

// ---- Session wiring -----------------------------------------------------

const hooks = vi.hoisted(() => ({
  captured: null as {
    onWelcome?: (f: unknown) => void;
    onPong?: (f: { kind: "pong"; nonce: number }) => void;
    onClose?: () => void;
    onSocketError?: () => void;
  } | null,
  pings: [] as number[],
}));

vi.mock("./sfu-signaling-client", () => ({
  SFU_SUBPROTOCOL: "lakko-sfu-v1",
  SfuSignalingClient: class {
    constructor(_base: string, _jwt: string, callbacks: unknown) {
      hooks.captured = callbacks as typeof hooks.captured;
    }
    async connect(): Promise<void> {}
    close(): void {}
    get isOpen(): boolean {
      return true;
    }
    sendSignalRelayToCall(): void {}
    sendSignalRelayToPeer(): void {}
    sendSdpOffer(): void {}
    sendSdpAnswer(): void {}
    sendIceCandidate(): void {}
    sendPing(nonce: number): void {
      hooks.pings.push(nonce);
    }
  },
}));

vi.mock("./sframe-transform", () => ({
  isSFrameTransformSupported: (): boolean => true,
  attachSenderSFrame: (): string => "s",
  attachReceiverSFrame: (): string => "r",
  rekeySFrame: (): void => {},
}));

vi.mock("../wasm", () => ({
  loadWasm: async () => ({
    IncomingEnvelopeKind: Object.freeze({ MediaState: 3 }),
    sfuDecodePayload: (b64: string): Uint8Array => new TextEncoder().encode(b64),
    dispatchIncomingEnvelope: (): never => {
      throw new Error("not dispatched in this suite");
    },
    composeMediaState: (): Uint8Array => new Uint8Array(0),
  }),
}));

import { SfuCallSession, type SfuCallConfig, type SfuCallState } from "./sfu-call-session";

function baseConfig(overrides: Partial<SfuCallConfig> = {}): SfuCallConfig {
  return {
    sfuBaseUrl: "wss://sfu.test",
    jwt: "jwt",
    seed: new Uint8Array(32),
    localLeaf: 0,
    media: { audio: true, video: false },
    deferPublish: true,
    nowMs: () => Date.now(),
    ...overrides,
  };
}

function pong(nonce: number): void {
  hooks.captured?.onPong?.({ kind: "pong", nonce });
}

beforeEach(() => {
  vi.useFakeTimers();
  hooks.captured = null;
  hooks.pings = [];
});

afterEach(() => {
  vi.useRealTimers();
  vi.clearAllMocks();
});

describe("SfuCallSession — signaling keepalive", () => {
  it("pings on the interval, stays alive on Pong, fails once the deadline passes in silence", async () => {
    const states: SfuCallState[] = [];
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const session = new SfuCallSession(baseConfig(), {
      onStateChange: (s) => states.push(s),
    });
    await session.connect();
    expect(hooks.pings).toEqual([]);

    // 15 s: exactly one ping.
    await vi.advanceTimersByTimeAsync(SFU_KEEPALIVE_PING_INTERVAL_MS);
    expect(hooks.pings).toEqual([1]);

    // The server answers; another 15 s: a second ping, still connected.
    pong(1);
    await vi.advanceTimersByTimeAsync(SFU_KEEPALIVE_PING_INTERVAL_MS);
    expect(hooks.pings).toEqual([1, 2]);
    expect(session.getState()).not.toBe("failed");
    expect(states).not.toContain("failed");

    // Silence: 45 s after the last Pong the link is declared dead.
    await vi.advanceTimersByTimeAsync(SFU_KEEPALIVE_DEADLINE_MS);
    expect(session.getState()).toBe("failed");
    expect(states).toContain("failed");
    expect(warn).toHaveBeenCalledTimes(1);
    expect(String(warn.mock.calls[0]?.[0])).toContain("keepalive dead");

    // Nothing pings after the failure tore the session down.
    const pingsAtFailure = hooks.pings.length;
    await vi.advanceTimersByTimeAsync(SFU_KEEPALIVE_DEADLINE_MS);
    expect(hooks.pings).toHaveLength(pingsAtFailure);
    warn.mockRestore();
  });

  it("a late Pong just before the deadline keeps the call alive", async () => {
    const session = new SfuCallSession(baseConfig());
    await session.connect();

    // Two pings go unanswered; a Pong lands at 40 s, inside the 45 s deadline.
    await vi.advanceTimersByTimeAsync(40_000);
    expect(hooks.pings).toEqual([1, 2]);
    pong(2);
    await vi.advanceTimersByTimeAsync(10_000);
    expect(session.getState()).toBe("connecting");
    session.hangUp();
  });

  it("sends no ping after hangUp()", async () => {
    const session = new SfuCallSession(baseConfig());
    await session.connect();
    session.hangUp();
    expect(session.getState()).toBe("closed");

    await vi.advanceTimersByTimeAsync(SFU_KEEPALIVE_DEADLINE_MS);
    expect(hooks.pings).toEqual([]);
    expect(session.getState()).toBe("closed");
  });
});

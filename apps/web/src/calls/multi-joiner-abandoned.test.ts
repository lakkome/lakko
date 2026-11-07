// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A call-link attempt the person left, or that a newer call replaced, writes
 * nothing to the shared call store when it finishes later.
 *
 * The store is one for the whole app. An abandoned attempt that set its
 * phase to `ended`, or reset it, did so under whatever call had the slot by
 * then, and the host's ended-phase effect hung that call up. The host passes
 * its attempt token in as `isCurrent`; these tests drive a real
 * `MultiJoinerCall` over a fake SFU session and read the real store.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const hooks = vi.hoisted(() => ({
  /** The callbacks the orchestrator handed its SFU session. */
  callbacks: null as {
    onStateChange?: (state: string) => void;
    onWelcome?: (self: string, roster: ReadonlyArray<{ peer_id: string }>) => void;
    onPeerJoined?: (peerId: string, pseudonym: string, tag: string | undefined) => void;
    onRemoteMediaState?: (peerId: string, cameraOn: boolean) => void;
  } | null,
  /** How many SFU sessions were built. */
  sessions: 0,
  /** How many of them were hung up. */
  hangUps: 0,
  /** What the fake session's `connect()` does. */
  connect: (async (): Promise<void> => {}) as () => Promise<void>,
  /** What the fake session reports as its state. */
  sessionState: "connecting" as string,
  /** Resolves the JWT mint; `null` for a mint that resolves by itself. */
  mintGate: null as Promise<void> | null,
}));

vi.mock("./sfu-call-session", () => ({
  SfuCallSession: class {
    constructor(_config: unknown, callbacks: unknown) {
      hooks.sessions += 1;
      hooks.callbacks = callbacks as typeof hooks.callbacks;
    }
    connect(): Promise<void> {
      return hooks.connect();
    }
    async publish(): Promise<void> {}
    hangUp(): void {
      hooks.hangUps += 1;
    }
    getState(): string {
      return hooks.sessionState;
    }
    rekey(): void {}
    setMicrophoneEnabled(): void {}
    setCameraEnabled(): void {}
    setOwnCallName(): void {}
    sendSignalRelayToCall(): void {}
    sendSignalRelayToPeer(): void {}
    setTurnProvider(): void {}
  },
}));

vi.mock("../wasm", () => ({
  loadWasm: async () => ({
    wrapCallKeyPackage: (): Uint8Array => new Uint8Array([1, 2, 3]),
  }),
}));

vi.mock("./call-jwt-client", () => ({
  CallJwtClient: class {
    async mintForCall(): Promise<{ jwt: string; turn: null }> {
      if (hooks.mintGate !== null) await hooks.mintGate;
      return { jwt: "test-jwt", turn: null };
    }
  },
}));

vi.mock("../devices/sync/state", () => ({
  groupIdHex: (bytes: Uint8Array): string =>
    Array.from(bytes)
      .map((b) => b.toString(16).padStart(2, "0"))
      .join(""),
}));

import {
  LinkCallAbandonedError,
  MultiJoinerCall,
  type MultiJoinerCallOptions,
} from "./multi-joiner-call";
import { callActions, callState } from "../state/call-state";

interface Attempt {
  opts: MultiJoinerCallOptions;
  /** The host's token check: flip to leave the attempt. */
  current: { value: boolean };
  errors: unknown[];
  ended: number;
}

function attempt(): Attempt {
  const a: Attempt = { opts: undefined as unknown as MultiJoinerCallOptions, current: { value: true }, errors: [], ended: 0 };
  a.opts = {
    identity: {
      signaturePublicKey: new Uint8Array(32).fill(7),
      createKeyPackage: (): unknown => ({}),
    },
    callId: new Uint8Array(16).fill(1),
    ownerAccountId: new Uint8Array(16).fill(2),
    ownerSignatureKey: new Uint8Array(32).fill(3),
    sfuBaseUrl: "wss://sfu.test",
    media: { audio: true, video: false },
    onError: (err: unknown) => a.errors.push(err),
    onEnded: () => {
      a.ended += 1;
    },
    isCurrent: () => a.current.value,
  } as unknown as MultiJoinerCallOptions;
  return a;
}

/** The store as a newer call would have left it. */
function seedNewerCall(): void {
  callActions.setPhase({ kind: "connected" });
  callActions.setParticipants([
    {
      id: "local",
      displayName: "You",
      isLocal: true,
      micOn: true,
      camOn: false,
      quality: "good",
      speaking: false,
    },
    {
      id: "peer-newer",
      displayName: "Someone",
      isLocal: false,
      micOn: true,
      camOn: true,
      quality: "good",
      speaking: false,
    },
  ]);
}

beforeEach(() => {
  hooks.callbacks = null;
  hooks.sessions = 0;
  hooks.hangUps = 0;
  hooks.connect = async (): Promise<void> => {};
  hooks.sessionState = "connecting";
  hooks.mintGate = null;
  callActions.reset();
});

afterEach(() => {
  callActions.reset();
  vi.clearAllMocks();
  vi.useRealTimers();
});

describe("an abandoned link-call attempt and the shared call store", () => {
  it("its late `closed` does not set the phase to ended under a newer call, and its hangUp does not reset the store", async () => {
    const a = attempt();
    const call = await MultiJoinerCall.join(a.opts);

    // The person left; a newer call now has the slot and the store.
    a.current.value = false;
    seedNewerCall();

    hooks.callbacks?.onStateChange?.("closed");
    expect(callState.phase().kind).toBe("connected");
    // The host is still told (it drops the report by the same token).
    expect(a.ended).toBe(1);

    call.hangUp();
    expect(hooks.hangUps).toBe(1);
    expect(callState.phase().kind).toBe("connected");
    expect(callState.participants().map((p) => p.id)).toEqual(["local", "peer-newer"]);
  });

  it("none of its roster or camera reports reach the newer call's roster", async () => {
    const a = attempt();
    const call = await MultiJoinerCall.join(a.opts);
    a.current.value = false;
    seedNewerCall();

    hooks.callbacks?.onStateChange?.("connecting");
    hooks.callbacks?.onStateChange?.("connected");
    hooks.callbacks?.onWelcome?.("self", [{ peer_id: "old" }]);
    hooks.callbacks?.onPeerJoined?.("older", "p", undefined);
    hooks.callbacks?.onRemoteMediaState?.("newer", false);

    expect(callState.phase().kind).toBe("connected");
    expect(callState.participants().map((p) => p.id)).toEqual(["local", "peer-newer"]);
    expect(callState.participants().find((p) => p.id === "peer-newer")?.camOn).toBe(true);
    call.hangUp();
  });

  it("control: the current attempt still drives the store and resets it on hangUp", async () => {
    const a = attempt();
    const call = await MultiJoinerCall.join(a.opts);

    hooks.callbacks?.onStateChange?.("connecting");
    expect(callState.phase().kind).toBe("connecting");
    hooks.callbacks?.onWelcome?.("self", [{ peer_id: "x" }]);
    expect(callState.participants().map((p) => p.id)).toEqual(["local", "peer-x"]);
    hooks.callbacks?.onStateChange?.("closed");
    expect(callState.phase().kind).toBe("ended");

    call.hangUp();
    expect(callState.phase().kind).toBe("idle");
    expect(callState.participants()).toEqual([]);
  });

  it("control: without the option an attempt is always current", async () => {
    const a = attempt();
    const call = await MultiJoinerCall.join({ ...a.opts, isCurrent: undefined } as unknown as MultiJoinerCallOptions);
    hooks.callbacks?.onStateChange?.("closed");
    expect(callState.phase().kind).toBe("ended");
    call.hangUp();
    expect(callState.phase().kind).toBe("idle");
  });
});

describe("an attempt left before its session exists", () => {
  for (const role of ["host", "join"] as const) {
    it(`${role}: left during the token mint, no session is opened and no failure is reported`, async () => {
      const a = attempt();
      let releaseMint: () => void = () => {};
      hooks.mintGate = new Promise<void>((resolve) => {
        releaseMint = resolve;
      });
      const started = MultiJoinerCall[role](a.opts);
      const outcome = started.then(
        () => "resolved",
        (err: unknown) => err,
      );

      a.current.value = false;
      seedNewerCall();
      releaseMint();

      expect(await outcome).toBeInstanceOf(LinkCallAbandonedError);
      expect(hooks.sessions).toBe(0);
      expect(a.errors).toEqual([]);
      expect(callState.phase().kind).toBe("connected");
    });
  }

  it("join: left while signaling opened, no admission timer is started", async () => {
    vi.useFakeTimers();
    const a = attempt();
    let opened: () => void = () => {};
    hooks.connect = (): Promise<void> =>
      new Promise<void>((resolve) => {
        opened = resolve;
      });
    const started = MultiJoinerCall.join(a.opts);
    // Let the mint and the constructor run.
    for (let i = 0; i < 20; i += 1) await Promise.resolve();
    expect(hooks.sessions).toBe(1);

    a.current.value = false;
    opened();
    const call = await started;
    expect(vi.getTimerCount()).toBe(0);
    call.hangUp();
  });
});

describe("a start that ended in the session's connect", () => {
  it("hung up by this device while connecting: not reported as an error", async () => {
    const a = attempt();
    hooks.connect = async (): Promise<void> => {
      hooks.sessionState = "closed";
      throw new Error("SFU signaling was closed before it connected");
    };
    await expect(MultiJoinerCall.host(a.opts)).rejects.toThrow();
    expect(a.errors).toEqual([]);
  });

  it("control: a connect that failed is reported", async () => {
    const a = attempt();
    hooks.connect = async (): Promise<void> => {
      hooks.sessionState = "failed";
      throw new Error("SFU WebSocket errored before opening");
    };
    await expect(MultiJoinerCall.host(a.opts)).rejects.toThrow();
    expect(a.errors).toHaveLength(1);
  });
});

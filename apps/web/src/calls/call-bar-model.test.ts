// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/calls.md` section 4.5 — the collapsed call bar's seam onto the
 * shared rules.
 *
 * ## What these tests prove, and what they deliberately do not
 *
 * Whether the bar is shown, and which of four things its line says, are the
 * rules of `crates/lakko-ui-rules/src/call_bar.rs` and are asserted there — that
 * a ringing call has no bar, that an outgoing one has one before anyone answers,
 * that only a connected call reports a running time, that the line says exactly
 * one thing. Restating those here would be a second copy of the paragraph the
 * shared crate exists to have one of, and a copy that drifted would keep
 * passing.
 *
 * So these cover the WIRING, which is the part that can only be wrong on this
 * client: that this client's store phase reaches the rule as the right member of
 * an enum it does not own, and that the pair the rule answers — a kind, split
 * across two wasm calls because wasm-bindgen cannot hand back an enum with data
 * — is re-joined into one line and never into two.
 *
 * ## Both ways round, on purpose
 *
 * The first half drives a spy, so it can assert WHAT the rule was asked rather
 * than only what came back. The second half drives the REAL wasm rule, because a
 * spy cannot catch the one mistake the mapping can make on its own: handing the
 * rule a phase that is not the one the store is in. A spy answering whatever it
 * likes would agree with a mapping that sent `Connected` for a call that was
 * still ringing.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import {
  callBarMuteControl,
  callBarPhase,
  callBarStatus,
  callBarWouldShow,
  type CallBarRules,
} from "./call-bar-model";
import type { CallPhaseKind } from "./call-screen-model";

/**
 * A stand-in for the shared rules that records what it was asked.
 *
 * Its `CallBarPhase` carries the phase NAMES rather than the wasm enum's
 * numbers, so a wrong mapping shows up in an assertion as a readable word
 * instead of an integer nobody can check by eye. The cast is the price: the
 * enum's members are opaque to this client by design (see the module's note on
 * why the discriminants are not written down here).
 */
function spy(answers: {
  visible?: boolean;
  kind?: string;
  elapsed?: bigint | undefined;
}): CallBarRules & { readonly asked: string[] } {
  const asked: string[] = [];
  const names = {
    Idle: "Idle",
    Ringing: "Ringing",
    Preparing: "Preparing",
    Calling: "Calling",
    Connecting: "Connecting",
    Connected: "Connected",
    Reconnecting: "Reconnecting",
    Ended: "Ended",
  } as unknown as CallBarRules["CallBarPhase"];
  return {
    asked,
    CallBarPhase: names,
    callBarVisible(phase, callScreenUp) {
      asked.push(`visible(${String(phase)}, ${String(callScreenUp)})`);
      return answers.visible ?? false;
    },
    callBarStatusKind(phase) {
      asked.push(`kind(${String(phase)})`);
      return answers.kind ?? "calling";
    },
    callBarElapsedMs(phase, connectedForMs) {
      asked.push(`elapsed(${String(phase)}, ${connectedForMs.toString()})`);
      return answers.elapsed;
    },
  };
}

describe("callBarPhase — this client's store, as the shared rule sees it", () => {
  it("hands over the phase the store is in", () => {
    const rules = spy({});
    const pairs: ReadonlyArray<readonly [CallPhaseKind, string]> = [
      ["idle", "Idle"],
      ["calling", "Calling"],
      ["connecting", "Connecting"],
      ["connected", "Connected"],
      ["reconnecting", "Reconnecting"],
      ["ended", "Ended"],
    ];
    for (const [store, expected] of pairs) {
      expect(String(callBarPhase(rules, store, false))).toBe(expected);
    }
  });

  /**
   * The store has no `ringing`: a ringing 1x1 call takes the whole screen (§2.2)
   * and `CallHostContext.syncCallUiFromState` resets the store for it. So the
   * ring is a separate fact and has to win over whatever the store still holds —
   * otherwise a call ringing in from a chat the person had just been on would be
   * reported by whatever the previous call left behind.
   */
  it("reports a ringing call as ringing whatever the store says", () => {
    const rules = spy({});
    for (const store of ["idle", "connected", "ended"] as const) {
      expect(String(callBarPhase(rules, store, true))).toBe("Ringing");
    }
  });

  /** An outgoing call is dialled in the same breath as it is built, so the store
   *  has one word for both and the rule's own note says both answer `Calling`.
   *  `Preparing` is therefore never sent, and that is deliberate rather than an
   *  omission. */
  it("never sends Preparing, which this client has no state for", () => {
    const rules = spy({});
    const every: readonly CallPhaseKind[] = [
      "idle",
      "calling",
      "connecting",
      "connected",
      "reconnecting",
      "ended",
    ];
    for (const store of every) {
      for (const ringing of [false, true]) {
        expect(String(callBarPhase(rules, store, ringing))).not.toBe("Preparing");
      }
    }
  });
});

describe("callBarWouldShow — may this call be put away", () => {
  /** The whole point of the helper: it asks the visibility rule with the call
   *  screen AWAY, because the question is whether there is a bar to collapse
   *  into — not whether one is showing right now, which it never is while the
   *  person is looking at the screen they are about to leave. */
  it("asks the shared rule with the call screen away", () => {
    const rules = spy({ visible: true });
    expect(callBarWouldShow(rules, "connected", false)).toBe(true);
    expect(rules.asked).toEqual(["visible(Connected, false)"]);
  });

  it("returns the rule's answer untouched", () => {
    expect(callBarWouldShow(spy({ visible: false }), "connected", false)).toBe(false);
  });
});

describe("callBarStatus — one line, never two things", () => {
  it("says the word for each of the three word kinds, and asks for no time", () => {
    for (const [kind, text] of [
      ["calling", "Calling..."],
      ["connecting", "Connecting..."],
      ["reconnecting", "Reconnecting..."],
    ] as const) {
      const rules = spy({ kind });
      const line = callBarStatus(rules, {
        phase: rules.CallBarPhase.Connected,
        connectedAtMs: 1_000,
        nowMs: 99_000,
      });
      expect(line.text).toBe(text);
      // A word kind must not reach for a running time at all: reading the
      // elapsed value without the kind is the mistake the pair exists to
      // prevent, and asking for it here would be the first half of making it.
      expect(rules.asked.some((a) => a.startsWith("elapsed("))).toBe(false);
    }
  });

  it("formats the running time the way the call screen formats its own", () => {
    const rules = spy({ kind: "elapsed", elapsed: 124_000n });
    const line = callBarStatus(rules, {
      phase: rules.CallBarPhase.Connected,
      connectedAtMs: 1_000,
      nowMs: 125_000,
    });
    expect(line.text).toBe("2:04");
    // The duration handed to the rule is the one this client measured, and the
    // rule is what carries it back — the model never reads its own clock.
    expect(rules.asked).toEqual(["kind(Connected)", "elapsed(Connected, 124000)"]);
  });

  /** A clock that steps backwards must not print a negative duration, which is
   *  the same flooring `elapsedMs` does for the call screen's line. */
  it("floors a backwards clock at zero", () => {
    const rules = spy({ kind: "elapsed", elapsed: 0n });
    callBarStatus(rules, {
      phase: rules.CallBarPhase.Connected,
      connectedAtMs: 9_000,
      nowMs: 1_000,
    });
    expect(rules.asked).toContain("elapsed(Connected, 0)");
  });

  /** Cannot happen — the pair comes from one rule over one phase — but if it
   *  ever does, nothing beats a `0:00` that would state the call had just
   *  connected. */
  it("says nothing rather than zero when an elapsed verdict carries no time", () => {
    const rules = spy({ kind: "elapsed", elapsed: undefined });
    expect(
      callBarStatus(rules, {
        phase: rules.CallBarPhase.Connected,
        connectedAtMs: 1_000,
        nowMs: 99_000,
      }).text,
    ).toBe("");
  });

  it("says nothing rather than inventing a state for a kind it does not know", () => {
    const rules = spy({ kind: "on-hold" });
    expect(
      callBarStatus(rules, {
        phase: rules.CallBarPhase.Connected,
        connectedAtMs: null,
        nowMs: 0,
      }),
    ).toEqual({ text: "", spoken: "" });
  });

  /** §8: `12:04` read back by a screen reader is a time of day. Every other
   *  kind already reads as words. */
  it("announces a running time as a duration and a word as itself", () => {
    const timed = spy({ kind: "elapsed", elapsed: 65_000n });
    expect(
      callBarStatus(timed, {
        phase: timed.CallBarPhase.Connected,
        connectedAtMs: 0,
        nowMs: 65_000,
      }).spoken,
    ).toBe("1 minute 5 seconds");

    const worded = spy({ kind: "reconnecting" });
    expect(
      callBarStatus(worded, {
        phase: worded.CallBarPhase.Reconnecting,
        connectedAtMs: 0,
        nowMs: 0,
      }).spoken,
    ).toBe("Reconnecting");
  });
});

describe("callBarMuteControl — drawn as the call screen draws it", () => {
  /** Not a second opinion about how mute looks: the button is taken out of the
   *  call screen's own control row, so §4.1.1's two states (quiet `outline` when
   *  live, loud `solid` when muted) and the slashed glyph come from one
   *  definition. A change to how the screen paints mute moves the bar with it. */
  it("takes the microphone entry out of the call screen's own row", () => {
    const live = callBarMuteControl("connected", true);
    expect(live).not.toBeNull();
    expect(live?.id).toBe("mic");
    expect(live?.icon).toBe("mic");
    expect(live?.tone).toBe("outline");
    expect(live?.pressed).toBe(false);
    expect(live?.disabled).toBe(false);

    const muted = callBarMuteControl("connected", false);
    expect(muted?.icon).toBe("micOff");
    expect(muted?.tone).toBe("solid");
    expect(muted?.pressed).toBe(true);
    expect(muted?.accessibleName).toBe("Unmute microphone");
  });

  /** §4.1.1 — never a hidden button. While a call is only `Calling` there is
   *  nothing yet to mute, so the control is inert rather than absent, and the
   *  bar inherits that from the row rather than deciding it again. */
  it("is inert while the call is still ringing out", () => {
    expect(callBarMuteControl("calling", true)?.disabled).toBe(true);
    expect(callBarMuteControl("connected", true)?.disabled).toBe(false);
  });
});

describe("against the real shared rule", () => {
  beforeAll(async () => {
    const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
    await init({ module_or_path: readFileSync(wasmPath) });
  });

  /**
   * The one mistake a spy cannot catch: the mapping handing the rule a phase
   * that is not the one the store is in. A spy answers whatever it was told to,
   * so it would agree with a mapping that sent `Connected` for a ringing call.
   *
   * These assertions are about the PAIRING of a store state with an outcome, not
   * about the outcomes themselves — those are the crate's, asserted there. Each
   * line here would fail if the enum member for that state were wrong.
   */
  it("maps this client's states onto the outcomes the spec names for them", () => {
    const rules: CallBarRules = wasm;
    const ask = (store: CallPhaseKind, ringing = false) => {
      const phase = callBarPhase(rules, store, ringing);
      return {
        shown: rules.callBarVisible(phase, false),
        overScreen: rules.callBarVisible(phase, true),
        line: callBarStatus(rules, { phase, connectedAtMs: 0, nowMs: 754_000 }).text,
      };
    };

    // "An outgoing call collapses from the moment it is placed, and the bar
    //  reads `Calling...`."
    expect(ask("calling")).toEqual({ shown: true, overScreen: false, line: "Calling..." });
    expect(ask("connecting")).toEqual({
      shown: true,
      overScreen: false,
      line: "Connecting...",
    });
    // A connected call shows the running time and nothing else.
    expect(ask("connected")).toEqual({ shown: true, overScreen: false, line: "12:34" });
    expect(ask("reconnecting")).toEqual({
      shown: true,
      overScreen: false,
      line: "Reconnecting...",
    });
    // "A call that is only ringing does not collapse."
    expect(ask("idle", true).shown).toBe(false);
    // "When a collapsed call ends, the bar goes away."
    expect(ask("ended").shown).toBe(false);
    expect(ask("idle").shown).toBe(false);
  });

  /** "The bar is never shown over the call screen. One state, one place." */
  it("is never shown over the call screen, whatever the call is doing", () => {
    const rules: CallBarRules = wasm;
    const every: readonly CallPhaseKind[] = [
      "idle",
      "calling",
      "connecting",
      "connected",
      "reconnecting",
      "ended",
    ];
    for (const store of every) {
      for (const ringing of [false, true]) {
        expect(rules.callBarVisible(callBarPhase(rules, store, ringing), true)).toBe(false);
      }
    }
  });

  /**
   * What the chevron and `Esc` read. The two states that must answer `false` are
   * the ones where collapsing would leave a live-looking app with no call
   * surface anywhere: an ended call, whose overlay and End button would become
   * unreachable, and a ringing one, which is answered or declined rather than
   * put away.
   */
  it("permits collapsing exactly the calls that have a bar to collapse into", () => {
    const rules: CallBarRules = wasm;
    expect(callBarWouldShow(rules, "calling", false)).toBe(true);
    expect(callBarWouldShow(rules, "connected", false)).toBe(true);
    expect(callBarWouldShow(rules, "reconnecting", false)).toBe(true);
    expect(callBarWouldShow(rules, "ended", false)).toBe(false);
    expect(callBarWouldShow(rules, "idle", false)).toBe(false);
    expect(callBarWouldShow(rules, "idle", true)).toBe(false);
  });

  /** The invariant the shared rule is a sum type for, checked once through this
   *  client's own seam: whatever the phase, the line is a word or a time and
   *  never both, and it is never a word with digits in it. */
  it("never produces a line that says two things", () => {
    const rules: CallBarRules = wasm;
    const every: readonly CallPhaseKind[] = [
      "calling",
      "connecting",
      "connected",
      "reconnecting",
    ];
    for (const store of every) {
      const phase = callBarPhase(rules, store, false);
      const text = callBarStatus(rules, { phase, connectedAtMs: 0, nowMs: 754_000 }).text;
      const hasWord = /[A-Za-z]/u.test(text);
      const hasTime = /\d+:\d\d/u.test(text);
      expect([hasWord, hasTime]).not.toEqual([true, true]);
      expect(hasWord || hasTime).toBe(true);
    }
  });
});

describe("the label the bar and the call screen share", () => {
  /**
   * §4.5: the bar carries "the same label the call screen uses". The rule was
   * lifted out of `routes/Call.tsx` to make that true by construction, and these
   * assertions are the reason it could not be checked before: nothing in a
   * `.tsx` is reachable from this suite, so the copy that lived in the view was
   * a rule nothing could assert.
   */
  it("names the person, names the group, and never invents either", async () => {
    const { callPeerLabel } = await import("./call-screen-model");
    expect(
      callPeerLabel({
        surface: "voice",
        peerName: "Anna",
        participantCount: 2,
        soleRemoteName: "leaf-1",
      }),
    ).toBe("Anna");
    // A group call has no single other person: the label is the group's
    // name, as on iOS and Android, and `Group call` when it has none or the
    // call came from a link.
    expect(
      callPeerLabel({
        surface: "group",
        peerName: "Anna",
        participantCount: 4,
        soleRemoteName: null,
        groupName: "  Weekend plans ",
      }),
    ).toBe("Weekend plans");
    expect(
      callPeerLabel({
        surface: "group",
        peerName: "Anna",
        participantCount: 4,
        soleRemoteName: null,
        groupName: null,
      }),
    ).toBe("Group call");
    // The host resolves a name for every 1x1 call, so this is practically
    // unreachable — but it must not invent a person if it is, and
    // `encryption-chip.ts` treats this exact word as "no name".
    expect(
      callPeerLabel({
        surface: "voice",
        peerName: "   ",
        participantCount: 2,
        soleRemoteName: "",
      }),
    ).toBe("Participant");
    // The sole remote's own name, when the host had none.
    expect(
      callPeerLabel({
        surface: "video",
        peerName: null,
        participantCount: 2,
        soleRemoteName: "Bo",
      }),
    ).toBe("Bo");
  });
});

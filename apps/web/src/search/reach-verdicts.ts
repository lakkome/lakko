// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Whether a kept grant still reaches a person (`docs/ux/who-can-find-me.md`
 * section 6.1, "A kept grant is checked before it is offered").
 *
 * A person this device knows from a group it shared with them may have left
 * that group, and then the grant it kept no longer opens their mailbox
 * (ADR-0078 section 9). So a local row offers **Add contact** or shows
 * `@username` only after the Delivery Service said one of the kept grants
 * still reaches them. Every grant is tried, in order, up to the first that
 * passes; no token is spent, there is no minimum length and no debounce.
 *
 * Until the answer arrives the person holds no grant: the row is a name. If
 * none passes that stays. If the check could not be made (a connection
 * failure) nothing was learned: a verdict already held stands, with none the
 * row is what it was before this check existed, and the send path checks
 * again.
 *
 * A check answers for one text (UX 6.1, "A row says what is true now"): a
 * person's reach changes while the field is open, so every change of the text
 * asks again about everyone it matches, whatever is held. The last answer
 * stays until the new one arrives, so a row does not flicker as the person
 * types.
 *
 * Verdicts are applied to the CANDIDATES, before the local match, so a person
 * nobody may find is matched by name alone and `localPeople` stays the one
 * match. A verdict answers for the grants it was asked about: a new grant
 * for the person voids it.
 *
 * No Solid, no wasm, no network: the check is an argument.
 */
import { localPeople, type LocalCandidate, type Normalize } from "./people";

/** How many people are checked at one time: the first letter typed can match
 *  every member of every group. Chosen, not measured. */
export const REACH_CHECKS_AT_ONCE = 4;

/** What a finished check said. `unanswered` is not a verdict: the check could
 *  not be made. */
export type ReachVerdict =
  | { readonly kind: "live"; readonly wireB64: string }
  | { readonly kind: "dead" }
  | { readonly kind: "unanswered" };

/**
 * Try the grants in order, stopping at the first that passes. All refused is
 * `dead`. Throws what the check throws: a check that could not be made says
 * nothing about the grants after it.
 */
export async function firstLiveGrant(
  wires: readonly string[],
  check: (wireB64: string) => Promise<boolean>,
): Promise<ReachVerdict> {
  for (const wireB64 of wires) {
    if (await check(wireB64)) return { kind: "live", wireB64 };
  }
  return { kind: "dead" };
}

/** The candidates the query matches: the same match the row gets, over what
 *  is held, so a person matched by a username not shown yet is asked about. */
export function candidatesMatching(
  candidates: readonly LocalCandidate[],
  query: string,
  normalize: Normalize,
  ownKeyHex: string | null,
): LocalCandidate[] {
  const matched = new Set(localPeople(candidates, query, normalize, ownKeyHex).map((p) => p.keyHex));
  return candidates.filter((c) => matched.has(c.keyHex));
}

/** What a candidate is once a verdict (or none yet) is applied. */
export function withVerdict(c: LocalCandidate, verdict: ReachVerdict | null): LocalCandidate {
  // A contact never waits, and a person with no grant has nothing to ask.
  if (c.isContact || c.keptGrantWires.length === 0) return c;
  if (verdict === null || verdict.kind === "dead") return { ...c, keptGrantWires: [] };
  if (verdict.kind === "unanswered") return c;
  // The grant that passed goes first, for the row. The others stay: a verdict
  // is an answer about one moment, and the send tries everything held.
  const rest = c.keptGrantWires.filter((w) => w !== verdict.wireB64);
  return { ...c, keptGrantWires: [verdict.wireB64, ...rest], grantChecked: true };
}

/** What a finished check leaves. One that could not be made learned nothing,
 *  so it never replaces a verdict. */
export function verdictAfter(held: ReachVerdict | null, answer: ReachVerdict): ReachVerdict {
  if (answer.kind !== "unanswered" || held === null) return answer;
  return held;
}

interface Entry {
  /** The grants the verdict answers for. */
  readonly wires: string;
  readonly verdict: ReachVerdict;
}

const wiresOf = (c: LocalCandidate): string => c.keptGrantWires.join("\u0000");

/** The verdicts of one search field, by the person's key. */
export class ReachVerdicts {
  private readonly reachCheck: (keyB64: string, wireB64: string) => Promise<boolean>;
  private readonly onChange: () => void;
  private readonly entries = new Map<string, Entry>();
  /** Everyone with a check queued or running: at most one per person. */
  private readonly inFlight = new Set<string>();
  private readonly queue: LocalCandidate[] = [];
  private running = 0;
  /** The text the last {@link ask} was for. */
  private askedText: string | null = null;

  constructor(opts: {
    /** The DS reach check of the person's mailbox with a grant. */
    readonly reachCheck: (keyB64: string, wireB64: string) => Promise<boolean>;
    /** A check finished: {@link apply} may answer differently. */
    readonly onChange: () => void;
  }) {
    this.reachCheck = opts.reachCheck;
    this.onChange = opts.onChange;
  }

  /** The verdict for a candidate's grants as they stand now, or `null`. */
  verdictOf(c: LocalCandidate): ReachVerdict | null {
    const entry = this.entries.get(c.keyHex);
    return entry !== undefined && entry.wires === wiresOf(c) ? entry.verdict : null;
  }

  /** Start a check for every candidate that holds grants, as the field holds
   *  `text`: everyone when the text changed, else whoever has no verdict. At
   *  most one check runs per person. */
  ask(candidates: readonly LocalCandidate[], text: string): void {
    const textChanged = text !== this.askedText;
    this.askedText = text;
    for (const c of candidates) {
      if (c.isContact || c.keptGrantWires.length === 0 || this.inFlight.has(c.keyHex)) continue;
      const verdict = this.verdictOf(c);
      if (!textChanged && verdict !== null && verdict.kind !== "unanswered") continue;
      this.inFlight.add(c.keyHex);
      this.queue.push(c);
    }
    this.pump();
  }

  /** The field is empty: nothing held answers for the next text. */
  clear(): void {
    this.entries.clear();
    this.askedText = null;
  }

  /** The candidates with the verdicts applied. */
  apply(candidates: readonly LocalCandidate[]): LocalCandidate[] {
    return candidates.map((c) => withVerdict(c, this.verdictOf(c)));
  }

  /** A send was refused by the reach check. */
  refused(keyHex: string): void {
    const entry = this.entries.get(keyHex);
    if (entry === undefined) return;
    this.entries.set(keyHex, { wires: entry.wires, verdict: { kind: "dead" } });
    this.onChange();
  }

  private pump(): void {
    while (this.running < REACH_CHECKS_AT_ONCE) {
      const c = this.queue.shift();
      if (c === undefined) return;
      this.running += 1;
      void this.check(c).finally(() => {
        this.running -= 1;
        this.pump();
      });
    }
  }

  private async check(c: LocalCandidate): Promise<void> {
    let answer: ReachVerdict;
    try {
      answer = await firstLiveGrant(c.keptGrantWires, (wire) => this.reachCheck(c.keyB64, wire));
    } catch {
      answer = { kind: "unanswered" };
    }
    this.inFlight.delete(c.keyHex);
    this.entries.set(c.keyHex, { wires: wiresOf(c), verdict: verdictAfter(this.verdictOf(c), answer) });
    this.onChange();
  }
}

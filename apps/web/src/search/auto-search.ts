// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * When the search field asks the network (`docs/ux/who-can-find-me.md`
 * section 6.1, "When it asks the network"). Nothing to press:
 *
 * - from {@link NETWORK_SEARCH_MIN_CHARS} characters, after
 *   {@link NETWORK_SEARCH_DEBOUNCE_MS} without typing, the field asks the
 *   network by username and by name at once;
 * - a leading `@` is ignored, and a text that cannot be a username and has
 *   no searchable name term asks nothing;
 * - a text asked in the last {@link SEARCH_ANSWER_KEPT_MS} is not asked again:
 *   its answer is shown from memory. Only an ANSWER is remembered (found, not
 *   found), and it ages: a person may have widened who can find them. Its age
 *   is read when a text is asked; no timer re-asks an unchanged text. A
 *   network failure or "out of searches" is not remembered, so typing again
 *   retries;
 * - a result already shown stays while the person keeps typing, until the
 *   new answer arrives.
 *
 * No Solid, no wasm, no network: the rules, the search and the clock are
 * arguments, so the whole behaviour is testable with fake timers.
 */
import type { Normalize } from "./people";
import type { PeopleSearchOutcome } from "./people-search";

/** Fewest characters (after a leading `@`) the network is asked with. Chosen,
 *  not measured (UX section 6.1). */
export const NETWORK_SEARCH_MIN_CHARS = 3;
/** How long the person must stop typing before the network is asked. Chosen,
 *  not measured (UX section 6.1). */
export const NETWORK_SEARCH_DEBOUNCE_MS = 500;
/** At most this many answers are remembered per session; the oldest goes
 *  first. Chosen, not measured. */
export const MAX_REMEMBERED_SEARCHES = 200;
/** How long an answer is reused for its text, a not-found included. Chosen, not
 *  measured (UX section 6.1). */
export const SEARCH_ANSWER_KEPT_MS = 60_000;

/** What decides whether a text may be asked. */
export interface NetworkQueryRules {
  /** The canonical username the text could be, or `null`. */
  canonicalUsername(typed: string): string | null;
  /** The name term the network would be asked with, or `null`. */
  queryTerm(query: string): string | null;
  /** The search normaliser: what makes two texts "the same search". */
  normalize: Normalize;
}

/** A text the network may be asked with. */
export interface NetworkQuery {
  /** The text handed to the search (leading `@` and outer spaces removed). */
  readonly query: string;
  /** What a session remembers the answer under. */
  readonly key: string;
}

/** The text the network would be asked with, or `null` when nothing is asked. */
export function networkQueryOf(typed: string, rules: NetworkQueryRules): NetworkQuery | null {
  const bare = typed.trim().replace(/^@+/u, "").trim();
  if ([...bare].length < NETWORK_SEARCH_MIN_CHARS) return null;
  if (rules.canonicalUsername(bare) === null && rules.queryTerm(bare) === null) return null;
  const key = rules.normalize(bare) ?? bare.toLowerCase();
  return { query: bare, key };
}

/** Whether an outcome is an answer worth remembering for the session. */
export function isRememberedAnswer(outcome: PeopleSearchOutcome): boolean {
  return outcome.state.kind === "found" || outcome.state.kind === "not-found";
}

/** The answers of this session, by search key. Bounded; insertion-ordered. */
export class SearchMemory {
  private readonly answers = new Map<
    string,
    { readonly outcome: PeopleSearchOutcome; readonly atMs: number }
  >();

  constructor(
    private readonly max: number = MAX_REMEMBERED_SEARCHES,
    /** A clock that never runs backwards, in milliseconds. */
    private readonly now: () => number = () => performance.now(),
  ) {}

  /** The answer for `key`, while it is young enough to reuse. */
  get(key: string): PeopleSearchOutcome | undefined {
    const kept = this.answers.get(key);
    if (kept === undefined) return undefined;
    if (this.now() - kept.atMs < SEARCH_ANSWER_KEPT_MS) return kept.outcome;
    this.answers.delete(key);
    return undefined;
  }

  /** Remember `outcome` when it is an answer; anything else is forgotten. */
  remember(key: string, outcome: PeopleSearchOutcome): void {
    if (!isRememberedAnswer(outcome)) {
      this.answers.delete(key);
      return;
    }
    this.answers.delete(key);
    this.answers.set(key, { outcome, atMs: this.now() });
    while (this.answers.size > this.max) {
      const oldest = this.answers.keys().next().value;
      if (oldest === undefined) break;
      this.answers.delete(oldest);
    }
  }

  /** Forget every answer `stale` calls no longer true. */
  forgetWhere(stale: (outcome: PeopleSearchOutcome) => boolean): void {
    for (const [key, kept] of [...this.answers]) {
      if (stale(kept.outcome)) this.answers.delete(key);
    }
  }

  clear(): void {
    this.answers.clear();
  }
}

/** The network half as the field shows it. */
export interface NetworkView {
  /** The answer shown: the current text's, or an earlier one while the
   *  current text's is on its way. `null`: nothing under People. */
  readonly outcome: PeopleSearchOutcome | null;
  /** A question for the current text is in flight. */
  readonly searching: boolean;
  /** The current text will be asked once the typing pauses. No spinner yet,
   *  but nothing may be said to be missing either. */
  readonly waiting?: true;
}

/** The clock, injectable for the tests. */
export interface Timers {
  set(fn: () => void, ms: number): unknown;
  clear(handle: unknown): void;
}

const realTimers: Timers = {
  set: (fn, ms) => setTimeout(fn, ms),
  clear: (h) => clearTimeout(h as ReturnType<typeof setTimeout>),
};

/**
 * One search field's network half. Feed it what was typed with
 * {@link input}; it calls `onChange` with what to show.
 */
export class AutoSearch {
  private readonly run: (query: string) => Promise<PeopleSearchOutcome>;
  private readonly memory: SearchMemory;
  private readonly onChange: (view: NetworkView) => void;
  private readonly timers: Timers;
  private readonly delayMs: number;
  private readonly inFlight = new Map<string, Promise<PeopleSearchOutcome>>();
  private timer: unknown = null;
  /** The key of the text the field holds now (`null`: nothing to ask). */
  private current: string | null = null;
  /** The text behind {@link current}, for asking it again. */
  private currentTarget: NetworkQuery | null = null;
  private view: NetworkView = { outcome: null, searching: false };
  private disposed = false;

  constructor(opts: {
    readonly run: (query: string) => Promise<PeopleSearchOutcome>;
    readonly memory: SearchMemory;
    readonly onChange: (view: NetworkView) => void;
    readonly timers?: Timers;
    readonly delayMs?: number;
  }) {
    this.run = opts.run;
    this.memory = opts.memory;
    this.onChange = opts.onChange;
    this.timers = opts.timers ?? realTimers;
    this.delayMs = opts.delayMs ?? NETWORK_SEARCH_DEBOUNCE_MS;
  }

  /** What was typed now, as {@link networkQueryOf} judged it. */
  input(target: NetworkQuery | null): void {
    if (this.disposed) return;
    if (target === null) {
      this.cancelTimer();
      this.current = null;
      this.currentTarget = null;
      this.show({ outcome: null, searching: false });
      return;
    }
    // The same search again: nothing new, unless its last question got no
    // answer (a failure, out of searches) - then typing is the retry.
    if (
      target.key === this.current &&
      (this.timer !== null || this.view.searching || this.memory.get(target.key) !== undefined)
    ) {
      return;
    }
    this.current = target.key;
    this.currentTarget = target;
    this.cancelTimer();
    const remembered = this.memory.get(target.key);
    if (remembered !== undefined) {
      this.show({ outcome: remembered, searching: false });
      return;
    }
    // The earlier answer stays; the spinner waits for the question itself.
    this.show({ outcome: this.view.outcome, searching: false, waiting: true });
    this.timer = this.timers.set(() => {
      this.timer = null;
      void this.ask(target);
    }, this.delayMs);
  }

  /**
   * Something this device knows moved under the answers: `stale` says which
   * are no longer true. They are forgotten, and the one on screen is asked
   * again at once, since nothing else would ask while the text stands.
   */
  recheck(stale: (outcome: PeopleSearchOutcome) => boolean): void {
    if (this.disposed) return;
    this.memory.forgetWhere(stale);
    const shown = this.view.outcome;
    const target = this.currentTarget;
    if (shown === null || target === null || !stale(shown)) return;
    this.cancelTimer();
    this.show({ outcome: null, searching: false });
    void this.ask(target);
  }

  /** Stop: nothing is shown or asked after this. */
  dispose(): void {
    this.disposed = true;
    this.cancelTimer();
  }

  private async ask(target: NetworkQuery): Promise<void> {
    if (this.disposed || this.current !== target.key) return;
    this.show({ outcome: this.view.outcome, searching: true });
    let pending = this.inFlight.get(target.key);
    if (pending === undefined) {
      pending = this.run(target.query);
      this.inFlight.set(target.key, pending);
    }
    let outcome: PeopleSearchOutcome;
    try {
      outcome = await pending;
    } catch {
      outcome = { people: [], channel: null, state: { kind: "failed" } };
    } finally {
      if (this.inFlight.get(target.key) === pending) this.inFlight.delete(target.key);
    }
    // An answer is remembered whatever the field holds now; it is shown only
    // when it is still the current text's.
    this.memory.remember(target.key, outcome);
    if (this.disposed || this.current !== target.key) return;
    this.show({ outcome, searching: false });
  }

  private cancelTimer(): void {
    if (this.timer !== null) {
      this.timers.clear(this.timer);
      this.timer = null;
    }
  }

  private show(view: NetworkView): void {
    this.view = view;
    this.onChange(view);
  }
}

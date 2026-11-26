// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The state behind `docs/ux/translation-and-transcription.md` on web: what
 * each bubble's quiet line says, what each voice message's transcript block
 * holds, the pack dialog and the language picker on screen, and the
 * per-conversation choices.
 *
 * Plain module with Solid signals, so it runs in the node test project with
 * fake engines and the real shared rules. `TranslationContext.tsx` mounts one
 * per session.
 *
 * What it guarantees, each stated where it is done:
 * - every engine it calls is an on-device one (`engines.ts`); a language whose
 *   engine is `unavailable` gets no control, and nothing is retried elsewhere;
 * - nothing is downloaded until the person pressed `Download` (UX 3.5);
 * - results are stored only through `ResultStore` (device only, encrypted,
 *   gated) and a stored one is used only while `storedResultIsCurrent` says it
 *   still describes the message;
 * - no message text, transcript, translation or language reaches a log line
 *   (ADR-0082 section 8): the two `console.warn` calls here name an engine
 *   step and an error name, nothing else.
 */

import { createSignal } from "solid-js";

import type { Detection, EngineState, Engines, OwnPack, PackDisclosure, SystemPacks } from "./engines";
import {
  CANDIDATE_LANGUAGES,
  connectionTypeKnown,
  messageKey,
  onCellular,
  SAME_LANGUAGE_LINE_MS,
} from "./model";
import {
  DEFAULT_PREFS,
  type ConversationPrefs,
  type ResultSlot,
  type ResultStore,
  type StoredTranscript,
  type StoredTranslation,
} from "./result-store";
import {
  barLanguageFor,
  candidateTags,
  conversationDetectionFor,
  resolveSourceFor,
  usualLanguage,
  withLanguageDetection,
  withLanguagePick,
  type SourceResolution,
  isCurrent,
  offerFor,
  transcriptOfferedFor,
  type Offer,
  type TranslationRules,
} from "./rules";
import type { PackUse, TranslationSettings } from "./settings";

/** The message a result describes. */
export interface MessageRef {
  readonly conversation: string;
  readonly messageId: number;
  /** The DS id of the message's latest edit or delete op, 0 when never edited
   *  (`MessageOpState.lastOpId`). */
  readonly lastOpId: number;
}

/** A translation slot: the message's text, or its transcript. */
export type TextSlot = "text" | "transcript-translation";

/** The quiet line under a bubble's text (UX 3.2 / 3.3 / 3.5). */
export type TextView =
  | { readonly phase: "none" }
  | { readonly phase: "translating"; readonly source: string | null }
  | { readonly phase: "translated"; readonly source: string; readonly text: string }
  | { readonly phase: "original"; readonly source: string }
  /** Shown before any translation (UX 3.2): the detected language of a
   *  message not in a reading language, with `Translate` when the device can
   *  translate from it. `Translate` here does what the menu row does. */
  | { readonly phase: "detected"; readonly source: string; readonly canTranslate: boolean }
  | { readonly phase: "failed"; readonly source: string | null }
  | { readonly phase: "same"; readonly language: string }
  | { readonly phase: "unsupported"; readonly language: string }
  /** `percent` is `null` until the browser reports progress, if ever. */
  | { readonly phase: "downloading"; readonly language: string; readonly percent: number | null }
  | { readonly phase: "waiting-wifi"; readonly language: string }
  | { readonly phase: "pack-failed"; readonly language: string };

/** The transcript block of a voice or video message (UX 4.3). */
export type TranscriptView =
  | { readonly phase: "none" }
  /** Fetching the audio or installing a pack: the ring on `Aa`. */
  | { readonly phase: "loading" }
  /** A pack whose download reports no progress (`install()`). */
  | { readonly phase: "installing"; readonly language: string }
  | { readonly phase: "transcribing"; readonly partial: string }
  | {
      readonly phase: "shown" | "hidden";
      readonly text: string;
      readonly language: string;
      readonly complete: boolean;
    }
  | { readonly phase: "no-speech"; readonly language: string }
  | { readonly phase: "failed" }
  | { readonly phase: "unsupported"; readonly language: string }
  | { readonly phase: "unavailable-file" };

/** The UX 3.5 dialog. */
export interface PackRequest {
  readonly use: PackUse;
  readonly language: string;
  /** On a cellular link without permission: the button reads `Download on Wi-Fi`. */
  readonly wifiOnly: boolean;
  /** The system downloads this pack in its own settings (macOS translation):
   *  the button opens them and nothing is downloaded by Lakko. */
  readonly systemSettings: boolean;
  /** The language groups the download fetches and their size, where the
   *  engine downloads groups (Lakko's own, ADR-0100 section 4); `null` for
   *  an engine that fetches one language from its vendor. */
  readonly disclosure: PackDisclosure | null;
  /** `Download` - called from the person's press, which the browser needs. */
  readonly download: () => void;
  readonly dismiss: () => void;
}

/** The UX 3.4 picker. */
export interface PickerRequest {
  readonly use: PackUse;
  readonly current: string | null;
  /** The message's close candidates, listed first (`Not X?`, UX 3.4). */
  readonly candidates: readonly string[];
  readonly choose: (language: string) => void;
  readonly dismiss: () => void;
}

export interface ControllerDeps {
  readonly engines: Engines;
  readonly store: ResultStore;
  readonly rules: () => TranslationRules | null;
  readonly accountId: () => string | null;
  readonly interfaceLanguage: () => string;
  readonly settings: () => TranslationSettings;
  readonly recordPack: (language: string, use: PackUse) => void;
  /** Turn transcription off on this device for good (the microphone guard). */
  readonly blockTranscription?: () => void;
  readonly setTimer?: (fn: () => void, ms: number) => unknown;
}

type DetectionSlot = Detection | null | "pending";

export class TranslationController {
  private readonly deps: ControllerDeps;
  private readonly version = createSignal(0);
  private readonly packSignal = createSignal<PackRequest | null>(null);
  private readonly pickerSignal = createSignal<PickerRequest | null>(null);
  private readonly views = new Map<string, TextView>();
  private readonly restored = new Set<string>();
  private readonly explicitOriginal = new Set<string>();
  private readonly transcripts = new Map<string, TranscriptView>();
  private readonly detections = new Map<string, DetectionSlot>();
  private readonly engineStates = new Map<string, EngineState | "pending">();
  private readonly prefsCache = new Map<string, ConversationPrefs | "pending">();
  private readonly aborts = new Map<string, AbortController>();
  private readonly autoTried = new Set<string>();

  constructor(deps: ControllerDeps) {
    this.deps = deps;
  }

  private bump(): void {
    this.version[1]((v) => v + 1);
  }
  private track(): void {
    this.version[0]();
  }
  private timer(fn: () => void, ms: number): void {
    (this.deps.setTimer ?? ((f: () => void, m: number) => setTimeout(f, m)))(fn, ms);
  }

  // ----- engines and languages ---------------------------------------------

  get translationPresent(): boolean {
    return this.deps.engines.translation.present;
  }
  get transcriptionPresent(): boolean {
    return this.deps.engines.transcription.present && !this.deps.settings().transcriptionBlocked;
  }

  /**
   * Whether `Download packs on mobile data` governs anything here (UX
   * section 6): some engine downloads a pack itself, and this device can tell
   * a mobile connection from another. In the desktop app on macOS a
   * translation pack comes from System Settings and the webview reports no
   * connection type, so the row is not shown.
   */
  mobileDataMatters(): boolean {
    if (!connectionTypeKnown()) return false;
    const translation = this.translationPresent && this.deps.engines.translation.fetchesPacks !== false;
    return translation || this.transcriptionPresent;
  }

  /** The packs the operating system holds for these engines, where it can
   *  list them (`Engines.systemPacks`); `null` where it cannot. */
  async systemPacks(target: string = this.target()): Promise<SystemPacks | null> {
    const list = this.deps.engines.systemPacks;
    if (list === undefined) return null;
    return list.call(this.deps.engines, target).catch(() => null);
  }

  /** The language groups Lakko's own engine downloaded (UX 6, ADR-0100);
   *  empty where no engine keeps its own packs. */
  async ownPacks(): Promise<OwnPack[]> {
    return (await this.deps.engines.translation.ownPacks?.().catch(() => [])) ?? [];
  }

  /** `Remove` on one of {@link ownPacks}: the group is deleted, every pair it
   *  served reads `needs-pack` again. */
  async removeOwnPack(id: string): Promise<void> {
    await this.deps.engines.translation.removeOwnPack?.(id);
    this.engineStates.clear();
    this.bump();
  }

  /** The language translations are made into. */
  target(): string {
    return this.deps.settings().target ?? this.deps.interfaceLanguage();
  }

  /** The person's reading languages (UX section 6). */
  reading(): string[] {
    const ui = this.deps.interfaceLanguage();
    const out = [ui];
    for (const l of this.deps.settings().extraReading) if (!out.includes(l)) out.push(l);
    return out;
  }

  /** The engine's answer for a pair, asked once and cached (reactive). */
  translationState(source: string, target: string = this.target()): EngineState | null {
    this.track();
    const key = `t:${source}>${target}`;
    const known = this.engineStates.get(key);
    if (known === "pending") return null;
    if (known !== undefined) return known;
    this.engineStates.set(key, "pending");
    void this.deps.engines.translation.state(source, target).then((s) => {
      this.engineStates.set(key, s);
      this.bump();
    });
    return null;
  }

  /** The transcription engine's answer for a language (reactive). */
  transcriptionState(language: string): EngineState | null {
    this.track();
    const key = `s:${language}`;
    const known = this.engineStates.get(key);
    if (known === "pending") return null;
    if (known !== undefined) return known;
    this.engineStates.set(key, "pending");
    void this.deps.engines.transcription.state(language).then((s) => {
      this.engineStates.set(key, s);
      this.bump();
    });
    return null;
  }

  /** Forget cached engine answers, e.g. after a pack arrived. */
  private forgetState(key: string): void {
    this.engineStates.delete(key);
  }

  /** Whether the device can translate into the target from anything at all. */
  anyEngine(): boolean {
    if (!this.translationPresent) return false;
    const target = this.target();
    let pending = false;
    for (const l of CANDIDATE_LANGUAGES) {
      const s = this.translationState(l, target);
      if (s === "ready" || s === "needs-pack") return true;
      if (s === null) pending = true;
    }
    // Until the answers are in, the browser having a translator at all is the
    // best evidence; the row is re-decided when they arrive.
    return pending;
  }

  /** The languages a picker offers for `use` (reactive). */
  pickerLanguages(use: PackUse): string[] {
    const target = this.target();
    return CANDIDATE_LANGUAGES.filter((l) => {
      const s = use === "translation" ? this.translationState(l, target) : this.transcriptionState(l);
      return s === "ready" || s === "needs-pack";
    });
  }

  /** A message's detected language (reactive; detection runs once per version). */
  detected(ref: MessageRef, text: string): Detection | null {
    this.track();
    if (text.trim().length === 0) return null;
    const key = `${ref.conversation}|${ref.messageId}|${ref.lastOpId}`;
    const known = this.detections.get(key);
    if (known === "pending") return null;
    if (known !== undefined) return known;
    void this.detect(ref, text);
    return null;
  }

  /** The detection, waiting for one in flight. */
  private detect(ref: MessageRef, text: string): Promise<Detection | null> {
    const key = `${ref.conversation}|${ref.messageId}|${ref.lastOpId}`;
    const known = this.detections.get(key);
    if (known !== undefined && known !== "pending") return Promise.resolve(known);
    const inFlight = this.detectionsInFlight.get(key);
    if (inFlight !== undefined) return inFlight;
    this.detections.set(key, "pending");
    const p = this.deps.engines.detection.detect(text).then(
      (d) => d,
      () => null,
    ).then((d) => {
      this.detections.set(key, d);
      this.detectionsInFlight.delete(key);
      this.rememberConfident(ref, d);
      this.bump();
      return d;
    });
    this.detectionsInFlight.set(key, p);
    return p;
  }

  private readonly detectionsInFlight = new Map<string, Promise<Detection | null>>();

  /**
   * The language a message is taken to be in (ADR-0082 note of 2026-11-15):
   * its detection, unless that is unsure and the conversation's languages
   * name one of its close candidates: a picked one, else the most detected
   * one (shared rule `conversationDetection`, independent of the order
   * messages were detected in). Used for the line up front, the `Translate`
   * row and the source a translation starts from.
   */
  effectiveDetection(ref: MessageRef, text: string): Detection | null {
    const d = this.detected(ref, text);
    const r = this.deps.rules();
    if (d === null || r === null) return d;
    return conversationDetectionFor(r, d, this.prefs(ref.conversation).languages);
  }

  /** A confident detection is recorded for its message, once per message id. */
  private rememberConfident(ref: MessageRef, d: Detection | null): void {
    const r = this.deps.rules();
    if (d === null || r === null || d.permille < r.translationDetectionConfidentPermille()) return;
    const known = this.prefsNow(ref.conversation).languages.detections.find((x) => x.messageId === ref.messageId);
    if (known !== undefined && r.sameLanguage(known.tag, d.tag)) return;
    void this.updatePrefs(ref.conversation, (p) => ({
      ...p,
      languages: withLanguageDetection(r, p.languages, ref.messageId, d.tag),
    }));
  }

  // ----- the menu row --------------------------------------------------------

  /** UX 3.1 — the `Translate` row (shared rule). */
  offer(ref: MessageRef, text: string, isOwn: boolean): Offer {
    const r = this.deps.rules();
    if (r === null || !this.translationPresent) return "none";
    const detected = isOwn || text.length === 0 ? null : this.effectiveDetection(ref, text);
    const engineForDetected =
      detected === null ? "unavailable" : (this.translationState(detected.tag) ?? "unavailable");
    return offerFor(r, {
      enabled: this.deps.settings().showTranslate,
      hasText: text.trim().length > 0,
      isOwn,
      detected,
      engineForDetected,
      anyEngine: this.anyEngine(),
      reading: this.reading(),
    });
  }

  /**
   * UX 3.2 — the language line shown before any translation. It names the
   * language `Translate` would start from (shared rule
   * `resolveTranslationSource`, the same call {@link sourceFor} makes), so
   * "hola, qué tal?" read as an unsure Catalan on a device that translates
   * Spanish shows `Spanish · Translate`. The menu row's rule then decides
   * whether that language is one to name (not own, has text, setting on, not
   * a reading language); asked with the engine fact set to "can translate" so
   * that an engine that cannot translate hides the button, not the line.
   */
  languageLine(ref: MessageRef, text: string, isOwn: boolean): TextView {
    const r = this.deps.rules();
    if (r === null || isOwn || text.trim().length === 0) return { phase: "none" };
    const res = this.resolution(ref, text);
    if (res === null || res.lineTag === null) return { phase: "none" };
    const named = offerFor(r, {
      enabled: this.deps.settings().showTranslate,
      hasText: true,
      isOwn,
      detected: { tag: res.lineTag, permille: res.linePermille },
      engineForDetected: "ready",
      anyEngine: true,
      reading: this.reading(),
    });
    if (named === "none") return { phase: "none" };
    return { phase: "detected", source: res.lineTag, canTranslate: res.lineCanTranslate };
  }

  /**
   * The line's language and `Translate`'s source for a message, from its
   * detection, the conversation's languages and the engine's
   * answers for every candidate (reactive). `null` until the detection and
   * those answers are in: the line would otherwise change a moment later.
   */
  private resolution(ref: MessageRef, text: string): SourceResolution | null {
    const r = this.deps.rules();
    if (r === null) return null;
    const raw = this.detected(ref, text);
    if (raw === null) return null;
    const languages = this.prefs(ref.conversation).languages;
    const usual = usualLanguage(r, languages);
    const target = this.target();
    const translatable: string[] = [];
    if (this.translationPresent) {
      // Every answer is asked at once, so the line waits one round, not one
      // per candidate.
      let pending = false;
      for (const tag of this.askedTags(r, raw, usual, target)) {
        const state = this.translationState(tag, target);
        if (state === null) pending = true;
        else if (state !== "unavailable") translatable.push(tag);
      }
      if (pending) return null;
    }
    return resolveSourceFor(r, raw, languages, target, translatable);
  }

  /** The tags whose engine answer the shared rule needs: every candidate and
   *  the conversation's usual language, less the language translated into. */
  private askedTags(r: TranslationRules, raw: Detection | null, usual: string | null, target: string): string[] {
    const out: string[] = [];
    for (const tag of [...candidateTags(raw), ...(usual === null ? [] : [usual])]) {
      if (r.sameLanguage(tag, target) || out.some((t) => r.sameLanguage(t, tag))) continue;
      out.push(tag);
    }
    return out;
  }

  /** The quiet line under a message's text: the translation's own line when
   *  there is one (manual or automatic), else the language line up front. */
  textLine(ref: MessageRef, text: string, isOwn: boolean): TextView {
    const v = this.textView("text", ref);
    return v.phase === "none" ? this.languageLine(ref, text, isOwn) : v;
  }

  // ----- translating a message ----------------------------------------------

  /** The quiet line for one message (reactive). Restores a stored result once. */
  textView(slot: TextSlot, ref: MessageRef): TextView {
    this.track();
    const key = messageKey(slot, ref.conversation, ref.messageId);
    const view = this.views.get(key);
    if (!this.restored.has(key)) {
      this.restored.add(key);
      void this.restore(slot, ref, key);
    }
    if (view === undefined) return { phase: "none" };
    // An edit since the translation was made: the line goes, the original shows.
    if (view.phase === "translated" && this.viewOpId.get(key) !== ref.lastOpId) return { phase: "none" };
    return view;
  }

  /** The op id a translated view was made for. */
  private readonly viewOpId = new Map<string, number>();

  private setView(key: string, view: TextView, lastOpId?: number): void {
    this.views.set(key, view);
    if (lastOpId !== undefined) this.viewOpId.set(key, lastOpId);
    this.bump();
  }

  private async restore(slot: TextSlot, ref: MessageRef, key: string): Promise<void> {
    const account = this.deps.accountId();
    const r = this.deps.rules();
    if (account === null || r === null) return;
    const stored = await this.deps.store.result(account, slot, ref.conversation, ref.messageId);
    if (stored === null || stored.kind !== "translation") return;
    if (this.views.has(key) || this.explicitOriginal.has(key)) return;
    if (!isCurrent(r, stored.lastOpId, ref.lastOpId, stored.target, this.target())) return;
    this.setView(key, { phase: "translated", source: stored.source, text: stored.text }, ref.lastOpId);
  }

  /** `Translate` / `Show original` from the menu or the quiet line. */
  toggle(slot: TextSlot, ref: MessageRef, text: string): void {
    const view = this.textView(slot, ref);
    if (view.phase === "translated") this.showOriginal(slot, ref);
    else this.translate(slot, ref, text);
  }

  showOriginal(slot: TextSlot, ref: MessageRef): void {
    const key = messageKey(slot, ref.conversation, ref.messageId);
    const view = this.views.get(key);
    this.explicitOriginal.add(key);
    this.aborts.get(key)?.abort();
    if (view !== undefined && "source" in view && view.source !== null) {
      this.setView(key, { phase: "original", source: view.source });
    } else {
      this.setView(key, { phase: "none" });
    }
  }

  /**
   * Translate one message. `source` is a language the person chose in the
   * picker; without it the detected language is used, even a guess below the
   * confidence line (the quiet line then offers `Not Spanish?`), then the
   * language chosen for the conversation, and the picker opens first only
   * when there is neither (UX 3.1 / 3.4).
   */
  translate(slot: TextSlot, ref: MessageRef, text: string, source?: string): void {
    const key = messageKey(slot, ref.conversation, ref.messageId);
    this.explicitOriginal.delete(key);
    void (async () => {
      const from = source !== undefined ? { tag: source, guess: null } : await this.sourceFor(ref, text);
      if (from.tag === null) {
        this.openSourcePicker(slot, ref, text, from.guess);
        return;
      }
      await this.run(slot, ref, text, from.tag, key);
    })();
  }

  /**
   * The source a translation starts from: the shared rule
   * `resolveTranslationSource`, the same one the line up front reads
   * ({@link resolution}), over the engine's answers asked now. In order: a
   * confident detection; the conversation's language (picked, else most
   * detected) when the detection is unsure and it is one of the close
   * candidates; the guess when usable; the first other close candidate the device can translate from;
   * the conversation's usual language; otherwise the picker, with the guess
   * marked.
   */
  private async sourceFor(
    ref: MessageRef,
    text: string,
  ): Promise<{ readonly tag: string | null; readonly guess: string | null }> {
    const raw = await this.detect(ref, text);
    const r = this.deps.rules();
    if (r === null) return { tag: raw?.tag ?? null, guess: null };
    const languages = this.prefsNow(ref.conversation).languages;
    const target = this.target();
    const translatable: string[] = [];
    for (const tag of this.askedTags(r, raw, usualLanguage(r, languages), target)) {
      if ((await this.engineState(tag, target)) !== "unavailable") translatable.push(tag);
    }
    const res = resolveSourceFor(r, raw, languages, target, translatable);
    return { tag: res.translateSource, guess: res.guess };
  }

  /** The engine's answer for a pair, asked now and cached. */
  private async engineState(source: string, target: string): Promise<EngineState> {
    const state = await this.deps.engines.translation.state(source, target);
    this.engineStates.set(`t:${source}>${target}`, state);
    return state;
  }

  private async run(slot: TextSlot, ref: MessageRef, text: string, source: string, key: string): Promise<void> {
    const r = this.deps.rules();
    const account = this.deps.accountId();
    const target = this.target();
    if (r === null) return;
    if (r.sameLanguage(source, target)) {
      this.setView(key, { phase: "same", language: target });
      this.timer(() => {
        if (this.views.get(key)?.phase === "same") this.setView(key, { phase: "none" });
      }, SAME_LANGUAGE_LINE_MS);
      return;
    }
    if (account !== null) {
      const stored = await this.deps.store.result(account, slot, ref.conversation, ref.messageId);
      if (
        stored?.kind === "translation" &&
        r.sameLanguage(stored.source, source) &&
        isCurrent(r, stored.lastOpId, ref.lastOpId, stored.target, target)
      ) {
        this.setView(key, { phase: "translated", source: stored.source, text: stored.text }, ref.lastOpId);
        return;
      }
    }
    const state = await this.deps.engines.translation.state(source, target);
    this.engineStates.set(`t:${source}>${target}`, state);
    if (state === "unavailable") {
      this.setView(key, { phase: "unsupported", language: source });
      return;
    }
    if (state === "needs-pack") {
      this.setView(key, { phase: "none" });
      if (this.packFetch(source, target) === "system-settings") {
        this.askForSystemPack(source, () => this.forgetState(`t:${source}>${target}`));
        return;
      }
      this.askForPack(
        "translation",
        source,
        (waited) => {
          // A wait for Wi-Fi the person cancelled from the line starts nothing.
          if (waited && this.views.get(key)?.phase !== "waiting-wifi") return;
          return this.execute(slot, ref, text, source, key, true);
        },
        () => undefined,
        () => this.setView(key, { phase: "waiting-wifi", language: source }),
        { source, target },
      );
      return;
    }
    await this.execute(slot, ref, text, source, key, false);
  }

  private async execute(
    slot: TextSlot,
    ref: MessageRef,
    text: string,
    source: string,
    key: string,
    downloading: boolean,
  ): Promise<void> {
    const target = this.target();
    const abort = new AbortController();
    this.aborts.set(key, abort);
    this.setView(key, downloading ? { phase: "downloading", language: source, percent: null } : { phase: "translating", source });
    try {
      const translated = await this.deps.engines.translation.translate(source, target, text, {
        signal: abort.signal,
        onProgress: (fraction) => {
          if (abort.signal.aborted) return;
          const percent = Math.max(0, Math.min(100, Math.round(fraction * 100)));
          if (percent >= 100) this.setView(key, { phase: "translating", source });
          else this.setView(key, { phase: "downloading", language: source, percent });
        },
      });
      if (abort.signal.aborted || this.explicitOriginal.has(key)) return;
      if (downloading) {
        this.recordTranslationPack(source, target);
        this.forgetState(`t:${source}>${target}`);
      }
      this.setView(key, { phase: "translated", source, text: translated }, ref.lastOpId);
      const account = this.deps.accountId();
      if (account !== null) {
        const value: StoredTranslation = { kind: "translation", lastOpId: ref.lastOpId, source, target, text: translated };
        await this.deps.store.saveResult(account, slot, ref.conversation, ref.messageId, value).catch((err: unknown) => {
          console.warn("[translation] store write failed", err instanceof Error ? err.name : "unknown");
        });
      }
    } catch (err) {
      if (abort.signal.aborted) {
        this.setView(key, { phase: "none" });
        return;
      }
      console.warn("[translation] engine failed", err instanceof Error ? err.name : "unknown");
      this.setView(key, downloading ? { phase: "pack-failed", language: source } : { phase: "failed", source });
    } finally {
      if (this.aborts.get(key) === abort) this.aborts.delete(key);
    }
  }

  /** `Cancel` on the downloading line. */
  cancel(slot: TextSlot, ref: MessageRef): void {
    const key = messageKey(slot, ref.conversation, ref.messageId);
    this.aborts.get(key)?.abort();
    this.setView(key, { phase: "none" });
  }

  /** `Not Spanish?`, or a message whose language nothing could tell.
   *  `guess` marks a detector's unusable guess in the list. */
  openSourcePicker(slot: TextSlot, ref: MessageRef, text: string, guess: string | null = null): void {
    const view = this.views.get(messageKey(slot, ref.conversation, ref.messageId));
    const current = view !== undefined && "source" in view ? view.source : guess;
    const known = this.detections.get(`${ref.conversation}|${ref.messageId}|${ref.lastOpId}`);
    const candidates = candidateTags(known === undefined || known === "pending" ? null : known);
    this.pickerSignal[1]({
      use: "translation",
      current,
      candidates,
      choose: (language) => {
        this.pickerSignal[1](null);
        const r = this.deps.rules();
        if (r !== null) {
          void this.updatePrefs(ref.conversation, (p) => ({
            ...p,
            languages: withLanguagePick(r, p.languages, language, Date.now()),
          }));
        }
        this.translate(slot, ref, text, language);
      },
      dismiss: () => this.pickerSignal[1](null),
    });
  }

  // ----- packs -----------------------------------------------------------------

  private askForPack(
    use: PackUse,
    language: string,
    start: (waited: boolean) => Promise<void> | void,
    onDismiss: () => void,
    onWaiting: () => void = () => undefined,
    pair: { readonly source: string; readonly target: string } | null = null,
  ): void {
    const ask = pair === null || use !== "translation"
      ? Promise.resolve(null)
      : (this.deps.engines.translation.packDisclosure?.(pair.source, pair.target) ?? Promise.resolve(null)).catch(() => null);
    void ask.then((disclosure) => this.showPackDialog(use, language, start, onDismiss, onWaiting, disclosure));
  }

  private showPackDialog(
    use: PackUse,
    language: string,
    start: (waited: boolean) => Promise<void> | void,
    onDismiss: () => void,
    onWaiting: () => void,
    disclosure: PackDisclosure | null,
  ): void {
    const wifiOnly = onCellular() && !this.deps.settings().mobileData;
    this.packSignal[1]({
      use,
      language,
      wifiOnly,
      systemSettings: false,
      disclosure,
      download: () => {
        this.packSignal[1](null);
        if (wifiOnly) {
          onWaiting();
          this.waitForWifi(() => void start(true));
          return;
        }
        void start(false);
      },
      dismiss: () => {
        this.packSignal[1](null);
        onDismiss();
      },
    });
  }

  /** Record a downloaded translation pack in the settings, unless the
   *  engine that owns the pair lists its own packs (Lakko's, UX 6). */
  private recordTranslationPack(source: string, target: string): void {
    if (this.deps.engines.translation.keepsOwnPacks?.(source, target) === true) return;
    this.deps.recordPack(source, "translation");
  }

  /** Who fetches the pack of a pair (`engines.ts`, `PackFetch`). */
  private packFetch(source: string, target: string): "engine" | "system-settings" {
    return this.deps.engines.translation.packFetch?.(source, target) ?? "engine";
  }

  /**
   * UX 3.5 where the system downloads translation packs in its own settings
   * (macOS): the dialog's button opens them. Lakko downloads nothing and
   * waits for nothing: the line goes back to `Spanish · Translate`, and the
   * next `Translate` asks the system again (the rule of ADR-0082 note of
   * 2026-11-15 for a pack the device fetches later).
   */
  private askForSystemPack(language: string, onOpened: () => void): void {
    this.packSignal[1]({
      use: "translation",
      language,
      wifiOnly: false,
      systemSettings: true,
      disclosure: null,
      download: () => {
        this.packSignal[1](null);
        onOpened();
        this.bump();
        void this.deps.engines.translation.openPackSettings?.().catch((err: unknown) => {
          console.warn("[translation] opening the system settings failed", err instanceof Error ? err.name : "unknown");
        });
      },
      dismiss: () => this.packSignal[1](null),
    });
  }

  private waitForWifi(start: () => void): void {
    const c = (globalThis.navigator as { connection?: EventTarget } | undefined)?.connection;
    if (c === undefined) {
      start();
      return;
    }
    const onChange = (): void => {
      if (onCellular()) return;
      c.removeEventListener("change", onChange);
      start();
    };
    c.addEventListener("change", onChange);
  }

  /** The dialog on screen, if any (reactive). */
  packRequest(): PackRequest | null {
    return this.packSignal[0]();
  }
  /** The picker on screen, if any (reactive). */
  pickerRequest(): PickerRequest | null {
    return this.pickerSignal[0]();
  }

  // ----- automatic translation -------------------------------------------------

  /**
   * UX 3.7 — translate an incoming message as it is shown, when the
   * conversation asks for it and the language is ready now. A message the
   * person put back to its original stays so; a language that needs a pack is
   * left for the person to choose.
   */
  autoTranslate(ref: MessageRef, text: string, isOwn: boolean): void {
    if (isOwn || text.trim().length === 0) return;
    const r = this.deps.rules();
    if (r === null || !this.prefs(ref.conversation).autoTranslate) return;
    const key = messageKey("text", ref.conversation, ref.messageId);
    if (this.explicitOriginal.has(key) || this.autoTried.has(`${key}|${ref.lastOpId}`)) return;
    const d = this.detected(ref, text);
    if (d === null || d.permille < r.translationDetectionConfidentPermille()) return;
    if (r.readsLanguage(this.reading(), d.tag)) return;
    if (this.translationState(d.tag) !== "ready") return;
    const view = this.views.get(key);
    if (view !== undefined && view.phase !== "none") return;
    this.autoTried.add(`${key}|${ref.lastOpId}`);
    void this.run("text", ref, text, d.tag, key);
  }

  // ----- the conversation bar and per-conversation choices --------------------

  private prefsNow(conversation: string): ConversationPrefs {
    const p = this.prefsCache.get(conversation);
    return p === undefined || p === "pending" ? DEFAULT_PREFS : p;
  }

  /** The conversation's choices (reactive; loaded on first read). */
  prefs(conversation: string): ConversationPrefs {
    this.track();
    const known = this.prefsCache.get(conversation);
    if (known === undefined) {
      const account = this.deps.accountId();
      if (account !== null) {
        this.prefsCache.set(conversation, "pending");
        void this.deps.store.prefs(account, conversation).then((p) => {
          if (this.prefsCache.get(conversation) === "pending") this.prefsCache.set(conversation, p);
          this.bump();
        });
      }
    }
    return this.prefsNow(conversation);
  }

  private async updatePrefs(conversation: string, change: (p: ConversationPrefs) => ConversationPrefs): Promise<void> {
    const next = change(this.prefsNow(conversation));
    this.prefsCache.set(conversation, next);
    this.bump();
    const account = this.deps.accountId();
    if (account === null) return;
    await this.deps.store.savePrefs(account, conversation, next).catch((err: unknown) => {
      console.warn("[translation] prefs write failed", err instanceof Error ? err.name : "unknown");
    });
  }

  /** `Translate automatically` (UX 3.7). Turning it off shows every original. */
  setAutoTranslate(conversation: string, on: boolean): void {
    void this.updatePrefs(conversation, (p) => ({ ...p, autoTranslate: on }));
    if (!on) {
      for (const [key, view] of this.views) {
        if (key.startsWith(`text|${conversation}|`) && view.phase === "translated") {
          this.views.set(key, { phase: "original", source: view.source });
        }
      }
      for (const k of [...this.autoTried]) if (k.startsWith(`text|${conversation}|`)) this.autoTried.delete(k);
      this.bump();
    }
  }

  /**
   * `Translate` on the bar (UX 3.6): automatic translation on, and when the
   * bar's language needs a pack, the 3.5 dialog first. Nothing downloads
   * unless the person presses `Download` there; the messages already on
   * screen are translated once the pack is in.
   */
  enableAutoFromBar(conversation: string, language: string): void {
    this.setAutoTranslate(conversation, true);
    if (this.translationState(language) !== "needs-pack") return;
    const target = this.target();
    if (this.packFetch(language, target) === "system-settings") {
      this.askForSystemPack(language, () => {
        this.forgetState(`t:${language}>${target}`);
        for (const k of [...this.autoTried]) if (k.startsWith(`text|${conversation}|`)) this.autoTried.delete(k);
      });
      return;
    }
    this.askForPack(
      "translation",
      language,
      async () => {
        try {
          // Creating the translator is what fetches the pack.
          await this.deps.engines.translation.translate(language, target, "");
          this.recordTranslationPack(language, target);
        } catch (err) {
          console.warn("[translation] pack download failed", err instanceof Error ? err.name : "unknown");
        }
        this.forgetState(`t:${language}>${target}`);
        for (const k of [...this.autoTried]) if (k.startsWith(`text|${conversation}|`)) this.autoTried.delete(k);
        this.bump();
      },
      () => undefined,
      () => undefined,
      { source: language, target },
    );
  }

  /** `Not now` / close on the bar: it does not come back for this language. */
  dismissBar(conversation: string, language: string): void {
    void this.updatePrefs(conversation, (p) =>
      p.dismissed.includes(language) ? p : { ...p, dismissed: [...p.dismissed, language] },
    );
  }

  /** UX 3.6 — the language the bar names, or `null` (reactive). */
  barLanguage(conversation: string, recent: readonly Detection[]): string | null {
    const r = this.deps.rules();
    if (r === null || !this.translationPresent) return null;
    const prefs = this.prefs(conversation);
    const language = barLanguageFor(r, {
      enabled: this.deps.settings().showTranslate,
      autoTranslateOn: prefs.autoTranslate,
      recent,
      dismissed: prefs.dismissed,
      reading: this.reading(),
    });
    if (language === null) return null;
    const state = this.translationState(language);
    return state === "ready" || state === "needs-pack" ? language : null;
  }

  // ----- transcripts -------------------------------------------------------------

  /** The language a transcript is made in (UX 4.4): the conversation's last
   *  choice, else the interface language. Web has no audio language detector. */
  transcriptLanguage(conversation: string): string {
    return this.prefs(conversation).transcriptionLanguage ?? this.deps.interfaceLanguage();
  }

  /** UX 4.1 — whether the `Aa` button is offered (shared rule). */
  transcriptOffered(conversation: string, hasAudio: boolean): boolean {
    const r = this.deps.rules();
    if (r === null || !this.transcriptionPresent) return false;
    const state = this.transcriptionState(this.transcriptLanguage(conversation)) ?? "unavailable";
    return transcriptOfferedFor(r, this.deps.settings().showTranscript, hasAudio, state);
  }

  transcriptView(ref: MessageRef): TranscriptView {
    this.track();
    const key = messageKey("transcript", ref.conversation, ref.messageId);
    if (!this.restored.has(key)) {
      this.restored.add(key);
      void this.restoreTranscript(ref, key);
    }
    return this.transcripts.get(key) ?? { phase: "none" };
  }

  private async restoreTranscript(ref: MessageRef, key: string): Promise<void> {
    const account = this.deps.accountId();
    const r = this.deps.rules();
    if (account === null || r === null) return;
    const stored = await this.deps.store.result(account, "transcript", ref.conversation, ref.messageId);
    if (stored?.kind !== "transcript" || this.transcripts.has(key)) return;
    if (!isCurrent(r, stored.lastOpId, ref.lastOpId, stored.language, stored.language)) return;
    this.transcripts.set(key, { phase: "hidden", text: stored.text, language: stored.language, complete: stored.complete });
    this.bump();
  }

  private setTranscript(key: string, view: TranscriptView): void {
    this.transcripts.set(key, view);
    this.bump();
  }

  /** The `Aa` button. `loadAudio` fetches and decrypts the file as playing does. */
  toggleTranscript(ref: MessageRef, loadAudio: () => Promise<Blob | null>): void {
    const key = messageKey("transcript", ref.conversation, ref.messageId);
    const view = this.transcriptView(ref);
    switch (view.phase) {
      case "shown":
        this.setTranscript(key, { ...view, phase: "hidden" });
        return;
      case "hidden":
        this.setTranscript(key, { ...view, phase: "shown" });
        return;
      case "loading":
      case "installing":
      case "transcribing":
        return;
      case "no-speech":
        this.setTranscript(key, { phase: "none" });
        return;
      default:
        void this.transcribe(ref, loadAudio, this.transcriptLanguage(ref.conversation));
    }
  }

  /** `Cancel` next to a pack download whose progress is not reported. */
  cancelTranscriptInstall(ref: MessageRef): void {
    const key = messageKey("transcript", ref.conversation, ref.messageId);
    if (this.transcripts.get(key)?.phase === "installing") this.setTranscript(key, { phase: "none" });
  }

  hideTranscript(ref: MessageRef): void {
    const key = messageKey("transcript", ref.conversation, ref.messageId);
    const view = this.transcripts.get(key);
    if (view?.phase === "shown") this.setTranscript(key, { ...view, phase: "hidden" });
  }

  /** `Not Spanish?` under a transcript. */
  openTranscriptPicker(ref: MessageRef, loadAudio: () => Promise<Blob | null>): void {
    const view = this.transcripts.get(messageKey("transcript", ref.conversation, ref.messageId));
    const current = view !== undefined && "language" in view ? view.language : null;
    this.pickerSignal[1]({
      use: "transcription",
      current,
      candidates: [],
      choose: (language) => {
        this.pickerSignal[1](null);
        void this.updatePrefs(ref.conversation, (p) => ({ ...p, transcriptionLanguage: language }));
        void this.transcribe(ref, loadAudio, language);
      },
      dismiss: () => this.pickerSignal[1](null),
    });
  }

  private async transcribe(ref: MessageRef, loadAudio: () => Promise<Blob | null>, language: string): Promise<void> {
    const key = messageKey("transcript", ref.conversation, ref.messageId);
    const engine = this.deps.engines.transcription;
    const state = await engine.state(language);
    this.engineStates.set(`s:${language}`, state);
    if (state === "unavailable") {
      this.setTranscript(key, { phase: "unsupported", language });
      return;
    }
    if (state === "needs-pack") {
      this.askForPack(
        "transcription",
        language,
        async () => {
          this.setTranscript(key, { phase: "installing", language });
          const ok = await engine.install(language);
          this.forgetState(`s:${language}`);
          if (ok) this.deps.recordPack(language, "transcription");
          // `Cancel` on the line: the browser's download cannot be stopped,
          // but nothing more is done with it here.
          if (this.transcripts.get(key)?.phase !== "installing") return;
          if (!ok) {
            this.setTranscript(key, { phase: "failed" });
            return;
          }
          await this.runTranscription(ref, key, loadAudio, language);
        },
        () => undefined,
      );
      return;
    }
    await this.runTranscription(ref, key, loadAudio, language);
  }

  private async runTranscription(
    ref: MessageRef,
    key: string,
    loadAudio: () => Promise<Blob | null>,
    language: string,
  ): Promise<void> {
    this.setTranscript(key, { phase: "loading" });
    let audio: Blob | null;
    try {
      audio = await loadAudio();
    } catch {
      audio = null;
    }
    if (audio === null) {
      this.setTranscript(key, { phase: "unavailable-file" });
      return;
    }
    this.setTranscript(key, { phase: "transcribing", partial: "" });
    try {
      const outcome = await this.deps.engines.transcription.transcribe(audio, language, {
        onPartial: (partial) => {
          if (this.transcripts.get(key)?.phase === "transcribing") this.setTranscript(key, { phase: "transcribing", partial });
        },
      });
      if (outcome.text.trim().length === 0) {
        this.setTranscript(key, { phase: "no-speech", language });
        return;
      }
      this.setTranscript(key, { phase: "shown", text: outcome.text, language, complete: outcome.complete });
      const account = this.deps.accountId();
      if (account !== null) {
        const value: StoredTranscript = {
          kind: "transcript",
          lastOpId: ref.lastOpId,
          language,
          text: outcome.text,
          complete: outcome.complete,
        };
        await this.deps.store.saveResult(account, "transcript", ref.conversation, ref.messageId, value).catch((err: unknown) => {
          console.warn("[translation] store write failed", err instanceof Error ? err.name : "unknown");
        });
      }
    } catch (err) {
      const name = err instanceof Error ? err.name : "unknown";
      console.warn("[transcript] engine failed", name);
      if (name === "MicrophoneGuard") {
        // ADR-0082 section 2: a recognizer that may have heard the
        // microphone is not used again on this device.
        this.deps.blockTranscription?.();
        this.setTranscript(key, { phase: "none" });
        return;
      }
      this.setTranscript(key, { phase: "failed" });
    }
  }

  // ----- deletion -----------------------------------------------------------------

  /** A message was deleted for everyone: everything made from it goes. */
  forgetMessage(conversation: string, messageId: number): void {
    const slots: ResultSlot[] = ["text", "transcript", "transcript-translation"];
    for (const s of slots) {
      const key = messageKey(s, conversation, messageId);
      this.views.delete(key);
      this.transcripts.delete(key);
      this.aborts.get(key)?.abort();
    }
    this.bump();
    const account = this.deps.accountId();
    if (account !== null) void this.deps.store.forgetMessage(account, conversation, messageId).catch(() => undefined);
  }

  /** A conversation was deleted from this device: everything made from it
   *  goes, in memory and on disk. */
  async forgetConversation(conversation: string): Promise<void> {
    const mark = `|${conversation}|`;
    for (const map of [this.views, this.transcripts]) {
      for (const k of [...map.keys()]) if (k.includes(mark)) map.delete(k);
    }
    for (const set of [this.restored, this.explicitOriginal, this.autoTried]) {
      for (const k of [...set]) if (k.includes(mark)) set.delete(k);
    }
    for (const [k, a] of this.aborts) {
      if (k.includes(mark)) {
        a.abort();
        this.aborts.delete(k);
      }
    }
    this.prefsCache.delete(conversation);
    this.bump();
    const account = this.deps.accountId();
    if (account !== null) await this.deps.store.forgetConversation(account, conversation);
  }

  /** How many results this device holds for the account. */
  async count(): Promise<number> {
    const account = this.deps.accountId();
    return account === null ? 0 : this.deps.store.count(account);
  }

  /** Settings: delete every translation and transcript on this device. */
  async deleteAll(): Promise<void> {
    const account = this.deps.accountId();
    this.views.clear();
    this.transcripts.clear();
    this.restored.clear();
    this.bump();
    if (account !== null) await this.deps.store.deleteAll(account);
  }

  /** The account changed or was wiped: drop everything held in memory. */
  reset(): void {
    this.views.clear();
    this.transcripts.clear();
    this.restored.clear();
    this.explicitOriginal.clear();
    this.prefsCache.clear();
    this.detections.clear();
    this.autoTried.clear();
    for (const a of this.aborts.values()) a.abort();
    this.aborts.clear();
    this.packSignal[1](null);
    this.pickerSignal[1](null);
    this.bump();
  }
}

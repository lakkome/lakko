// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * One call session's camera background (ADR-0089, UX
 * `docs/ux/call-backgrounds.md`): which track the session publishes as its
 * camera, the raw camera or the processed one, and the rules around it.
 *
 * The session hands every camera track it captures to {@link prepare} /
 * {@link adopt} (first capture, camera turned on mid-call, device switch) and
 * publishes the track it gets back: the raw camera with no background
 * selected, the processed one otherwise. That track is what the session puts
 * in its local stream, so the self-view, the main camera sender, every copy
 * of ADR-0070 (cloned from it) and the one-to-one sender all carry it. A
 * shared screen never comes here.
 *
 * Rules the controller keeps:
 *
 * - **While a background is selected, the raw camera is never handed out.**
 *   The processed track carries only frames the background was applied to
 *   (`background-engine.ts`); while it starts, it carries nothing.
 * - **A change applies at once**: a different background is a new mode on the
 *   running stage; switching between none and a background swaps the
 *   published track through the session's `replaceCameraTrack` (same sender,
 *   same SFrame transform, no renegotiation).
 * - **A failure turns the camera off** (the host does it, as if the person
 *   pressed the button) and shows the line. The selection is never changed
 *   here: only the person decides to show the room. A failed background
 *   starts again when the person picks a background again, switches the
 *   camera, or turns the camera back on.
 * - **The controller owns the raw camera track** once it was adopted: it
 *   stops the previous one when a new one is adopted, and on dispose.
 */

import {
  backgroundChoice,
  choiceForCall,
  isBackground,
  onBackgroundChoiceChange,
  type BackgroundChoice,
} from "./background-choice";
import type { StageMode } from "./background-mode";
import { backgroundPhotos } from "./background-photos";
import { createBackgroundStage, type BackgroundStage, type StageFactory, type StageState } from "./background-stage";
import {
  publishCameraBackground,
  reportCameraBackgroundFailure,
  type CameraBackgroundState,
} from "./background-status";
import { wasmSync } from "../wasm";

export interface CameraBackgroundDeps {
  /** Names the call kind in the log line (`group`, `1x1`, `preview`). */
  readonly label: string;
  /**
   * Publish `track` as the camera: in the local stream, on the camera sender
   * and on every copy. Must NOT stop the track it replaces (this controller
   * owns the raw camera and the stages).
   */
  replaceCameraTrack(track: MediaStreamTrack): Promise<void>;
  /** Defaults to {@link createBackgroundStage}. */
  readonly stageFactory?: StageFactory;
  /** Defaults to {@link resolveStageMode}. */
  readonly resolveMode?: (choice: BackgroundChoice) => Promise<StageMode>;
  /** Defaults to the stored choice. */
  readonly initialChoice?: BackgroundChoice;
  /** Defaults to {@link onBackgroundChoiceChange}. */
  readonly subscribe?: (listener: (choice: BackgroundChoice) => void) => () => void;
  /** Whether this is the live call (writes `background-status.ts` and
   *  reports a failure to the host). Default `true`. */
  readonly live?: boolean;
  /** Hears every state change (the Settings sheet's own preview). */
  readonly onState?: (state: CameraBackgroundState, track: MediaStreamTrack | null) => void;
  /** Defaults to `console.info`. */
  readonly log?: (line: string) => void;
}

/** A captured camera not yet published: publish {@link track}, then
 *  {@link commit}; or {@link abort} when it could not be published. */
export interface PreparedCamera {
  readonly track: MediaStreamTrack;
  commit(): void;
  abort(): void;
}

function closeMode(mode: StageMode): void {
  if (mode.kind === "picture") mode.bitmap.close();
}

/** The built-in ids in strip order, from the shared rule. */
function builtinIds(): readonly string[] {
  try {
    return wasmSync().backgroundBuiltinIds();
  } catch {
    return [];
  }
}

/**
 * The default mode for a choice: a blur, a built-in by its index, or the own
 * photo decoded. A photo that is missing or cannot be read is blur, never the
 * camera; so is an unknown built-in.
 */
export async function resolveStageMode(choice: BackgroundChoice): Promise<StageMode> {
  switch (choice.kind) {
    case "none":
    case "blur":
      return { kind: "blur", strong: choice.kind === "blur" && choice.strong };
    case "builtin": {
      const index = builtinIds().indexOf(choice.id);
      return index < 0 ? { kind: "blur", strong: false } : { kind: "builtin", index };
    }
    case "photo": {
      let bitmap: ImageBitmap | null = null;
      try {
        const bytes = await backgroundPhotos()?.get(choice.id);
        if (bytes !== null && bytes !== undefined) bitmap = await createImageBitmap(new Blob([bytes.slice()]));
      } catch {
        bitmap = null;
      }
      const usable = choiceForCall(choice, bitmap !== null);
      if (usable.kind === "photo" && bitmap !== null) return { kind: "picture", bitmap };
      return { kind: "blur", strong: false };
    }
  }
}

export class CameraBackgroundController {
  private raw: MediaStreamTrack | null = null;
  private stage: BackgroundStage | null = null;
  private choice: BackgroundChoice;
  private enabled = true;
  private disposed = false;
  private queue: Promise<void> = Promise.resolve();
  private readonly stageStates = new WeakMap<BackgroundStage, StageState>();
  private readonly failedReported = new WeakSet<BackgroundStage>();
  private readonly modeTokens = new WeakMap<BackgroundStage, number>();
  private readonly factory: StageFactory;
  private readonly resolveMode: (choice: BackgroundChoice) => Promise<StageMode>;
  private readonly unsubscribe: () => void;
  private readonly log: (line: string) => void;
  private readonly live: boolean;

  constructor(private readonly deps: CameraBackgroundDeps) {
    this.choice = deps.initialChoice ?? backgroundChoice();
    this.factory = deps.stageFactory ?? createBackgroundStage;
    this.resolveMode = deps.resolveMode ?? resolveStageMode;
    this.live = deps.live ?? true;
    // eslint-disable-next-line no-console
    this.log = deps.log ?? ((line: string): void => console.info(line));
    this.unsubscribe = (deps.subscribe ?? onBackgroundChoiceChange)((choice) => {
      this.onChoice(choice);
    });
  }

  /** The track published now (processed or raw), or `null` with no camera. */
  get track(): MediaStreamTrack | null {
    return this.stage?.track ?? this.raw;
  }

  /** The state the call screen shows. */
  get state(): CameraBackgroundState {
    if (this.stage === null) return "off";
    return this.stageStates.get(this.stage) ?? "starting";
  }

  /**
   * `raw` was just captured. Returns the track to publish instead of it,
   * unpublished: the session puts it on its sender and stream, then commits
   * (the previous camera and its stage are let go) or aborts (the new ones
   * are). With no background selected the track is `raw` itself.
   */
  prepare(raw: MediaStreamTrack): PreparedCamera {
    const enabled = raw.enabled;
    if (this.disposed || !isBackground(this.choice)) {
      return {
        track: raw,
        commit: (): void => this.commit(raw, null, enabled),
        abort: (): void => raw.stop(),
      };
    }
    const stage = this.makeStage(raw, enabled, this.choice);
    return {
      track: stage.track,
      commit: (): void => this.commit(raw, stage, enabled),
      abort: (): void => {
        stage.dispose();
        raw.stop();
      },
    };
  }

  /** {@link prepare} and commit at once, for a capture whose track is
   *  published in the same turn. */
  adopt(raw: MediaStreamTrack): MediaStreamTrack {
    const prepared = this.prepare(raw);
    prepared.commit();
    return prepared.track;
  }

  /** The person turned the camera off or on. Mirrors it onto the raw camera
   *  (so the camera itself stops sending) and the stage; turning it on starts
   *  a failed background again. */
  setCameraEnabled(on: boolean): void {
    if (this.disposed) return;
    this.enabled = on;
    if (this.raw !== null) this.raw.enabled = on;
    const stage = this.stage;
    if (stage !== null) {
      stage.track.enabled = on;
      stage.setEnabled(on);
      if (on && this.stageStates.get(stage) === "failed") {
        this.enqueue(() => this.rebuild());
      }
    }
  }

  /** Lets everything go: the stage, the raw camera, the subscription. */
  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.unsubscribe();
    this.stage?.dispose();
    this.stage = null;
    this.raw?.stop();
    this.raw = null;
    this.publish();
  }

  /** Resolves once every queued change has run (tests). */
  settled(): Promise<void> {
    return this.queue;
  }

  // -------------------------------------------------------------------------

  private commit(raw: MediaStreamTrack, stage: BackgroundStage | null, enabled: boolean): void {
    if (this.disposed) {
      stage?.dispose();
      raw.stop();
      return;
    }
    const oldRaw = this.raw;
    const oldStage = this.stage;
    this.raw = raw;
    this.stage = stage;
    this.enabled = enabled;
    if (oldStage !== null && oldStage !== stage) oldStage.dispose();
    if (oldRaw !== null && oldRaw !== raw) oldRaw.stop();
    this.publish();
    if (stage !== null && this.stageStates.get(stage) === "failed") this.reportFailure(stage);
  }

  private makeStage(raw: MediaStreamTrack, enabled: boolean, choice: BackgroundChoice): BackgroundStage {
    const holder: { stage: BackgroundStage | null; early: StageState | null } = { stage: null, early: null };
    const stage = this.factory(raw, (state) => {
      if (holder.stage === null) {
        holder.early = state;
        return;
      }
      this.onStageState(holder.stage, state);
    });
    holder.stage = stage;
    this.stageStates.set(stage, holder.early ?? "starting");
    stage.track.enabled = enabled;
    stage.setEnabled(enabled);
    this.loadMode(stage, choice);
    return stage;
  }

  private onStageState(stage: BackgroundStage, state: StageState): void {
    const before = this.stageStates.get(stage);
    if (before === "failed") return;
    this.stageStates.set(stage, state);
    if (stage !== this.stage) return;
    if (state !== before) this.log(`[background] ${this.deps.label}: ${state}`);
    this.publish();
    if (state === "failed") this.reportFailure(stage);
  }

  private reportFailure(stage: BackgroundStage): void {
    if (this.failedReported.has(stage)) return;
    this.failedReported.add(stage);
    if (this.live) reportCameraBackgroundFailure();
  }

  private loadMode(stage: BackgroundStage, choice: BackgroundChoice): void {
    const token = (this.modeTokens.get(stage) ?? 0) + 1;
    this.modeTokens.set(stage, token);
    void this.resolveMode(choice).then(
      (mode) => {
        if (this.modeTokens.get(stage) !== token) {
          closeMode(mode);
          return;
        }
        // A disposed stage closes the mode itself.
        stage.setMode(mode);
      },
      () => {
        if (this.modeTokens.get(stage) === token) stage.setMode({ kind: "blur", strong: false });
      },
    );
  }

  private onChoice(choice: BackgroundChoice): void {
    if (this.disposed) return;
    this.choice = choice;
    this.enqueue(async () => {
      if (this.raw === null || this.disposed) return;
      const stage = this.stage;
      if (!isBackground(choice)) {
        if (stage === null) return;
        await this.publishTrack(this.raw, () => {
          stage.dispose();
          if (this.stage === stage) this.stage = null;
        });
        return;
      }
      if (stage !== null && this.stageStates.get(stage) !== "failed") {
        this.loadMode(stage, choice);
        return;
      }
      await this.rebuild();
    });
  }

  /** A new stage on the current camera, published in place of the current
   *  track (a failed background starting again, or none -> a background). */
  private async rebuild(): Promise<void> {
    const raw = this.raw;
    if (raw === null || this.disposed || !isBackground(this.choice)) return;
    const stage = this.makeStage(raw, this.enabled, this.choice);
    const old = this.stage;
    const ok = await this.publishTrack(stage.track, () => {
      this.stage = stage;
      if (old !== null && old !== stage) old.dispose();
    });
    if (!ok) {
      stage.dispose();
      return;
    }
    if (this.stageStates.get(stage) === "failed") this.reportFailure(stage);
  }

  /**
   * Puts `track` where the camera goes, then runs `then` if the camera was
   * not replaced meanwhile (a device switch commits a new camera of its own;
   * the switch's track is then put back). Returns whether `then` ran.
   */
  private async publishTrack(track: MediaStreamTrack, then: () => void): Promise<boolean> {
    const raw = this.raw;
    try {
      await this.deps.replaceCameraTrack(track);
    } catch (err) {
      this.log(`[background] ${this.deps.label}: could not publish the camera (${err instanceof Error ? err.message : String(err)})`);
      return false;
    }
    if (this.disposed) return false;
    if (this.raw !== raw) {
      const current = this.track;
      if (current !== null) await this.deps.replaceCameraTrack(current).catch(() => undefined);
      return false;
    }
    then();
    this.publish();
    return true;
  }

  private enqueue(work: () => Promise<void>): void {
    this.queue = this.queue.then(work).catch((err: unknown) => {
      this.log(`[background] ${this.deps.label}: ${err instanceof Error ? err.message : String(err)}`);
    });
  }

  private publish(): void {
    const state = this.disposed ? "off" : this.state;
    const track = this.disposed ? null : this.track;
    if (this.live) publishCameraBackground(state, track);
    this.deps.onState?.(state, track);
  }
}

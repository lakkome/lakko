// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The collapsed call — one small window floating over the app
 * (`docs/ux/calls.md` section 4.5).
 *
 * A call that owns the whole screen owns the whole app. The first answer to
 * that was a bar across the top, which kept the call reachable and hid every
 * face. This window keeps the picture: the person the call screen would put
 * on the stage, your own camera as an inset while it sends, their name and
 * `+N`, the state or the running time, a lock when the call screen would show
 * its chip, and one round mute button. It sits above whatever screen is
 * showing and survives every navigation, because the call is owned by the
 * app-level host (`CallHostContext.tsx`) and not by a screen.
 *
 * ## What it deliberately does not carry
 *
 * **End.** Section 4.5.1: ending is the one irreversible control in a call,
 * and a small window a pointer crosses on the way to a chat is the wrong place
 * for it. Coming back costs one click.
 *
 * ## Where the decisions live
 *
 * Mostly not here. Whether the window is up and what its status line says are
 * the shared `call_bar` rules (`call-bar-model.ts`); who is in the picture is
 * the shared `call_stage` rule (`group-call-layout.ts` `stagePerson`); which
 * picture of them is asked of the SFU is `tile_video_quality`
 * (`video-wants.ts`); the mute button is `call-controls.ts` through
 * `callBarMuteControl`; the lock is `encryption-chip.ts`
 * `showsEncryptionChip`; the window's size and corners are
 * `call-window-model.ts`. The person is drawn with the call screen's own
 * pieces (`routes/Call.tsx`): `RemoteVideo`, `LocalVideo`, `PhotoOrInitials`,
 * `ScreenShareView`. This file is the markup, the drag, and the bookkeeping of
 * what it reports to the SFU.
 *
 * ## Web only: no tuck, no system window
 *
 * Section 4.5.2: web has no tuck (a pointer does not throw), and section
 * 4.5.4's system window is iOS and Android only. The window lives in the page.
 *
 * ## The desktop app: a window of its own
 *
 * Section 4.5.4a: in the desktop app the collapsed call is a separate window
 * of the operating system, and the in-app window is never drawn. The picture
 * is still built here, by the same code, but out of sight: its videos play
 * the call's streams with the layer choice this file makes, and
 * `desktop-call-window-relay.ts` draws the shown one into images and posts
 * them, with the window's view, to the host, which hands them to the second
 * window (`DesktopCallWindowPage.tsx`). Clicks there come back as host events.
 */

import {
  Show,
  createEffect,
  createMemo,
  createSignal,
  onCleanup,
  onMount,
  untrack,
  type JSX,
} from "solid-js";

import { Icon } from "../ui/icons";
import { bottomStripPx } from "../ui/bottom-strip";
import { initialsOf, toneOf } from "../ui/Avatar";
import { isDesktop } from "../platform";
import { onHostCallWindowAction } from "../platform/host";
import { avatarMediaFor } from "../profile/avatar-runtime";
import { joinList, t } from "../l10n";
import { callActions, callState, type Participant } from "../state/call-state";
import { uiRules } from "../chat/ui-rules";
import {
  LocalVideo,
  PhotoOrInitials,
  RemoteVideo,
  ScreenShareView,
  peerIdOfParticipant,
  withShareTile,
  type LayoutTile,
} from "../routes/Call";
import { showsEncryptionChip } from "./encryption-chip";
import { currentShareStream, screenShareState } from "./screen-share-store";
import { screenTileLabel } from "./screen-share-layout";
import { callPeerLabel, callSurfaceKind } from "./call-screen-model";
import { stagePerson } from "./group-call-layout";
import { cameraBackgroundState, ownCameraPreparing } from "./background-status";
import { lastDefined } from "./held";
import { SpeakingMonitor, type VoiceSource } from "./speaking-monitor";
import type { LevelReceiver } from "./audio-level";
import {
  askedQuality,
  physicalTileWidth,
  sharedTileQualityRules,
  tileQualityOf,
  type VideoQuality,
} from "./video-wants";
import {
  callBarMuteControl,
  callBarPhase,
  callBarStatus,
  type CallBarRules,
} from "./call-bar-model";
import {
  CallWindowDrag,
  callWindowSize,
  cornerPosition,
  insetCorner,
  insetShown,
  othersBeyondShown,
  rememberCorner,
  rememberedCorner,
  NO_INSETS,
  type CallWindowCorner,
  type CallWindowInsets,
  type CallWindowPoint,
  type CallWindowViewport,
} from "./call-window-model";
import {
  OS_CALL_WINDOW_HEIGHT,
  OS_CALL_WINDOW_WIDTH,
  type CallWindowPicture,
  type CallWindowView,
} from "./desktop-call-window-model";
import { startCallWindowRelay } from "./desktop-call-window-relay";

/** Where the window reads the call's pictures from: the same functions the
 *  host hands the call screen, for whichever call is live. */
export interface CallWindowSources {
  readonly remoteStreamFor: (participantId: string) => MediaStream | undefined;
  readonly remoteLightStreamFor?: ((participantId: string) => MediaStream | undefined) | undefined;
  readonly remoteMiddleStreamFor?: ((participantId: string) => MediaStream | undefined) | undefined;
  readonly remoteVideoGivenFor?: ((participantId: string) => VideoQuality | undefined) | undefined;
  /** What the window shows of a person, for the SFU (`video-wants.ts`).
   *  Absent on a one-to-one call, which has one picture per person. */
  readonly reportTileVideoQuality?:
    | ((tile: object, participantId: string, quality: VideoQuality | null) => void)
    | undefined;
  readonly localStream: () => MediaStream | undefined;
  readonly remoteAudioReceiverFor?: ((participantId: string) => LevelReceiver | undefined) | undefined;
}

export interface CallWindowProps {
  /** The other person, as the host resolved them, or `null` on a group or
   *  link call. Exactly what the host hands the call screen. */
  readonly peerName: string | null;
  /** The other person's colour id on a one-to-one call (the same id the call
   *  screen hashes for the portrait); `null` otherwise. */
  readonly peerId?: string | null;
  /** The other person's key for their photo on a one-to-one call. */
  readonly peerPersonKey?: string | null;
  /** A group or link call: the group label rather than a person's name. */
  readonly groupCall: boolean;
  /** The group's shared name on a group call placed in a conversation. */
  readonly groupName?: string | null;
  /** A 1x1 call is ringing and unanswered (the one phase the store does not
   *  carry, because such a call takes the whole screen, §2.2). */
  readonly ringing: boolean;
  /** The call screen is up; the window is never shown over it (§4.5.3). */
  readonly callScreenUp: boolean;
  /** Bring the call back. */
  readonly onExpand: () => void;
  /** The live call's pictures. */
  readonly sources: CallWindowSources;
  /**
   * Where the collapsed call is drawn: in the page, or (the desktop app,
   * section 4.5.4a) in a window of the operating system the page feeds.
   * Defaults to the build: the desktop app is `os-window`.
   */
  readonly presentation?: "in-app" | "os-window";
}

/** Who the window shows. */
type Subject =
  | { readonly kind: "person"; readonly person: Participant; readonly isYou: boolean }
  | { readonly kind: "screen"; readonly sharerName: string };

/**
 * The window, or nothing.
 *
 * Renders nothing until wasm has loaded: the visibility decision is the shared
 * rule's and there is no second copy of it here to fall back to.
 */
export function CallWindow(props: CallWindowProps): JSX.Element {
  const rules = (): CallBarRules | null => uiRules();

  const phase = createMemo(() => {
    const r = rules();
    if (r === null) return null;
    return callBarPhase(r, callState.phase().kind, props.ringing);
  });

  const shown = createMemo(() => {
    const r = rules();
    const p = phase();
    if (r === null || p === null) return false;
    return r.callBarVisible(p, props.callScreenUp);
  });

  const osWindow = (): boolean => (props.presentation ?? (isDesktop() ? "os-window" : "in-app")) === "os-window";

  return (
    <Show when={shown()}>
      <CallWindowBody {...props} rules={rules} phase={phase} osWindow={osWindow()} />
    </Show>
  );
}

/** The window while it is up. A component of its own so its timers, its
 *  speaking monitor and what it reports to the SFU exist only while it does. */
function CallWindowBody(
  props: CallWindowProps & {
    readonly rules: () => CallBarRules | null;
    readonly phase: () => ReturnType<typeof callBarPhase> | null;
    /** Drawn in a window of the operating system (section 4.5.4a). */
    readonly osWindow: boolean;
  },
): JSX.Element {
  const now = tickingClock();
  /** Fixed for the life of the window: the build decides it. */
  const osWindow = untrack(() => props.osWindow);

  const local = (): Participant | null =>
    callState.participants().find((participant) => participant.isLocal) ?? null;
  const remotes = createMemo(() => callState.participants().filter((p) => !p.isLocal));

  /** The call's label, the one the call screen uses; the window's spoken
   *  name (§4.5.1: it is not printed). */
  const label = createMemo(() =>
    callPeerLabel({
      surface: callSurfaceKind({
        groupCall: props.groupCall,
        remoteCount: remotes().length,
        remoteCameraOn: remotes()[0]?.camOn === true,
        localCameraOn: local()?.camOn === true,
      }),
      peerName: props.peerName,
      participantCount: callState.participants().length,
      soleRemoteName: remotes()[0]?.displayName,
      groupName: props.groupName,
    }),
  );

  const status = createMemo(() => {
    const r = props.rules();
    const p = props.phase();
    if (r === null || p === null) return { text: "", spoken: "" };
    return callBarStatus(r, {
      phase: p,
      connectedAtMs: callState.connectedAt(),
      nowMs: now(),
      sharingScreen: screenShareState.current()?.isOwn === true,
    });
  });

  /** What a screen reader hears when the state changes (§4.5.6): the state
   *  words, and `Connected` once the time runs — never the ticking time,
   *  which would be read out every second. */
  const announced = createMemo(() => {
    const r = props.rules();
    const p = props.phase();
    if (r === null || p === null) return "";
    return r.callBarStatusKind(p) === "elapsed" ? t("call.status.connected") : status().spoken;
  });

  const mute = createMemo(() =>
    callBarMuteControl(callState.phase().kind, local()?.micOn === true),
  );
  const encrypted = createMemo(() => showsEncryptionChip(callState.e2ee()));

  // ----- who is in the picture --------------------------------------------

  const people = createMemo((): LayoutTile[] =>
    withShareTile(callState.participants(), screenShareState.current()),
  );

  /**
   * The person the call screen would put on the stage (§4.5.1), by the shared
   * rule. A one-to-one call that is not connected yet has only this device in
   * its roster; the window then shows the person called, with their avatar,
   * as §4.5.3 says an outgoing call does.
   */
  const subject = createMemo((): Subject | null => {
    const r = uiRules();
    if (r === null) return null;
    const stage = stagePerson(r, people(), callState.pinnedId(), callState.speech());
    if (stage !== null && stage.screen === true) {
      return { kind: "screen", sharerName: screenShareState.current()?.name ?? "" };
    }
    const oneToOne = !props.groupCall;
    if (oneToOne && (stage === null || stage.isLocal) && remotes().length === 0) {
      return {
        kind: "person",
        isYou: false,
        person: {
          id: "peer",
          displayName: props.peerName ?? "",
          isLocal: false,
          micOn: true,
          camOn: false,
          quality: "good",
          speaking: false,
          personKeyB64: props.peerPersonKey ?? undefined,
        },
      };
    }
    if (stage === null) return null;
    return { kind: "person", person: stage, isYou: stage.isLocal };
  });

  const shownPerson = (): Participant | null => {
    const s = subject();
    return s?.kind === "person" ? s.person : null;
  };
  const shownId = createMemo((): string | null => shownPerson()?.id ?? null);
  const shownIsYou = (): boolean => {
    const s = subject();
    return s?.kind === "person" && s.isYou;
  };
  const showingScreen = (): boolean => subject()?.kind === "screen";

  /** The name under the picture: the label the person's tile uses. */
  const shownName = (): string => {
    const s = subject();
    if (s === null) return "";
    if (s.kind === "screen") return screenTileLabel(s.sharerName);
    const p = s.person;
    if (p.displayName !== "") return p.displayName;
    if (p.isLocal) return t("call.participant.you");
    if (!props.groupCall && props.peerName !== null && props.peerName !== "") return props.peerName;
    return t("call.participant.unnamed");
  };

  /** `+N` for everyone beyond the person shown and you; a group call only. */
  const others = (): number =>
    props.groupCall ? othersBeyondShown(callState.participants().length, shownIsYou()) : 0;

  const ownCameraSending = (): boolean =>
    local()?.camOn === true &&
    !ownCameraPreparing(cameraBackgroundState(), callState.videoPublishState() === "starting") &&
    props.sources.localStream() !== undefined;

  /** The colour of whoever is shown, from the same id their tile hashes. */
  const toneId = (): string => {
    const p = shownPerson();
    if (p === null) return "";
    if (!props.groupCall && !p.isLocal) return props.peerId ?? p.id;
    return p.id;
  };

  // ----- size and place ---------------------------------------------------

  const viewport = viewportSignal();
  /** The screen's safe-area insets: the window never covers the system bars
   *  (§4.5.2). Read again whenever the viewport changes. The app's own
   *  bottom strip (the update banner, `ui/bottom-strip.ts`) counts as one, so
   *  a window in a bottom corner sits above it, and moves up when it appears. */
  const insets = createMemo((): CallWindowInsets => {
    viewport();
    const safe = readSafeInsets();
    return withBottomStrip(safe, bottomStripPx());
  });
  const size = createMemo(() =>
    osWindow
      ? { width: OS_CALL_WINDOW_WIDTH, height: OS_CALL_WINDOW_HEIGHT }
      : callWindowSize(viewport(), showingScreen(), insets()),
  );
  const [corner, setCorner] = createSignal<CallWindowCorner>(rememberedCorner());
  /** While a drag is under way: where the window is. `null` at rest. */
  const [dragAt, setDragAt] = createSignal<CallWindowPoint | null>(null);
  /** How far down the screen's own header reaches in the column of the
   *  window's corner (§4.5.2: the window starts below it). Re-read with the
   *  clock, because a route change replaces the header under the window. */
  const topClearance = createMemo(() => {
    now();
    return headerBottom(corner(), viewport());
  });
  const restAt = createMemo(() => cornerPosition(corner(), size(), viewport(), topClearance(), insets()));
  const position = (): CallWindowPoint => dragAt() ?? restAt();

  let drag: CallWindowDrag | null = null;
  let windowEl: HTMLDivElement | undefined;
  /** A drag ends with a click on whatever was under the pointer; that click
   *  is not a request to expand. */
  let swallowClick = false;

  /**
   * A press is followed on the document, not on the window: a pointer moved
   * quickly leaves the window before its first move arrives, and the window
   * would never hear it. Not captured either, so a plain click still reaches
   * the button under the pointer and expands the call.
   */
  const onPointerMove = (event: PointerEvent): void => {
    if (drag === null) return;
    const at = drag.move({ x: event.clientX, y: event.clientY }, event.timeStamp);
    if (at !== null) setDragAt(at);
  };
  const stopFollowing = (): void => {
    globalThis.document?.removeEventListener("pointermove", onPointerMove, true);
    globalThis.document?.removeEventListener("pointerup", finishDrag, true);
    globalThis.document?.removeEventListener("pointercancel", cancelDrag, true);
  };
  const finishDrag = (event: PointerEvent): void => {
    stopFollowing();
    const current = drag;
    drag = null;
    if (current === null) return;
    const release = current.release(size(), viewport(), event.timeStamp);
    if (release.kind === "click") return;
    swallowClick = true;
    setCorner(release.corner);
    rememberCorner(release.corner);
    setDragAt(null);
  };
  const cancelDrag = (): void => {
    stopFollowing();
    drag = null;
    setDragAt(null);
  };
  const onPointerDown = (event: PointerEvent): void => {
    // The OS window is dragged and clicked in its own page.
    if (osWindow) return;
    if (event.button !== 0) return;
    // A click swallowed after a drag that ended off the window never came.
    swallowClick = false;
    // The mute button is its own control; a press on it never drags.
    if ((event.target as Element | null)?.closest(".lk-call-window-mute") !== null) return;
    stopFollowing();
    drag = new CallWindowDrag({ x: event.clientX, y: event.clientY }, position(), event.timeStamp);
    globalThis.document?.addEventListener("pointermove", onPointerMove, true);
    globalThis.document?.addEventListener("pointerup", finishDrag, true);
    globalThis.document?.addEventListener("pointercancel", cancelDrag, true);
  };
  onCleanup(stopFollowing);
  // The click that ends a drag lands on the button under the pointer; it is
  // not a request to expand. Caught on the way down, before the button.
  onMount(() => {
    const el = windowEl;
    if (el === undefined) return;
    const onClick = (event: MouseEvent): void => {
      if (!swallowClick) return;
      swallowClick = false;
      event.stopPropagation();
      event.preventDefault();
    };
    el.addEventListener("click", onClick, true);
    onCleanup(() => el.removeEventListener("click", onClick, true));
  });

  // ----- what the SFU is asked for ----------------------------------------

  /** The window's width in physical pixels: what the picture is drawn over. */
  const widthPx = (): number => physicalTileWidth(size().width, globalThis.devicePixelRatio ?? 1);

  /** Which picture of the shown person the window asks for: the shared rule,
   *  for the window's width, as not the stage tile. */
  const quality = createMemo((): VideoQuality | null => {
    const p = shownPerson();
    if (p === null || p.isLocal || props.sources.reportTileVideoQuality === undefined) return null;
    uiRules();
    return askedQuality(
      tileQualityOf(sharedTileQualityRules, callState.participants().length, widthPx(), false),
      props.sources.remoteLightStreamFor?.(p.id) !== undefined,
    );
  });

  const tileKey = {};
  let reportedFor: string | null = null;
  createEffect(() => {
    const report = props.sources.reportTileVideoQuality;
    if (report === undefined) return;
    const p = shownPerson();
    const draws =
      p !== null && !p.isLocal && p.camOn && props.sources.remoteStreamFor(p.id) !== undefined;
    if (p === null || p.isLocal) {
      if (reportedFor !== null) report(tileKey, reportedFor, null);
      reportedFor = null;
      return;
    }
    report(tileKey, p.id, draws ? quality() : null);
    reportedFor = p.id;
  });
  onCleanup(() => {
    const id = reportedFor;
    if (id === null) return;
    try {
      untrack(() => props.sources.reportTileVideoQuality)?.(tileKey, id, null);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("call window: could not withdraw what it asked for", err);
    }
  });

  // ----- who is speaking ---------------------------------------------------

  // The call screen reads who is speaking for its stage, and is taken down
  // while the call is collapsed. A group call's window follows the speaker by
  // the same reading, so it runs here while the window is up; never both,
  // because the window is never shown over the call screen.
  const speaking = new SpeakingMonitor((snapshot) => {
    callActions.setSpeech(snapshot.speakingIds, {
      activeSpeakerId: snapshot.activeSpeakerId,
      lastSpokeAtMs: snapshot.lastSpokeAtMs,
    });
  });
  onCleanup(() => speaking.dispose());
  const voices = createMemo(
    (): VoiceSource[] =>
      props.groupCall
        ? callState.participants().map((p) => ({
            id: p.id,
            isLocal: p.isLocal,
            micOn: p.micOn,
            stream: p.isLocal ? props.sources.localStream() : props.sources.remoteStreamFor(p.id),
            receiver: p.isLocal ? undefined : props.sources.remoteAudioReceiverFor?.(p.id),
          }))
        : [],
  );
  createEffect(() => speaking.sync(voices()));
  // A browser starts an audio context only on a user gesture, and while the
  // call is collapsed the gestures happen in the app around the window, not
  // on the call screen that used to pass them on.
  onMount(() => {
    if (!props.groupCall) return;
    const onGesture = (): void => {
      speaking.userGesture();
    };
    globalThis.document?.addEventListener("pointerdown", onGesture, true);
    globalThis.document?.addEventListener("keydown", onGesture, true);
    onCleanup(() => {
      globalThis.document?.removeEventListener("pointerdown", onGesture, true);
      globalThis.document?.removeEventListener("keydown", onGesture, true);
    });
  });

  // ----- the spoken name -----------------------------------------------------

  /**
   * One button is the way back in, and its label names who the call is with
   * and how long it has been running (§4.5.6). An `aria-label` on a button
   * replaces everything inside it, so the lock and the inset are folded in
   * here: otherwise a screen reader user is never told their camera is still
   * sending.
   */
  const spokenName = createMemo(() => {
    const parts = [t("call.bar.return_a11y", { name: label() }), status().spoken];
    if (encrypted()) parts.push(t("call.bar.encrypted_a11y"));
    if (local()?.camOn === true) parts.push(t("call.bar.camera_on_a11y"));
    return joinList(parts.filter((part) => part.length > 0));
  });

  // ----- the desktop app: feeding the window of its own (§4.5.4a) ---------

  /** What the OS window draws for the picture, by the same reading
   *  `PersonPicture` makes. */
  const pictureView = (): CallWindowPicture => {
    const s = subject();
    if (s === null) return { kind: "blank" };
    if (s.kind === "screen") return { kind: "frame" };
    const p = s.person;
    if (p.camOn) {
      const stream = p.isLocal ? props.sources.localStream() : props.sources.remoteStreamFor(p.id);
      return stream !== undefined ? { kind: "frame" } : { kind: "blank" };
    }
    if (avatarMediaFor(p.personKeyB64, false)?.stillUrl !== undefined) return { kind: "frame" };
    return { kind: "initials", text: initialsOf(shownName()), tone: toneOf(toneId()) };
  };

  if (osWindow) {
    const osView = createMemo((): CallWindowView => {
      const spec = mute();
      const root = globalThis.document?.documentElement;
      return {
        status: status().text,
        encrypted: encrypted(),
        name: shownName(),
        others: others(),
        mute:
          spec === null
            ? null
            : {
                icon: spec.icon,
                tone: spec.tone,
                pressed: spec.pressed,
                label: spec.accessibleName,
                disabled: spec.disabled,
              },
        picture: pictureView(),
        inset: insetShown(ownCameraSending(), shownIsYou()),
        spoken: spokenName(),
        announced: announced(),
        region: t("call.bar.region_a11y"),
        lang: root?.lang ?? "en",
        dir: root?.dir === "rtl" ? "rtl" : "ltr",
      };
    });

    onMount(() => {
      const root = windowEl;
      if (root === undefined) return;
      const stop = startCallWindowRelay({
        root,
        view: osView,
        pictureIsScreen: showingScreen,
        pictureMirrored: shownIsYou,
      });
      onCleanup(stop);
    });
    const unsubscribe = onHostCallWindowAction((action) => {
      if (action === "expand") {
        props.onExpand();
        return;
      }
      const spec = untrack(mute);
      if (spec === null || spec.disabled) return;
      callActions.setMic(untrack(local)?.micOn !== true);
    });
    onCleanup(unsubscribe);
  }

  return (
    <div
      class="lk-call-window"
      ref={windowEl}
      role={osWindow ? undefined : "region"}
      aria-label={osWindow ? undefined : t("call.bar.region_a11y")}
      aria-hidden={osWindow ? "true" : undefined}
      inert={osWindow ? true : undefined}
      data-corner={corner()}
      data-shape={osWindow || showingScreen() || !isUpright(size()) ? "landscape" : "portrait"}
      classList={{ dragging: dragAt() !== null, "lk-call-window-relay": osWindow }}
      style={{
        width: `${size().width}px`,
        height: `${size().height}px`,
        transform: osWindow
          ? undefined
          : `translate(${Math.round(position().x)}px, ${Math.round(position().y)}px)`,
      }}
      onPointerDown={onPointerDown}
    >
      {/* The picture: the stage person, drawn as their tile draws them. */}
      <div class="lk-call-tile lk-call-window-picture" data-tone={toneOf(toneId())}>
        <Show
          when={!showingScreen()}
          fallback={
            <Show when={screenShareState.current()}>
              {(share) => (
                <ScreenShareView
                  share={share()}
                  stream={currentShareStream()}
                  large={false}
                  class="lk-call-window-share"
                />
              )}
            </Show>
          }
        >
          {/* Keyed by the person's id, never by the roster object: the store
              replaces a person's object whenever anything about them changes,
              and a picture rebuilt on every change of the speaking ring would
              restart its video each time. */}
          <Show when={shownId()} keyed>
            {(id) => (
              <PersonPicture
                id={id}
                person={shownPerson}
                sources={props.sources}
                quality={quality}
                name={shownName}
              />
            )}
          </Show>
        </Show>
      </div>

      {/* Your own camera while it sends, mirrored, in the bottom corner away
          from the edge the window rests against. */}
      <Show when={insetShown(ownCameraSending(), shownIsYou())}>
        <div class="lk-call-window-inset" data-corner={insetCorner(corner())} aria-hidden="true">
          <LocalVideo
            stream={props.sources.localStream()}
            where="window"
            class="lk-call-tile-video mirrored"
          />
        </div>
      </Show>

      {/* Top left: the state or the running time, never two at once (the
          shared rule answered one of them), and the lock beside it. */}
      <div class="lk-call-window-top" aria-hidden="true">
        <span class="lk-call-window-state">{status().text}</span>
        <Show when={encrypted()}>
          <span class="lk-call-window-mark">
            <Icon name="lock-fill" size={14} />
          </span>
        </Show>
      </div>

      {/* Bottom: who is shown, and `+N` in a group call. */}
      <div class="lk-call-window-bottom" aria-hidden="true">
        <span class="lk-call-window-name">{shownName()}</span>
        <Show when={others() > 0}>
          <span class="lk-call-window-more">{`+${others()}`}</span>
        </Show>
      </div>

      {/* The whole window is the way back in: a real button over it, so
          `Enter` and `Space` work and the mute button is a sibling rather
          than nested inside another control. The OS window has its own. */}
      <Show when={!osWindow}>
        <button
          type="button"
          class="lk-call-window-open"
          aria-label={spokenName()}
          onClick={(): void => props.onExpand()}
        />
      </Show>

      <Show when={osWindow ? null : mute()}>
        {(spec) => (
          <button
            type="button"
            class="lk-call-window-mute"
            data-tone={spec().tone}
            aria-label={spec().accessibleName}
            aria-pressed={spec().pressed ?? undefined}
            title={spec().accessibleName}
            disabled={spec().disabled}
            onClick={(event): void => {
              // The window around it expands; this one control does not.
              event.stopPropagation();
              callActions.setMic(local()?.micOn !== true);
            }}
          >
            <Icon name={spec().icon} size={20} />
          </button>
        )}
      </Show>

      <Show when={!osWindow}>
        <span class="lk-visually-hidden" role="status" aria-live="polite">
          {announced()}
        </span>
      </Show>
    </div>
  );
}

/** One person's picture: their camera when it sends, else their avatar, as
 *  their tile draws them. */
function PersonPicture(props: {
  readonly id: string;
  readonly person: () => Participant | null;
  readonly sources: CallWindowSources;
  readonly quality: () => VideoQuality | null;
  readonly name: () => string;
}): JSX.Element {
  const id = untrack(() => props.id);
  // Read through `lastDefined`: the picture is taken down after the person
  // stopped being the one shown, and must not read a roster that no longer
  // names them (`calls/held.ts`).
  const person = lastDefined(props.person);
  const isLocal = untrack(() => person()?.isLocal === true);
  const stream = (): MediaStream | undefined =>
    isLocal ? props.sources.localStream() : props.sources.remoteStreamFor(id);
  const shownStream = lastDefined(stream);
  return (
    <Show
      when={person()?.camOn === true}
      fallback={
        <div class="lk-call-tile-avatar" aria-hidden="true">
          <PhotoOrInitials
            name={props.name()}
            personKey={person()?.personKeyB64}
            class="lk-call-photo lk-call-tile-photo"
          />
        </div>
      }
    >
      <Show
        when={stream() !== undefined}
        fallback={
          <div class="lk-call-tile-blank" aria-hidden="true">
            <Icon name="video" size={24} />
          </div>
        }
      >
        <Show
            when={!isLocal}
            fallback={<LocalVideo stream={shownStream()} where="window" class="lk-call-tile-video mirrored" />}
          >
            <RemoteVideo
              stream={shownStream() as MediaStream}
              middle={props.sources.remoteMiddleStreamFor?.(id)}
              light={props.sources.remoteLightStreamFor?.(id)}
              quality={props.quality() ?? undefined}
              given={props.sources.remoteVideoGivenFor?.(id)}
              peerId={peerIdOfParticipant(id)}
              class="lk-call-tile-video"
            />
          </Show>
      </Show>
    </Show>
  );
}

function isUpright(size: { readonly width: number; readonly height: number }): boolean {
  return size.height > size.width;
}

/**
 * The viewport as a signal, updated on resize. `visualViewport` where the
 * browser has it, so a phone's on-screen keyboard shrinks the space and a
 * bottom-corner window moves up with it (§4.5.2).
 */
function viewportSignal(): () => CallWindowViewport {
  const read = (): CallWindowViewport => {
    const vv = globalThis.visualViewport;
    if (vv !== undefined && vv !== null && vv.width > 0 && vv.height > 0) {
      return { width: vv.width, height: vv.height };
    }
    return { width: globalThis.innerWidth ?? 0, height: globalThis.innerHeight ?? 0 };
  };
  const [viewport, setViewport] = createSignal(read());
  onMount(() => {
    const update = (): void => {
      setViewport(read());
    };
    globalThis.addEventListener("resize", update);
    globalThis.visualViewport?.addEventListener("resize", update);
    onCleanup(() => {
      globalThis.removeEventListener("resize", update);
      globalThis.visualViewport?.removeEventListener("resize", update);
    });
  });
  return viewport;
}

/** `insets` with the app's bottom strip added to the bottom one. Pure. */
export function withBottomStrip(insets: CallWindowInsets, stripPx: number): CallWindowInsets {
  return stripPx > 0 ? { ...insets, bottom: insets.bottom + stripPx } : insets;
}

/**
 * The safe-area insets, in CSS pixels: `env(safe-area-inset-*)` is readable
 * only through a property it resolves into, so a hidden fixed element takes
 * it as padding for one measurement and is removed. Every inset reads as 0
 * where the browser has none (a desktop browser) or there is no document.
 */
function readSafeInsets(): CallWindowInsets {
  const doc = globalThis.document;
  if (doc?.body === undefined || doc.body === null) return NO_INSETS;
  const probe = doc.createElement("div");
  probe.setAttribute("aria-hidden", "true");
  probe.style.cssText =
    "position:fixed;top:0;left:0;width:0;height:0;visibility:hidden;pointer-events:none;" +
    "padding:env(safe-area-inset-top,0px) env(safe-area-inset-right,0px) " +
    "env(safe-area-inset-bottom,0px) env(safe-area-inset-left,0px);";
  doc.body.appendChild(probe);
  try {
    const cs = globalThis.getComputedStyle(probe);
    const px = (value: string): number => {
      const n = Number.parseFloat(value);
      return Number.isFinite(n) && n > 0 ? n : 0;
    };
    return {
      top: px(cs.paddingTop),
      right: px(cs.paddingRight),
      bottom: px(cs.paddingBottom),
      left: px(cs.paddingLeft),
    };
  } finally {
    probe.remove();
  }
}

/** The headers a top corner keeps clear of: a conversation's, a list pane's
 *  and a settings layer's. */
const HEADER_SELECTOR = ".lk-chat-head, .lk-pane-head, .lk-layer-head";

/**
 * How far down the header under the window's corner reaches, in CSS pixels;
 * `0` for a bottom corner or where there is none. Only headers near the top of
 * the viewport and in the column of the corner count.
 */
function headerBottom(corner: CallWindowCorner, viewport: CallWindowViewport): number {
  if (corner === "bottom-left" || corner === "bottom-right") return 0;
  const doc = globalThis.document;
  if (doc === undefined) return 0;
  const right = corner === "top-right";
  let bottom = 0;
  for (const el of Array.from(doc.querySelectorAll(HEADER_SELECTOR))) {
    const rect = el.getBoundingClientRect();
    if (rect.width <= 0 || rect.height <= 0) continue;
    if (rect.top > 160 || rect.bottom <= 0) continue;
    const inColumn = right ? rect.right >= viewport.width - 40 : rect.left <= 40;
    if (inColumn) bottom = Math.max(bottom, rect.bottom);
  }
  return bottom;
}

/**
 * `Date.now()` as a signal that ticks once a second while the window is up.
 * A second is the resolution a running time needs, the same cadence the call
 * screen uses for its own line.
 */
function tickingClock(): () => number {
  const [now, setNow] = createSignal(Date.now());
  const handle = globalThis.setInterval(() => setNow(Date.now()), 1_000);
  onCleanup(() => globalThis.clearInterval(handle));
  return now;
}

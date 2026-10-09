// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * SFrame transform worker: the event wiring of the per-frame seal / open
 * engine. Companion to the main-thread controller in `./sframe-transform.ts`
 * (see that file for the architecture and the native-shim parallel).
 *
 * This script runs OFF the main thread. Everything it does per frame is in
 * `./sframe-worker-core.ts`; this file only connects the two ways a browser
 * hands a worker a stream of encoded frames to that one engine:
 *
 *   * `rtctransform` - the W3C Encoded Transform API. The browser fires one
 *     event per `RTCRtpScriptTransform` the main thread constructs; it
 *     carries an `RTCRtpScriptTransformer` whose `readable` / `writable` are
 *     the encoded frames and whose `options` are what the main thread passed.
 *   * `message` with `type: "encoded-streams"` - the earlier
 *     insertable-streams interface. The main thread called
 *     `createEncodedStreams()` on a sender or receiver and transferred the
 *     readable / writable pair here, with the same options.
 *
 * Which of the two a page uses is decided in `./sframe-transport.ts`. The
 * `rekey` and `codecs` control messages address a transform by its id and
 * reach one created either way.
 *
 * A frame that cannot be sealed or opened is dropped, never passed on, and a
 * transform that cannot be built leaves its streams unconnected (security
 * invariant #4); see the engine.
 *
 * Until the native clients move to the ADR-0071 frame layout (phase 2), a
 * web client and a native client do not open each other's media.
 */

import { loadWasm } from "../wasm";
import {
  createSFrameWorkerCore,
  type EncodedStreams,
} from "./sframe-worker-core";

/**
 * Worker-side view of the Encoded Transform event. It is part of
 * `DedicatedWorkerGlobalScope` and is NOT in TypeScript's bundled lib
 * definitions, so the minimal surface touched is declared here.
 */
interface RTCTransformEvent extends Event {
  readonly transformer: EncodedStreams & { readonly options: unknown };
}

const core = createSFrameWorkerCore({
  loadWasm,
  post: (message) => {
    (self as unknown as { postMessage: (m: unknown) => void }).postMessage(message);
  },
});

addEventListener("message", (event: MessageEvent): void => {
  core.onMessage(event.data);
});

addEventListener("rtctransform", (event: Event): void => {
  core.onTransformer((event as RTCTransformEvent).transformer);
});

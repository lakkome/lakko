// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Put the `generic-frame-descriptor-00` RTP header extension on the video
 * this browser publishes.
 *
 * ## Why a native phone cannot decode web video without it
 *
 * Lakko seals the whole encoded frame with SFrame before libwebrtc packetizes
 * it. A libwebrtc receiver reads the key-frame flag from the first byte of the
 * VP8 bitstream and the picture size from bytes 6..9 of it — both ciphertext
 * here — unless the packet carries the generic frame descriptor, which states
 * them in the clear. iOS and Android advertise the descriptor through a
 * libwebrtc field trial (`WebRTC-GenericDescriptorAdvertised`) and the SFU
 * relays it (`services/sfu/src/frame_descriptor.rs`), so native-to-native
 * video works. Chrome ships the extension in a `stopped` state and a web page
 * cannot set field trials, so a web publisher sent video with no descriptor and
 * every phone receiving it guessed key frames from random bytes: a smeared
 * picture on Android, a blank tile on an iPhone that joined after the web had
 * started sending (2026-09-19, the four-way call).
 *
 * The Header Extension Control API (`setHeaderExtensionsToNegotiate`) lets the
 * page turn a `stopped` extension back on before the offer is made, which is
 * what this does for every video transceiver. The dependency descriptor stays
 * as it is: with both enabled libwebrtc sends only the dependency descriptor,
 * which the natives did not negotiate (see the Android field-trial note).
 *
 * The descriptor puts the frame type and picture size on the wire in the
 * clear, where a relay that chose to look could read them. That is the same
 * exposure the natives already accept, and the SFU module documents it.
 */

/** libwebrtc's URI for the extension. */
export const GENERIC_FRAME_DESCRIPTOR_URI =
  "http://www.webrtc.org/experiments/rtp-hdrext/generic-frame-descriptor-00";

/** One entry of the Header Extension Control API. */
export interface NegotiableHeaderExtension {
  readonly uri: string;
  readonly direction: string;
}

/** The slice of `RTCRtpTransceiver` this reads. TypeScript's DOM library does
 *  not yet declare the two methods, so the session casts to this shape. */
export interface HeaderExtensionTransceiver {
  readonly receiver: { readonly track: { readonly kind: string } | null };
  getHeaderExtensionsToNegotiate?: () => NegotiableHeaderExtension[];
  setHeaderExtensionsToNegotiate?: (extensions: NegotiableHeaderExtension[]) => void;
}

/** What happened, per call: how many video transceivers now negotiate the
 *  descriptor, and how many could not (no API, or the browser refused). */
export interface FrameDescriptorOutcome {
  readonly armed: number;
  readonly unsupported: number;
}

/**
 * Ask every video transceiver to negotiate the generic frame descriptor.
 *
 * Idempotent: a transceiver already at `sendrecv` counts as armed. A browser
 * without the API, or one that throws on the change, is counted rather than
 * failing the call — the call still works between browsers, and a native
 * receiver gets the picture it got before.
 */
export function armGenericFrameDescriptor(
  transceivers: readonly HeaderExtensionTransceiver[],
): FrameDescriptorOutcome {
  let armed = 0;
  let unsupported = 0;
  for (const t of transceivers) {
    if (t.receiver.track?.kind !== "video") continue;
    const get = t.getHeaderExtensionsToNegotiate;
    const set = t.setHeaderExtensionsToNegotiate;
    if (get === undefined || set === undefined) {
      unsupported += 1;
      continue;
    }
    try {
      const current = get.call(t);
      const entry = current.find((h) => h.uri === GENERIC_FRAME_DESCRIPTOR_URI);
      if (entry === undefined) {
        unsupported += 1;
        continue;
      }
      if (entry.direction !== "sendrecv") {
        set.call(
          t,
          current.map((h) =>
            h.uri === GENERIC_FRAME_DESCRIPTOR_URI ? { uri: h.uri, direction: "sendrecv" } : h,
          ),
        );
      }
      armed += 1;
    } catch {
      unsupported += 1;
    }
  }
  return { armed, unsupported };
}

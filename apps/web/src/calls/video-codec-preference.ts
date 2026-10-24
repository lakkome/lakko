// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Negotiate VP8 for a one-to-one call's video.
 *
 * A sealed media frame (ADR-0071) is opaque to the browser's RTP
 * packetizer, and only VP8 has a rule for what stays in the clear. A codec
 * whose packetizer reads the frame's own structure cannot carry it: WebKit's
 * H264 packetizer looks for NAL start codes, finds none in a sealed frame and
 * sends nothing at all. Measured in WebKit 2359 (Playwright 1.63): two
 * production `CallSession`s, WebKit negotiated H264 (its first choice), the
 * caller encoded 113 video frames and sent 0 packets, while the audio beside
 * them flowed. The group call never met this because the SFU only offers VP8;
 * between two browsers nothing chose the codec, and two WebKit peers, or a
 * call whose offer came from Safari, landed on H264.
 *
 * So every video transceiver of a one-to-one call is told to prefer VP8 and
 * nothing else that carries media, before every description. The
 * retransmission and redundancy formats stay, as they wrap the VP8 payload
 * rather than replace it. A browser without VP8, or without
 * `setCodecPreferences`, is left as it is and the caller says so: its video
 * may not flow, and nothing about that is hidden.
 */

/** One codec as `RTCRtpReceiver.getCapabilities` lists it. */
export interface CodecCapabilityLike {
  readonly mimeType: string;
  readonly clockRate: number;
  readonly channels?: number;
  readonly sdpFmtpLine?: string;
}

/** The part of an `RTCRtpTransceiver` this needs. */
export interface CodecPreferenceTransceiver {
  readonly receiver: { readonly track: { readonly kind: string } | null };
  setCodecPreferences?(codecs: CodecCapabilityLike[]): void;
}

/** The formats that wrap a media payload rather than carry one. */
const WRAPPERS = new Set(["video/rtx", "video/red", "video/ulpfec", "video/flexfec-03"]);

/** VP8 first, then the wrappers, in the order the browser listed them. `null`
 *  when the browser offers no VP8. Pure. */
export function vp8Preferences(
  capabilities: readonly CodecCapabilityLike[],
): CodecCapabilityLike[] | null {
  const vp8 = capabilities.filter((c) => c.mimeType.toLowerCase() === "video/vp8");
  if (vp8.length === 0) return null;
  const wrappers = capabilities.filter((c) => WRAPPERS.has(c.mimeType.toLowerCase()));
  return [...vp8, ...wrappers];
}

/** What {@link preferVp8} did. */
export interface PreferVp8Outcome {
  /** Video transceivers now preferring VP8. */
  readonly applied: number;
  /** Video transceivers it could not set (no VP8, no API, or refused). */
  readonly unsupported: number;
}

/** Tell every video transceiver to prefer VP8. Safe to call before every
 *  description: the same list is set again. */
export function preferVp8(
  transceivers: readonly CodecPreferenceTransceiver[],
  capabilities: readonly CodecCapabilityLike[] | null,
): PreferVp8Outcome {
  const prefs = capabilities === null ? null : vp8Preferences(capabilities);
  let applied = 0;
  let unsupported = 0;
  for (const t of transceivers) {
    if (t.receiver.track?.kind !== "video") continue;
    if (prefs === null || typeof t.setCodecPreferences !== "function") {
      unsupported += 1;
      continue;
    }
    try {
      t.setCodecPreferences(prefs);
      applied += 1;
    } catch {
      unsupported += 1;
    }
  }
  return { applied, unsupported };
}

/** The browser's video receive capabilities, or `null` where it gives none. */
export function videoReceiveCapabilities(): readonly CodecCapabilityLike[] | null {
  try {
    const caps = (
      globalThis as { RTCRtpReceiver?: { getCapabilities?: (k: string) => { codecs: CodecCapabilityLike[] } | null } }
    ).RTCRtpReceiver?.getCapabilities?.("video");
    return caps?.codecs ?? null;
  } catch {
    return null;
  }
}

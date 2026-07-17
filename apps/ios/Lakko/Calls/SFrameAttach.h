// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors
//
// LakkoSFrameAttach — Objective-C++ shim that attaches the
// `apps/shared/sframe-bridge` LakkoFrameEncryptor /
// LakkoFrameDecryptor (G3-sframe-native N-3) to a libwebrtc
// `RTCRtpSender` / `RTCRtpReceiver` from stasel/WebRTC 125.0.0
// on iOS (G3-sframe-native N-4).
//
// This header is the ONLY surface the Swift call layer ever
// imports — it deliberately exposes no C++ types. The Obj-C++
// implementation (`SFrameAttach.mm`) consumes
// `apps/shared/sframe-bridge/sframe-bridge.h` + the M125
// `RTCRtpSender` / `RTCRtpReceiver` header pair internally; the
// Swift caller sees only `NSData`, `uint32_t`, and the typed
// status enum below.
//
// ## Security posture (invariant #4: no fallback to non-E2EE)
//
// libwebrtc M125's `RTCRtpSender` Obj-C surface exposes a
// non-public selector
//
//   - (void)setFrameEncryptor:(rtc::scoped_refptr<webrtc::
//                              FrameEncryptorInterface>)encryptor
//
// (and a symmetric `-[RTCRtpReceiver setFrameDecryptor:]`).
// stasel/WebRTC 125.0.0 is an upstream-binary repackaging of the
// branch-heads/6422 milestone, so the same selector ships in the
// .framework binary; what is NOT pinned is whether the public
// Obj-C header re-exports it. The selector probe in
// `+[LakkoSFrameAttach bootProbe]` resolves it via
// `class_respondsToSelector(...)` at runtime.
//
// If the probe fails — either because the framework was built
// with a different surface, or because a milestone bump dropped
// the selector — `bootProbe` returns NO. The Swift caller MUST
// honour this: `CallSession.start(...)` aborts the call rather
// than letting media flow unencrypted. There is no silent
// downgrade path; that is the explicit refusal the audit pack
// pins under security invariant #4.

#ifndef LAKKO_SFRAME_ATTACH_H_
#define LAKKO_SFRAME_ATTACH_H_

#import <Foundation/Foundation.h>

// Forward-declare to avoid pulling the full WebRTC umbrella header
// into Swift bridging. The shim's .mm includes
// <WebRTC/RTCRtpSender.h> + <WebRTC/RTCRtpReceiver.h> at the
// implementation site.
@class RTCRtpSender;
@class RTCRtpReceiver;

NS_ASSUME_NONNULL_BEGIN

typedef NS_ENUM(NSInteger, LakkoSFrameAttachStatus) {
    /// Attached. The encryptor / decryptor is now live on the
    /// (sender|receiver); freeing the wrapping NSObject auto-
    /// releases the C++ side via its refcount.
    LakkoSFrameAttachStatusOK = 0,
    /// `+bootProbe` found that the non-public
    /// `setFrameEncryptor:` / `setFrameDecryptor:` selectors are
    /// absent from the running framework. Refuse the call —
    /// shipping media unencrypted under MLS keys is a security
    /// invariant #4 violation.
    LakkoSFrameAttachStatusSelectorMissing = 1,
    /// `LakkoFrameEncryptor::Create` / `Decryptor::Create`
    /// returned nullptr. Either the seed was malformed (the
    /// 32-byte exporter output, exactly) or the underlying
    /// per-participant HKDF failed.
    LakkoSFrameAttachStatusBridgeCreateFailed = 2,
    /// The 32-byte MLS exporter seed (`Group::export_secret
    /// ("lakko/call/v1", call_id, 32)`) was missing or wrong
    /// length.
    LakkoSFrameAttachStatusBadSeedLength = 3,
    /// The Swift caller passed `nil` where a sender / receiver
    /// was required. Not a security failure, but indicates a
    /// wiring bug — the caller should not let nil reach this
    /// path.
    LakkoSFrameAttachStatusNullArgument = 4,
};

/// Stream kind folded into the SFrame per-participant key
/// derivation (ADR-0034). A participant's audio and video streams
/// derive on distinct key + nonce-salt spaces, so a frame counter
/// (nonce) reused across the two streams of one leaf no longer
/// collides — the cross-stream nonce-reuse bug ADR-0034 closes.
///
/// An encryptor is bound to exactly one local SEND track's kind,
/// and a decryptor to exactly one remote RECEIVE track's kind, for
/// its whole lifetime; the far-side encryptor must have sealed on
/// the same media type or the frame will not open (fail-closed).
///
/// The raw values are pinned to the `lakko-sframe-native` C ABI
/// convention (`LAKKO_SFRAME_MEDIA_TYPE_AUDIO` = 1 / `_VIDEO` = 2);
/// `0` is reserved and refused fail-closed by the bridge.
typedef NS_ENUM(uint8_t, LakkoSFrameMediaType) {
    /// Audio send / receive track.
    LakkoSFrameMediaTypeAudio = 1,
    /// Video send / receive track.
    LakkoSFrameMediaTypeVideo = 2,
};

@interface LakkoSFrameAttach : NSObject

/// One-shot capability probe — runs once at app startup
/// (typically from `LakkoApp.init`). Returns `YES` only if both
/// `setFrameEncryptor:` and `setFrameDecryptor:` selectors are
/// present on the loaded WebRTC framework's `RTCRtpSender` and
/// `RTCRtpReceiver` classes.
///
/// On `NO`, the caller MUST disable all calling features —
/// shipping unencrypted media under an MLS group's identity is
/// forbidden by security invariant #4. The proper UI surface is
/// a "Calling unavailable on this build of Lakko" banner with a
/// link to a support page explaining how to update the app.
///
/// Idempotent — repeated calls return the cached result.
+ (BOOL)bootProbe NS_SWIFT_NAME(bootProbe());

/// Build a fresh `LakkoFrameEncryptor` for `seed` + `localLeaf`,
/// then attach it to `sender` via the non-public
/// `setFrameEncryptor:` selector. Holds a strong retain on the
/// wrapper for the lifetime of the sender; detaching is the
/// caller's job at call-end via [-detachFromSender:].
///
/// `mediaType` is the kind of the local send track being armed
/// (ADR-0034) — the audio sender passes `Audio`, the video sender
/// `Video`. The encryptor derives + seals on that one media-type
/// key space for its whole lifetime.
///
/// Returns `LakkoSFrameAttachStatusOK` on success.
+ (LakkoSFrameAttachStatus)attachEncryptorToSender:
                              (RTCRtpSender * _Nullable)sender
                                                seed:(NSData *_Nullable)seed
                                           localLeaf:(uint32_t)localLeaf
                                           mediaType:(LakkoSFrameMediaType)mediaType
    NS_SWIFT_NAME(attachEncryptor(toSender:seed:localLeaf:mediaType:));

/// Build a fresh `LakkoFrameDecryptor` for `seed`, then attach
/// it to `receiver` via the non-public `setFrameDecryptor:`
/// selector. Per-sender ParticipantKey derivation happens lazily
/// inside the decryptor as frames arrive (see
/// `apps/shared/sframe-bridge/sframe-bridge.cc`'s
/// `lookup_or_derive`).
///
/// `mediaType` is the kind of the remote receive track being armed
/// (ADR-0034) — the Swift caller reads `RTCRtpReceiver.track`'s
/// kind and passes `Audio` / `Video`. The decryptor derives every
/// per-sender key on that one media-type space; a mismatch with the
/// far-side encryptor's space fails closed (the frame will not
/// open) rather than opening on the wrong key.
+ (LakkoSFrameAttachStatus)attachDecryptorToReceiver:
                              (RTCRtpReceiver * _Nullable)receiver
                                                  seed:(NSData *_Nullable)seed
                                             mediaType:(LakkoSFrameMediaType)mediaType
    NS_SWIFT_NAME(attachDecryptor(toReceiver:seed:mediaType:));

/// Mid-call rekey (G-18, ADR-0029) — rotate the encryptor attached to
/// `sender` to the new MLS epoch's 32-byte `seed`, keeping the SAME
/// local leaf. The encryptor seals only under the CURRENT epoch, so a
/// rekey is a straight key swap (no overlap on the send side — already-
/// sealed frames are already bytes in libwebrtc's pipeline).
///
/// Returns `YES` on success, and also `YES` when the sender was never
/// SFrame-armed (a benign no-op). Returns `NO` only when the encryptor
/// is present but the rotation failed (bad seed length / HKDF): the
/// bridge then RETAINS the current epoch's key, so media stays E2EE —
/// never plaintext (invariant #4). The caller keeps the call up and
/// logs; a rekey failure is an availability event, not a downgrade.
+ (BOOL)rekeyEncryptorOnSender:(RTCRtpSender * _Nullable)sender
                          seed:(NSData * _Nullable)seed
    NS_SWIFT_NAME(rekeyEncryptor(onSender:seed:));

/// Mid-call rekey (G-18, ADR-0029) — rotate the decryptor attached to
/// `receiver` to the new MLS epoch's 32-byte `seed`, retaining the
/// PREVIOUS key until `nowMs + clamp(overlapMs)` so in-flight frames
/// sealed under the old epoch still open. `pruneDecryptorOnReceiver:`
/// drops the retained key once the window elapses.
///
/// Same return contract as [+rekeyEncryptorOnSender:seed:]: `YES` on
/// success or a no-op (receiver never armed); `NO` only when a present
/// decryptor's rotation failed (the ring keeps its current + previous
/// keys — still E2EE).
+ (BOOL)rekeyDecryptorOnReceiver:(RTCRtpReceiver * _Nullable)receiver
                            seed:(NSData * _Nullable)seed
                           nowMs:(uint64_t)nowMs
                       overlapMs:(uint64_t)overlapMs
    NS_SWIFT_NAME(rekeyDecryptor(onReceiver:seed:nowMs:overlapMs:));

/// Mid-call rekey (G-18, ADR-0029) — close the overlap window: drop the
/// decryptor's retained previous key once it has expired at `nowMs`
/// (seed-boundary forward secrecy). No-op if the receiver was never
/// armed or the window has not yet elapsed.
+ (void)pruneDecryptorOnReceiver:(RTCRtpReceiver * _Nullable)receiver
                           nowMs:(uint64_t)nowMs
    NS_SWIFT_NAME(pruneDecryptor(onReceiver:nowMs:));

/// Drop the encryptor that was attached to `sender` by
/// [+attachEncryptorToSender:seed:localLeaf:]. No-op if the
/// sender was never attached.
+ (void)detachFromSender:(RTCRtpSender * _Nullable)sender
    NS_SWIFT_NAME(detach(fromSender:));

/// Drop the decryptor that was attached to `receiver` by
/// [+attachDecryptorToReceiver:seed:].
+ (void)detachFromReceiver:(RTCRtpReceiver * _Nullable)receiver
    NS_SWIFT_NAME(detach(fromReceiver:));

@end

NS_ASSUME_NONNULL_END

#endif  // LAKKO_SFRAME_ATTACH_H_

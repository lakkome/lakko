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

/// The codec a sender negotiated, as far as the sealed-frame layout
/// cares (ADR-0071). It decides how many leading bytes of each encoded
/// frame stay in the clear so that a receiver without the generic frame
/// descriptor still finds key frames: 10 of a VP8 key frame, 3 of any
/// other VP8 frame, none for Opus and for every other codec.
///
/// The raw values are the `LAKKO_SFRAME_MEDIA_CODEC_*` constants of the
/// `lakko-sframe-native` C ABI (`SFrameAttach.mm` asserts the match at
/// compile time).
typedef NS_ENUM(uint8_t, LakkoSFrameMediaCodec) {
    /// Any codec without a rule (H.264, VP9, AV1, the RED / RTX / FEC
    /// wrappers), and a codec that is not known yet. Nothing is left
    /// clear. This is what an encryptor uses until it is told otherwise.
    LakkoSFrameMediaCodecOther = 0,
    /// VP8 video.
    LakkoSFrameMediaCodecVP8 = 1,
    /// Opus audio. Nothing is left clear.
    LakkoSFrameMediaCodecOpus = 2,
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
/// `streamIndex` is the local send track's per-sender stream
/// ordinal (ADR-0035) — 0 = audio, 1 = first video, 2 = second
/// video / screen-share, … The seal KID is composed
/// `(localLeaf << 8) | streamIndex` and folded whole into the key
/// derivation, so every one of a participant's concurrent streams
/// keys into a disjoint `(key, nonce_salt)` space and a frame
/// counter (nonce) reused across two streams of one leaf no longer
/// collides. The encryptor is bound to this one stream for its
/// whole lifetime. Every `uint8_t` is a valid stream index — there
/// is no fail-closed rejection on this argument.
///
/// The fresh encryptor seals a VIDEO sender's frames as
/// `LakkoSFrameMediaCodecOther` (nothing in the clear) until
/// [+setCodec:forSender:] names the negotiated codec; an AUDIO sender
/// needs no such call. Attaching again replaces the encryptor, so the
/// codec has to be set again after a re-attach.
///
/// Returns `LakkoSFrameAttachStatusOK` on success.
+ (LakkoSFrameAttachStatus)attachEncryptorToSender:
                              (RTCRtpSender * _Nullable)sender
                                                seed:(NSData *_Nullable)seed
                                           localLeaf:(uint32_t)localLeaf
                                         streamIndex:(uint8_t)streamIndex
    NS_SWIFT_NAME(attachEncryptor(toSender:seed:localLeaf:streamIndex:));

/// Build a fresh `LakkoFrameDecryptor` for `seed`, then attach
/// it to `receiver` via the non-public `setFrameDecryptor:`
/// selector. Per-sender ParticipantKey derivation happens lazily
/// inside the decryptor as frames arrive (see
/// `apps/shared/sframe-bridge/sframe-bridge.cc`'s
/// `lookup_or_derive`).
///
/// The decryptor takes NO stream argument (ADR-0035): a single
/// decryptor bound to one remote receive track opens every frame
/// on it by reading the composite KID
/// (`(senderLeaf << 8) | streamIndex`) from each frame's own
/// SFrame header — the per-stream discriminator that survives the
/// SFU — and derives that sender+stream's key internally, so audio
/// and either video stream are each keyed correctly with no local
/// media-type or MID signal.
+ (LakkoSFrameAttachStatus)attachDecryptorToReceiver:
                              (RTCRtpReceiver * _Nullable)receiver
                                                  seed:(NSData *_Nullable)seed
    NS_SWIFT_NAME(attachDecryptor(toReceiver:seed:));

/// Tell the encryptor attached to `sender` which codec the sender
/// negotiated (ADR-0071). libwebrtc hands the encryptor a media type
/// and no codec, so the Swift side reads it from the sender (the first
/// entry of `sender.parameters.codecs` once the answer is applied) and
/// calls this after every negotiation that could change it. A rekey
/// keeps the codec; a re-attach does not.
///
/// Until this is called a video encryptor seals with nothing in the
/// clear: the frames still open, but a receiver that has no generic
/// frame descriptor (Firefox, Safari) cannot find key frames in them.
/// Audio frames are sealed with nothing in the clear whatever is set.
///
/// Safe to call from any thread while media is flowing.
///
/// Returns `YES` when an encryptor was attached to `sender` and now
/// holds `codec`; `NO` when `sender` is nil or carries no encryptor
/// (nothing was changed).
+ (BOOL)setCodec:(LakkoSFrameMediaCodec)codec
       forSender:(RTCRtpSender * _Nullable)sender
    NS_SWIFT_NAME(setCodec(_:forSender:));

/// The same as [+setCodec:forSender:], taking the codec by name: the
/// `name` of an `RTCRtpCodecParameters` ("VP8", "opus") or the MIME
/// form ("video/VP8", "audio/opus"), compared without regard to case.
/// Any other name, and nil, is `LakkoSFrameMediaCodecOther`. The
/// mapping is the bridge's (`LakkoFrameEncryptor::CodecFromName`), the
/// one Android uses too.
+ (BOOL)setCodecName:(NSString * _Nullable)codecName
           forSender:(RTCRtpSender * _Nullable)sender
    NS_SWIFT_NAME(setCodecName(_:forSender:));

/// The codec [+setCodecName:forSender:] would set for `codecName`.
+ (LakkoSFrameMediaCodec)codecForName:(NSString * _Nullable)codecName
    NS_SWIFT_NAME(codec(forName:));

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

// MARK: - A receiver's minimum playout delay (ADR-0070, note of 2026-10-12)

/// What a set of a receiver's minimum playout delay says. The values are
/// `lakko::receiver_delay::Status` verbatim.
typedef NS_ENUM(NSInteger, LakkoReceiverDelayStatus) {
    /// The delay was handed to the engine.
    LakkoReceiverDelayStatusOK = 0,
    /// Nothing was called: the setter is not available in this process
    /// (see [+isAvailable]), the receiver is nil or carries no track of a
    /// known kind, or it has no Lakko decryptor to run the probe against.
    LakkoReceiverDelayStatusNotAvailable = 1,
    /// The engine's object did not answer the two probe questions as the
    /// evidence says it must. Nothing is set for the rest of the process.
    LakkoReceiverDelayStatusProbeFailed = 2,
};

/// Sets a remote receiver's minimum playout delay through the engine's
/// C++ `webrtc::RtpReceiverInterface::SetJitterBufferMinimumDelay`, which
/// the framework's Objective-C surface does not wrap.
///
/// The engine object is reached through the framework's non-public
/// `nativeRtpReceiver` accessor and called by vtable slot
/// (`apps/shared/sframe-bridge/receiver-delay.h`, where the evidence for
/// the slot and the calling convention is written down). That evidence
/// was read from the binary of stasel/WebRTC 125.0.0, so the setter is
/// offered only when the loaded WebRTC image IS that binary, by its
/// Mach-O `LC_UUID`; for any other image the answer is "not available"
/// and Lakko measures without setting.
@interface LakkoReceiverDelay : NSObject

/// The WebRTC package version the evidence was gathered for.
@property(class, nonatomic, readonly) NSString *verifiedPackageVersion;

/// The `LC_UUID` of every slice of that package the evidence covers
/// (device arm64, simulator arm64, simulator x86_64), as `NSUUID`
/// strings.
@property(class, nonatomic, readonly) NSArray<NSString *> *verifiedEngineIdentities;

/// The `LC_UUID` of the WebRTC image loaded in this process, or nil when
/// it cannot be read (no `RTCRtpReceiver` class, no `nativeRtpReceiver`
/// accessor, or an image without the load command).
@property(class, nonatomic, readonly, nullable) NSString *loadedEngineIdentity;

/// `YES` when a set can be attempted in this process: a 64-bit arm or
/// x86 build, `RTCRtpReceiver` has the `nativeRtpReceiver` accessor with
/// the type the evidence was read for, the loaded image is one of
/// [+verifiedEngineIdentities], and the probe has not failed. The static
/// part is decided once; a failed probe turns the answer to `NO` for the
/// rest of the process.
+ (BOOL)isAvailable NS_SWIFT_NAME(isAvailable());

/// Set the minimum playout delay of `receiver` to `delayMs` milliseconds;
/// 0 gives the delay back to the engine. The receiver must carry the
/// decryptor [LakkoSFrameAttach attachDecryptorToReceiver:seed:]
/// attached, at least for the first set of the process: the probe asks
/// the engine for it and compares.
///
/// Blocks until the engine's worker thread has run the call. Never call
/// it on the main thread; use the queue the peer connection's other
/// blocking calls run on.
+ (LakkoReceiverDelayStatus)setMinimumPlayoutDelayMs:(uint32_t)delayMs
                                         forReceiver:(RTCRtpReceiver * _Nullable)receiver
    NS_SWIFT_NAME(setMinimumPlayoutDelayMs(_:forReceiver:));

@end

NS_ASSUME_NONNULL_END

#endif  // LAKKO_SFRAME_ATTACH_H_

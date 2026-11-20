// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors
//
// Lakko's capture noise suppressor on iOS (ADR-0088, `docs/ux/calls.md`
// section 4.1.3): RNNoise on the microphone after the platform's voice
// processing and before the encoder, so before SFrame.
//
// ## Where it runs
//
// libwebrtc's capture post-processing stage. The process-wide
// `RTCPeerConnectionFactory` (`WebRTCPeerConnection.sharedFactory`) is built
// with an `RTCDefaultAudioProcessingModule` whose capture post-processing
// delegate is the one `LakkoCaptureNoiseProcessor`. In the pinned
// webrtc-sdk 125.6422.07 that stage is a `webrtc::CustomProcessing` the
// audio processing module runs on its full-band capture buffer after echo
// cancellation, noise suppression and gain control; the SDK's adapter
// (`RTCAudioCustomProcessingAdapter.mm`, m125_release) calls
// `audioProcessingProcess:` under its own `os_unfair_lock` (taken with
// `trylock`), on libwebrtc's audio thread, 10 ms at a time. The samples are
// `float` in the 16-bit range (`webrtc::AudioBuffer::channels()`), which is
// what `lakko_noise_process_s16_in_place` takes. On iOS the voice-processing
// I/O unit (the platform's echo canceller) runs before that, in the audio
// unit itself.
//
// ## Sample rate
//
// The model runs at 48 kHz mono. The native side also takes a mono stream
// at 24, 16, 12 or 8 kHz (`lakko_noise_supports_rate`): it raises each
// 10 ms buffer to 48 kHz, cleans it and brings it back down
// (`lakko_noise_process_s16_in_place_at_rate`), so a capture stage at one of
// those rates is cleaned too (the iOS Simulator captures at 16 kHz). A stage
// at any other rate, or with more than one channel, is passed through
// untouched, and the format line says so.
//
// ## Threads
//
// `audioProcessingInitialize...` / `audioProcessingProcess:` /
// `audioProcessingRelease` are libwebrtc's, serialised by the SDK adapter's
// lock. `arm...` / `disarm...` / `setEnabled:` are the app's, serialised by
// a lock the audio thread never takes. They share only atomics. The process
// path allocates nothing of its own, takes no lock and never throws; only
// its first failure logs, once.
//
// A fresh native suppressor is made for every call and every switch-on, so
// no audio held in the suppressor's 10 ms delay from an earlier call reaches
// a new one. A suppressor is freed only after the audio thread has been seen
// outside the process path once the handle was taken away; if that does not
// happen within the wait, the handle is left allocated rather than freed
// under the audio thread.
//
// ## Failure
//
// Anything that goes wrong leaves the call on the platform's processing, as
// with the switch off, and logs one line: no suppressor can be made, or the
// native side refuses a buffer (the buffer is then untouched and the rest of
// the call passes through).

#import <Foundation/Foundation.h>
#import <WebRTC/RTCAudioCustomProcessingDelegate.h>

NS_ASSUME_NONNULL_BEGIN

/// What `LakkoNoiseEngine.process` returned when it cleaned the samples.
/// Equal to `LAKKO_NOISE_STATUS_T_OK` of the C ABI.
FOUNDATION_EXPORT const int32_t LakkoNoiseEngineStatusOK;

/// The native suppressor, as `LakkoCaptureNoiseProcessor` uses it. The
/// production engine is `LakkoNativeNoiseEngine`; tests supply a fake.
@protocol LakkoNoiseEngine <NSObject>

/// 48 000, or 0 when there is no native suppressor.
@property(nonatomic, readonly) NSInteger sampleRate;

/// A fresh suppressor handle, or NULL. Control thread.
- (nullable void *)createSuppressor;

/// Frees `handle`; NULL is a no-op. Control thread.
- (void)destroySuppressor:(nullable void *)handle;

/// Cleans `frames` floats at `samples` in place, a 48 kHz mono stream.
/// Returns `LakkoNoiseEngineStatusOK` on success; nothing is touched
/// otherwise. Audio thread; must not allocate.
- (int32_t)processSuppressor:(void *)handle
                     samples:(float *)samples
                      frames:(size_t)frames;

/// Whether `processSuppressor:rate:samples:frames:` takes a mono stream at
/// `rateHz`. Never YES when there is no native suppressor.
- (BOOL)supportsRate:(NSInteger)rateHz;

/// As `processSuppressor:samples:frames:` for a mono stream at `rateHz`;
/// the rate may change between calls. Audio thread; must not allocate.
- (int32_t)processSuppressor:(void *)handle
                        rate:(NSInteger)rateHz
                     samples:(float *)samples
                      frames:(size_t)frames;

@end

/// RNNoise through `lakko_noise_*` of `liblakko_sframe_native.a`.
@interface LakkoNativeNoiseEngine : NSObject <LakkoNoiseEngine>
@end

/// The capture post-processing stage: cleans the microphone when the switch
/// is on and the stage runs mono at a rate the engine supports, else leaves
/// the buffer as it is.
@interface LakkoCaptureNoiseProcessor : NSObject <RTCAudioCustomProcessingDelegate>

/// `log` receives one plain-ASCII line at a time, with no identifier.
/// `pause` is called between waits for the audio thread to leave the
/// process path (a short sleep in production).
- (instancetype)initWithEngine:(id<LakkoNoiseEngine>)engine
                           log:(void (^)(NSString *line))log
                         pause:(void (^)(void))pause NS_DESIGNATED_INITIALIZER;

/// `pause` is a 1 ms sleep.
- (instancetype)initWithEngine:(id<LakkoNoiseEngine>)engine
                           log:(void (^)(NSString *line))log;

- (instancetype)init NS_UNAVAILABLE;

/// YES while an armed call has the suppressor running.
@property(nonatomic, readonly) BOOL running;

/// Buffers cleaned / passed through since the last arm (diagnostics, tests).
@property(nonatomic, readonly) int64_t cleanedCount;
@property(nonatomic, readonly) int64_t passedCount;

/// A call's microphone starts: a fresh suppressor when `on`, the counters
/// cleared, and one line saying which processing applies. `owner` is only
/// compared by identity (never retained); a later arm takes over.
- (void)armForOwner:(id)owner on:(BOOL)on platform:(NSString *)platform;

/// `owner`'s call ended: the suppressor freed, a summary logged. A no-op
/// when another owner has armed since.
- (void)disarmForOwner:(id)owner;

/// The switch changed: applies to the running call at once.
- (void)setEnabled:(BOOL)on;

/// The process path the delegate method runs, on one channel's samples.
/// Exposed so tests can drive it without a libwebrtc audio buffer.
- (void)processSamples:(nullable float *)samples
                frames:(size_t)frames
              channels:(size_t)channels;

@end

NS_ASSUME_NONNULL_END

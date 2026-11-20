// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

#import "CaptureNoiseProcessor.h"

#import <WebRTC/RTCAudioBuffer.h>

#include <unistd.h>

#include <atomic>
#include <cstdint>
#include <mutex>

#include "../../../../target/lakko-include/lakko_sframe_native.h"

const int32_t LakkoNoiseEngineStatusOK = LAKKO_NOISE_STATUS_T_OK;

namespace {

// Waits for the audio thread to leave the process path before a replaced
// suppressor is freed (see `swapIn:`).
constexpr int kMaxWaits = 200;

}  // namespace

@implementation LakkoNativeNoiseEngine

- (NSInteger)sampleRate {
    return LAKKO_NOISE_SAMPLE_RATE;
}

- (void *)createSuppressor {
    return lakko_noise_new();
}

- (void)destroySuppressor:(void *)handle {
    lakko_noise_free(static_cast<lakko_noise_suppressor_t *>(handle));
}

- (int32_t)processSuppressor:(void *)handle
                     samples:(float *)samples
                      frames:(size_t)frames {
    return static_cast<int32_t>(lakko_noise_process_s16_in_place(
        static_cast<lakko_noise_suppressor_t *>(handle), samples, frames));
}

- (BOOL)supportsRate:(NSInteger)rateHz {
    if (rateHz <= 0 || rateHz > static_cast<NSInteger>(UINT32_MAX)) {
        return NO;
    }
    return lakko_noise_supports_rate(static_cast<uint32_t>(rateHz)) ? YES : NO;
}

- (int32_t)processSuppressor:(void *)handle
                        rate:(NSInteger)rateHz
                     samples:(float *)samples
                      frames:(size_t)frames {
    if (rateHz <= 0 || rateHz > static_cast<NSInteger>(UINT32_MAX)) {
        return static_cast<int32_t>(LAKKO_NOISE_STATUS_T_UNSUPPORTED_RATE);
    }
    return static_cast<int32_t>(lakko_noise_process_s16_in_place_at_rate(
        static_cast<lakko_noise_suppressor_t *>(handle), static_cast<uint32_t>(rateHz),
        samples, frames));
}

@end

@implementation LakkoCaptureNoiseProcessor {
    id<LakkoNoiseEngine> _engine;
    void (^_log)(NSString *line);
    void (^_pause)(void);
    // The engine's model rate, read once: it is a constant of the engine
    // (0 when there is no native suppressor).
    NSInteger _engineRate;

    // Shared between the audio thread and the control thread.
    std::atomic<void *> _handle;
    std::atomic<int> _inProcess;
    std::atomic<bool> _enabled;
    std::atomic<bool> _failed;
    std::atomic<bool> _failureLogged;
    std::atomic<int64_t> _cleaned;
    std::atomic<int64_t> _passed;
    std::atomic<int> _rateHz;
    std::atomic<int> _channels;
    // Whether the engine takes `_rateHz`, decided when the format is
    // reported so the process path asks nothing of the engine.
    std::atomic<bool> _rateSupported;

    // Control thread only, under `_control`.
    std::mutex _control;
    const void *_owner;

    // libwebrtc callbacks only (serialised by the SDK adapter's lock).
    int _formatLoggedRate;
    int _formatLoggedChannels;
}

- (instancetype)initWithEngine:(id<LakkoNoiseEngine>)engine
                           log:(void (^)(NSString *line))log
                         pause:(void (^)(void))pause {
    if (self = [super init]) {
        _engine = engine;
        _log = [log copy];
        _pause = [pause copy];
        _engineRate = engine.sampleRate;
        _handle.store(nullptr);
        _inProcess.store(0);
        _enabled.store(false);
        _failed.store(false);
        _failureLogged.store(false);
        _cleaned.store(0);
        _passed.store(0);
        _rateHz.store(0);
        _channels.store(0);
        _rateSupported.store(false);
        _owner = nullptr;
        _formatLoggedRate = -1;
        _formatLoggedChannels = -1;
    }
    return self;
}

- (instancetype)initWithEngine:(id<LakkoNoiseEngine>)engine
                           log:(void (^)(NSString *line))log {
    return [self initWithEngine:engine
                            log:log
                          pause:^{
                              usleep(1000);
                          }];
}

- (BOOL)running {
    return _enabled.load() && !_failed.load() && _handle.load() != nullptr;
}

- (int64_t)cleanedCount {
    return _cleaned.load();
}

- (int64_t)passedCount {
    return _passed.load();
}

// MARK: - libwebrtc side

- (void)audioProcessingInitializeWithSampleRate:(size_t)sampleRateHz
                                       channels:(size_t)channels {
    const BOOL supported =
        _engineRate != 0 && sampleRateHz <= static_cast<size_t>(INT32_MAX) &&
        [_engine supportsRate:static_cast<NSInteger>(sampleRateHz)];
    // This and `audioProcessingProcess:` are serialised by the SDK adapter's
    // lock, so the process path never pairs one rate with another's answer.
    _rateSupported.store(supported);
    _rateHz.store(static_cast<int>(sampleRateHz));
    _channels.store(static_cast<int>(channels));
    [self logFormat];
}

- (void)audioProcessingProcess:(RTCAudioBuffer *)audioBuffer {
    const size_t channels = audioBuffer.channels;
    float *samples = channels > 0 ? [audioBuffer rawBufferForChannel:0] : nullptr;
    [self processSamples:samples frames:audioBuffer.frames channels:channels];
}

- (void)audioProcessingRelease {
    // Nothing is held for the stage itself: the suppressor belongs to the
    // armed call and is freed by `disarmForOwner:`.
}

- (void)processSamples:(float *)samples frames:(size_t)frames channels:(size_t)channels {
    const int rate = _rateHz.load();
    if (!_enabled.load() || _failed.load() || !_rateSupported.load() || rate <= 0 ||
        channels != 1 || samples == nullptr ||
        static_cast<int64_t>(frames) * 100 != static_cast<int64_t>(rate)) {
        _passed.fetch_add(1);
        return;
    }
    _inProcess.fetch_add(1);
    void *handle = _handle.load();
    if (handle == nullptr) {
        _passed.fetch_add(1);
        _inProcess.fetch_sub(1);
        return;
    }
    const int32_t status = [_engine processSuppressor:handle
                                                 rate:rate
                                              samples:samples
                                               frames:frames];
    if (status == LakkoNoiseEngineStatusOK) {
        _cleaned.fetch_add(1);
    } else {
        _failed.store(true);
        _passed.fetch_add(1);
        if (!_failureLogged.exchange(true)) {
            _log([NSString stringWithFormat:@"noise suppression failed (native status %d); "
                                            @"platform processing only for the rest of the call",
                                            status]);
        }
    }
    _inProcess.fetch_sub(1);
}

// MARK: - App side

- (void)armForOwner:(id)owner on:(BOOL)on platform:(NSString *)platform {
    std::lock_guard<std::mutex> guard(_control);
    _owner = (__bridge const void *)owner;
    _failed.store(false);
    _failureLogged.store(false);
    _cleaned.store(0);
    _passed.store(0);
    _enabled.store(false);
    void *fresh = on ? [_engine createSuppressor] : nullptr;
    [self swapIn:fresh];
    _enabled.store(fresh != nullptr);
    _log([NSString stringWithFormat:@"call audio: %@; %@", [self describeOn:on fresh:fresh],
                                    platform]);
}

- (void)disarmForOwner:(id)owner {
    std::lock_guard<std::mutex> guard(_control);
    if (_owner != (__bridge const void *)owner) {
        return;
    }
    _owner = nullptr;
    _enabled.store(false);
    [self swapIn:nullptr];
    _log([NSString stringWithFormat:@"call audio ended: %lld buffers cleaned by RNNoise, "
                                    @"%lld passed through",
                                    static_cast<long long>(_cleaned.load()),
                                    static_cast<long long>(_passed.load())]);
}

- (void)setEnabled:(BOOL)on {
    std::lock_guard<std::mutex> guard(_control);
    if (_owner == nullptr) {
        return;
    }
    if (on && !_enabled.load()) {
        _failed.store(false);
        _failureLogged.store(false);
        void *fresh = [_engine createSuppressor];
        [self swapIn:fresh];
        _enabled.store(fresh != nullptr);
        _log([NSString stringWithFormat:@"noise suppression switched on: %@",
                                        [self describeOn:YES fresh:fresh]]);
    } else if (!on && _enabled.load()) {
        _enabled.store(false);
        [self swapIn:nullptr];
        _log(@"noise suppression switched off: platform processing only");
    }
}

// MARK: - Helpers

- (NSString *)describeOn:(BOOL)on fresh:(void *)fresh {
    if (!on) {
        return @"noise suppression off, platform processing only";
    }
    if (fresh == nullptr) {
        return @"noise suppression on but unavailable (no native suppressor), platform "
               @"processing only";
    }
    return [@"RNNoise noise suppression on after platform processing"
        stringByAppendingString:[self formatNote]];
}

- (NSString *)formatNote {
    const int rate = _rateHz.load();
    const int channels = _channels.load();
    if (rate == 0) {
        return @" (capture format not reported yet)";
    }
    NSString *reason = [self bypassReasonForChannels:channels];
    if (reason != nil) {
        return [NSString stringWithFormat:@" but bypassed: capture runs at %d Hz, %d ch; %@",
                                          rate, channels, reason];
    }
    if (rate == _engineRate) {
        return [NSString stringWithFormat:@" (capture %d Hz mono)", rate];
    }
    return [NSString stringWithFormat:@" (capture %d Hz mono, raised to %ld Hz for RNNoise)", rate,
                                      static_cast<long>(_engineRate)];
}

/// Why the stage at the reported format is passed through, or nil when
/// RNNoise applies to it.
- (nullable NSString *)bypassReasonForChannels:(int)channels {
    const BOOL rateOK = _rateSupported.load();
    if (_engineRate == 0) {
        return @"no native suppressor";
    }
    if (!rateOK && channels != 1) {
        return @"RNNoise takes one channel, at a rate it can raise to 48000 Hz";
    }
    if (!rateOK) {
        return [NSString stringWithFormat:@"RNNoise runs at %ld Hz and cannot be raised to it "
                                          @"from this rate",
                                          static_cast<long>(_engineRate)];
    }
    if (channels != 1) {
        return @"RNNoise takes one channel only";
    }
    return nil;
}

- (void)logFormat {
    const int rate = _rateHz.load();
    const int channels = _channels.load();
    if (rate == _formatLoggedRate && channels == _formatLoggedChannels) {
        return;
    }
    _formatLoggedRate = rate;
    _formatLoggedChannels = channels;
    NSString *reason = [self bypassReasonForChannels:channels];
    NSString *note;
    if (reason != nil) {
        note = [NSString stringWithFormat:@"RNNoise bypassed (%@)", reason];
    } else if (rate == _engineRate) {
        note = @"RNNoise applies when the switch is on";
    } else {
        note = [NSString stringWithFormat:@"RNNoise applies when the switch is on, raised to %ld "
                                          @"Hz",
                                          static_cast<long>(_engineRate)];
    }
    _log([NSString stringWithFormat:@"capture post-processing format %d Hz, %d ch: %@", rate,
                                    channels, note]);
}

/// Puts `fresh` in place and frees what it replaced, once unused.
- (void)swapIn:(void *)fresh {
    void *old = _handle.exchange(fresh);
    if (old == nullptr) {
        return;
    }
    int waits = 0;
    while (_inProcess.load() != 0) {
        if (waits >= kMaxWaits) {
            _log([NSString stringWithFormat:@"noise suppressor still in use after %d waits; "
                                            @"left allocated",
                                            kMaxWaits]);
            return;
        }
        _pause();
        waits++;
    }
    [_engine destroySuppressor:old];
}

@end

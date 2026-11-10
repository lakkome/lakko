// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

#import "SFrameAttach.h"

#import <WebRTC/RTCRtpReceiver.h>
#import <WebRTC/RTCRtpSender.h>

#include <dlfcn.h>
#include <mach-o/loader.h>
#include <objc/message.h>
#include <objc/runtime.h>
#include <atomic>
#include <cstring>

// The N-3 C++ bridge layer. Its symbols come in via the static
// archive `liblakko_sframe_native.a` (built by cargo with target
// `aarch64-apple-ios` for device + `aarch64-apple-ios-sim` /
// `x86_64-apple-ios` for the simulator) — wiring lands in the
// N-4-build follow-up sub-sprint (cross-compile + Xcode "Link
// Binary With Libraries" entry).
#include "../../../shared/sframe-bridge/sframe-bridge.h"
#include "../../../shared/sframe-bridge/receiver-delay.h"
#include "api/scoped_refptr.h"

// The Swift-facing codec enum carries the C ABI's values verbatim, so the
// shim passes it straight through. A renumbering on either side fails here.
static_assert(LakkoSFrameMediaCodecOther == LAKKO_SFRAME_MEDIA_CODEC_OTHER,
              "LakkoSFrameMediaCodecOther must equal the C ABI's OTHER");
static_assert(LakkoSFrameMediaCodecVP8 == LAKKO_SFRAME_MEDIA_CODEC_VP8,
              "LakkoSFrameMediaCodecVP8 must equal the C ABI's VP8");
static_assert(LakkoSFrameMediaCodecOpus == LAKKO_SFRAME_MEDIA_CODEC_OPUS,
              "LakkoSFrameMediaCodecOpus must equal the C ABI's OPUS");

// MARK: - Selector probe

namespace {

// Non-public selectors libwebrtc M125's RTCRtpSender.mm /
// RTCRtpReceiver.mm declare against the C++ pointer type. We
// look them up by name; the Obj-C runtime returns nil for an
// unknown selector, so an evolved upstream surface refuses
// loudly rather than silently misbehaving.
NSString * const kSetFrameEncryptorSelectorName = @"setFrameEncryptor:";
NSString * const kSetFrameDecryptorSelectorName = @"setFrameDecryptor:";

// Atomic boot-probe result. `std::atomic<int>` rather than
// `dispatch_once_t` because we want a tri-state (unknown / yes /
// no) — the public surface returns BOOL, but logging the prior
// answer on repeated calls requires us to distinguish "we have
// not run the probe" from "we ran it and the answer is NO".
//
//   0  — probe has not run yet
//   1  — probe ran, selectors present, attach is allowed
//   2  — probe ran, selectors absent, attach MUST refuse
std::atomic<int> g_probe_result{0};

bool ClassHasInstanceMethod(Class klass, SEL selector) {
    if (klass == nil || selector == nullptr) {
        return false;
    }
    // `class_getInstanceMethod` walks the superclass chain;
    // `class_respondsToSelector` does the same and is the
    // documented surface for "would `-foo:` dispatch to a real
    // implementation". Either works; this is the canonical form.
    return class_respondsToSelector(klass, selector);
}

// Look up the per-instance encryptor / decryptor associated
// strong-retain so the LakkoFrameEncryptor / Decryptor stays
// alive as long as the (sender|receiver) object does. We use
// `objc_setAssociatedObject` with `OBJC_ASSOCIATION_RETAIN_NONATOMIC`
// so dealloc of the host releases our wrapper, and the wrapper's
// `dealloc` (in turn) calls `Release()` on the C++ pointer,
// dropping its refcount to 0 and firing `delete this`.
NSString * const kEncryptorAssociationKey = @"lakko.sframe.encryptor.wrapper";
NSString * const kDecryptorAssociationKey = @"lakko.sframe.decryptor.wrapper";

// objc_msgSend casts. We type-erase through `id` so Swift bridging
// never sees the C++ pointer — only the Obj-C++ side ever forms
// it. The upstream M125 declaration is:
//
//   - (void)setFrameEncryptor:
//        (rtc::scoped_refptr<webrtc::FrameEncryptorInterface>)encryptor;
//
// At the ABI level (arm64 + x86_64), `rtc::scoped_refptr<T>` is
// laid out as a single `T*` (it is a pointer-sized wrapper that
// implicit-converts both ways). objc_msgSend's prototype for "one
// pointer argument returning void" works for both arches because
// the System V / arm64-darwin ABIs pass the first object pointer
// argument in `x1` / `rsi` and the receiver in `x0` / `rdi` — the
// `void*` cast covers both.
//
// IMPORTANT: the `void *` is the ADDRESS OF A `rtc::scoped_refptr`,
// never the C++ object pointer. The framework declares both methods
// as taking `rtc::scoped_refptr<webrtc::Frame{En,De}cryptorInterface>`
// BY VALUE (the Obj-C type encoding in the binary is
// `{scoped_refptr<webrtc::FrameEncryptorInterface>=^{...}}`), and on
// arm64 a class that is non-trivial for the purposes of calls is
// passed indirectly: the caller materialises a temporary and passes
// a pointer to it. Passing the object pointer itself made the
// framework read our object's vptr as the FrameEncryptorInterface*
// and jump through whatever the first vtable slot pointed at
// (SIGBUS, "possible pointer authentication failure" in the first
// live group call). The callee copy-constructs from the temporary
// (AddRef), so our own AddRef before the call is the wrapper's
// retain and balances the framework's Release on detach.
using SetFrameEncryptorIMP = void (*)(id, SEL, void *);
using SetFrameDecryptorIMP = void (*)(id, SEL, void *);

}  // namespace

// MARK: - Obj-C wrapper for the C++ encryptor / decryptor.
//
// We pin the C++ pointer behind an NSObject so it can ride the
// `objc_setAssociatedObject` strong-retain path. The wrapper's
// `dealloc` is the matching `Release()` that pairs with the
// `AddRef()` we issued before handing the pointer to the
// framework.

@interface LakkoSFrameEncryptorWrapper : NSObject {
   @package
    lakko::sframe::LakkoFrameEncryptor *_cppEncryptor;
}
@end

@implementation LakkoSFrameEncryptorWrapper
- (void)dealloc {
    if (_cppEncryptor) {
        _cppEncryptor->Release();
        _cppEncryptor = nullptr;
    }
}
@end

@interface LakkoSFrameDecryptorWrapper : NSObject {
   @package
    lakko::sframe::LakkoFrameDecryptor *_cppDecryptor;
}
@end

@implementation LakkoSFrameDecryptorWrapper
- (void)dealloc {
    if (_cppDecryptor) {
        _cppDecryptor->Release();
        _cppDecryptor = nullptr;
    }
}
@end

// MARK: - Call keys

@interface LakkoSFrameCallKeys ()
- (instancetype)initWithKeys:(lakko::sframe::LakkoCallKeys *)keys;
- (lakko::sframe::LakkoCallKeys *)cppKeys;
@end

@implementation LakkoSFrameCallKeys {
    lakko::sframe::LakkoCallKeys *_keys;
}

+ (nullable instancetype)keysWithSeed:(NSData *_Nullable)seed {
    if (seed == nil || seed.length != 32) {
        return nil;
    }
    lakko::sframe::LakkoCallKeys *keys = lakko::sframe::LakkoCallKeys::Create(
        static_cast<const uint8_t *>(seed.bytes), static_cast<size_t>(seed.length));
    if (keys == nullptr) {
        return nil;
    }
    LakkoSFrameCallKeys *wrapper = [[LakkoSFrameCallKeys alloc] initWithKeys:keys];
    return wrapper;
}

- (instancetype)initWithKeys:(lakko::sframe::LakkoCallKeys *)keys {
    if ((self = [super init])) {
        // Refcount: 0 -> 1 (this wrapper's retain). Every transform made on
        // the keys takes its own.
        keys->AddRef();
        _keys = keys;
    }
    return self;
}

- (void)dealloc {
    if (_keys) {
        _keys->Release();
        _keys = nullptr;
    }
}

- (BOOL)rekeyWithSeed:(NSData *_Nullable)seed
                nowMs:(uint64_t)nowMs
            overlapMs:(uint64_t)overlapMs {
    if (seed == nil || seed.length != 32) {
        return NO;
    }
    const bool ok = _keys->Rekey(static_cast<const uint8_t *>(seed.bytes),
                                 static_cast<size_t>(seed.length), nowMs, overlapMs);
    if (!ok) {
        NSLog(@"[lakko.sframe] call keys Rekey failed — retaining the "
              @"current epoch key (media stays E2EE, invariant #4).");
    }
    return ok ? YES : NO;
}

- (void)pruneAtMs:(uint64_t)nowMs {
    _keys->Prune(nowMs);
}

- (uint64_t)generation {
    return _keys->Generation();
}

- (lakko::sframe::LakkoCallKeys *)cppKeys {
    return _keys;
}

@end

// MARK: - Public class

namespace {

// The bridge is shared with Android and vendors no libwebrtc logging header, so
// it cannot pick a logger of its own; it emits through a host-installed sink.
// This is iOS's (see `lakko::sframe::SetLogSink`). Plain ASCII, already
// formatted by the bridge, and it may arrive on a libwebrtc worker thread —
// `NSLog` is safe there.
void LakkoBridgeLogSink(const char *line) {
    if (line == nullptr) {
        return;
    }
    NSLog(@"%s", line);
}

}  // namespace

@implementation LakkoSFrameAttach

+ (BOOL)bootProbe {
    // Installed from the probe because the probe is what every attach path
    // already runs first, so the sink is in place before any decryptor exists
    // and there is no second lifecycle to get wrong. Idempotent: the last
    // writer wins and every writer writes the same pointer.
    lakko::sframe::SetLogSink(&LakkoBridgeLogSink);

    const int cached = g_probe_result.load(std::memory_order_acquire);
    if (cached == 1) {
        return YES;
    }
    if (cached == 2) {
        return NO;
    }

    Class senderClass = NSClassFromString(@"RTCRtpSender");
    if (senderClass == nil) {
        // Try the unmangled name — Apple's RTC_OBJC_TYPE macro
        // expands differently across stasel/WebRTC builds. The
        // milestone-stable path is the framework's own
        // `RTCRtpSender` symbol, which the framework's umbrella
        // header registers under both names.
        senderClass = NSClassFromString(@"RTCRtpSender");
    }
    Class receiverClass = NSClassFromString(@"RTCRtpReceiver");
    if (receiverClass == nil) {
        receiverClass = NSClassFromString(@"RTCRtpReceiver");
    }

    SEL encSel = NSSelectorFromString(kSetFrameEncryptorSelectorName);
    SEL decSel = NSSelectorFromString(kSetFrameDecryptorSelectorName);

    const bool encOk = ClassHasInstanceMethod(senderClass, encSel);
    const bool decOk = ClassHasInstanceMethod(receiverClass, decSel);

    const int result = (encOk && decOk) ? 1 : 2;
    g_probe_result.store(result, std::memory_order_release);

    if (result == 2) {
        NSLog(@"[lakko.sframe] bootProbe FAILED — setFrameEncryptor: "
              @"present=%d, setFrameDecryptor: present=%d. Calling "
              @"will be disabled per security invariant #4 (no "
              @"fallback to non-E2EE).",
              encOk ? 1 : 0, decOk ? 1 : 0);
    }
    return result == 1;
}

+ (LakkoSFrameAttachStatus)attachEncryptorToSender:
                                (RTCRtpSender * _Nullable)sender
                                                  keys:(LakkoSFrameCallKeys *_Nullable)keys
                                             localLeaf:(uint32_t)localLeaf
                                           streamIndex:(uint8_t)streamIndex {
    if (sender == nil || keys == nil) {
        return LakkoSFrameAttachStatusNullArgument;
    }
    if (![LakkoSFrameAttach bootProbe]) {
        return LakkoSFrameAttachStatusSelectorMissing;
    }

    lakko::sframe::LakkoFrameEncryptor *enc =
        lakko::sframe::LakkoFrameEncryptor::Create([keys cppKeys], localLeaf, streamIndex);
    if (enc == nullptr) {
        return LakkoSFrameAttachStatusBridgeCreateFailed;
    }
    // Refcount: 0 -> 1 (our retain).
    enc->AddRef();

    LakkoSFrameEncryptorWrapper *wrapper = [[LakkoSFrameEncryptorWrapper alloc] init];
    wrapper->_cppEncryptor = enc;

    // Drop any prior attach on this sender (the wrapper's dealloc
    // calls Release() to balance our AddRef).
    objc_setAssociatedObject(sender,
                             (__bridge const void *)kEncryptorAssociationKey,
                             nil,
                             OBJC_ASSOCIATION_RETAIN_NONATOMIC);

    // Hand the framework a pointer to a caller-owned scoped_refptr
    // temporary (the arm64 calling convention for a by-value
    // scoped_refptr argument, see SetFrameEncryptorIMP above).
    //
    // The framework's `setFrameEncryptor:` copy-constructs its own
    // scoped_refptr from the temporary (AddRef); the temporary
    // itself releases at the end of this scope, leaving the count
    // at 2: ours retains 1 for the lifetime of the wrapper. When the
    // sender drops the encryptor (call ends, framework releases
    // its scoped_refptr → count back to 1), our wrapper still
    // retains; when the sender object itself deallocs and our
    // associated-object retain dies, the wrapper's dealloc fires
    // its Release() and the C++ pointer is freed (count 1 -> 0,
    // `delete this`).
    SEL selector = NSSelectorFromString(kSetFrameEncryptorSelectorName);
    IMP imp = class_getMethodImplementation([sender class], selector);
    if (imp == nullptr) {
        // The probe said YES but we can't resolve IMP — race
        // against framework reload (unlikely on iOS) or class
        // posing. Refuse defensively. Under ARC, the local
        // `wrapper` strong reference dies when this method
        // returns; its dealloc fires `enc->Release()` and the
        // count drops back to 0.
        return LakkoSFrameAttachStatusSelectorMissing;
    }
    auto setEncryptor = reinterpret_cast<SetFrameEncryptorIMP>(imp);
    {
        rtc::scoped_refptr<webrtc::FrameEncryptorInterface> holder(enc);
        setEncryptor(sender, selector, static_cast<void *>(&holder));
    }

    objc_setAssociatedObject(sender,
                             (__bridge const void *)kEncryptorAssociationKey,
                             wrapper,
                             OBJC_ASSOCIATION_RETAIN_NONATOMIC);
    return LakkoSFrameAttachStatusOK;
}

+ (LakkoSFrameAttachStatus)attachDecryptorToReceiver:
                                (RTCRtpReceiver * _Nullable)receiver
                                                    keys:(LakkoSFrameCallKeys *_Nullable)keys {
    if (receiver == nil || keys == nil) {
        return LakkoSFrameAttachStatusNullArgument;
    }
    if (![LakkoSFrameAttach bootProbe]) {
        return LakkoSFrameAttachStatusSelectorMissing;
    }

    // Which track this decryptor serves, for the bridge's diagnostic lines.
    // libwebrtc already knows it; without it a failure line cannot say which of
    // a call's two decryptors produced it, which is exactly the ambiguity a
    // live call ran into.
    const char *label = "unknown";
    if (receiver.track != nil && receiver.track.kind != nil) {
        label = receiver.track.kind.UTF8String;
    }
    lakko::sframe::LakkoFrameDecryptor *dec =
        lakko::sframe::LakkoFrameDecryptor::Create([keys cppKeys], label);
    if (dec == nullptr) {
        return LakkoSFrameAttachStatusBridgeCreateFailed;
    }
    dec->AddRef();

    LakkoSFrameDecryptorWrapper *wrapper = [[LakkoSFrameDecryptorWrapper alloc] init];
    wrapper->_cppDecryptor = dec;

    objc_setAssociatedObject(receiver,
                             (__bridge const void *)kDecryptorAssociationKey,
                             nil,
                             OBJC_ASSOCIATION_RETAIN_NONATOMIC);

    SEL selector = NSSelectorFromString(kSetFrameDecryptorSelectorName);
    IMP imp = class_getMethodImplementation([receiver class], selector);
    if (imp == nullptr) {
        return LakkoSFrameAttachStatusSelectorMissing;
    }
    auto setDecryptor = reinterpret_cast<SetFrameDecryptorIMP>(imp);
    {
        // Same calling convention as the encryptor: the address of a
        // caller-owned scoped_refptr temporary, never the raw pointer.
        rtc::scoped_refptr<webrtc::FrameDecryptorInterface> holder(dec);
        setDecryptor(receiver, selector, static_cast<void *>(&holder));
    }

    objc_setAssociatedObject(receiver,
                             (__bridge const void *)kDecryptorAssociationKey,
                             wrapper,
                             OBJC_ASSOCIATION_RETAIN_NONATOMIC);
    return LakkoSFrameAttachStatusOK;
}

// MARK: - Negotiated codec (ADR-0071)

+ (BOOL)setCodec:(LakkoSFrameMediaCodec)codec
       forSender:(RTCRtpSender * _Nullable)sender {
    if (sender == nil) {
        return NO;
    }
    id obj = objc_getAssociatedObject(
        sender, (__bridge const void *)kEncryptorAssociationKey);
    LakkoSFrameEncryptorWrapper *wrapper = (LakkoSFrameEncryptorWrapper *)obj;
    if (wrapper == nil || wrapper->_cppEncryptor == nullptr) {
        return NO;
    }
    // An atomic store on the bridge side: safe against a concurrent
    // Encrypt on the codec thread.
    wrapper->_cppEncryptor->SetCodec(static_cast<uint8_t>(codec));
    return YES;
}

+ (BOOL)setCodecName:(NSString * _Nullable)codecName
           forSender:(RTCRtpSender * _Nullable)sender {
    return [LakkoSFrameAttach setCodec:[LakkoSFrameAttach codecForName:codecName]
                             forSender:sender];
}

+ (LakkoSFrameMediaCodec)codecForName:(NSString * _Nullable)codecName {
    const uint8_t codec = lakko::sframe::LakkoFrameEncryptor::CodecFromName(
        codecName != nil ? codecName.UTF8String : nullptr);
    switch (codec) {
        case LAKKO_SFRAME_MEDIA_CODEC_VP8:
            return LakkoSFrameMediaCodecVP8;
        case LAKKO_SFRAME_MEDIA_CODEC_OPUS:
            return LakkoSFrameMediaCodecOpus;
        default:
            return LakkoSFrameMediaCodecOther;
    }
}

// MARK: - Mid-call rekey (G-18, ADR-0029)

+ (void)detachFromSender:(RTCRtpSender * _Nullable)sender {
    if (sender == nil) {
        return;
    }
    // First clear the framework's reference — pass nil through
    // `setFrameEncryptor:`. The framework drops its scoped_refptr,
    // dropping the C++ refcount by 1.
    SEL selector = NSSelectorFromString(kSetFrameEncryptorSelectorName);
    IMP imp = class_getMethodImplementation([sender class], selector);
    if (imp != nullptr) {
        auto setEncryptor = reinterpret_cast<SetFrameEncryptorIMP>(imp);
        // An EMPTY scoped_refptr, passed by address like the attach
        // path: the framework reads the argument as a pointer to a
        // scoped_refptr, so a bare nullptr is dereferenced and crashes.
        rtc::scoped_refptr<webrtc::FrameEncryptorInterface> none;
        setEncryptor(sender, selector, static_cast<void *>(&none));
    }
    // Then drop our wrapper retain — wrapper's dealloc calls
    // Release() on the C++ pointer, taking the count to 0 and
    // freeing it.
    objc_setAssociatedObject(sender,
                             (__bridge const void *)kEncryptorAssociationKey,
                             nil,
                             OBJC_ASSOCIATION_RETAIN_NONATOMIC);
}

+ (void)detachFromReceiver:(RTCRtpReceiver * _Nullable)receiver {
    if (receiver == nil) {
        return;
    }
    SEL selector = NSSelectorFromString(kSetFrameDecryptorSelectorName);
    IMP imp = class_getMethodImplementation([receiver class], selector);
    if (imp != nullptr) {
        auto setDecryptor = reinterpret_cast<SetFrameDecryptorIMP>(imp);
        // An empty scoped_refptr by address; see detachFromSender.
        rtc::scoped_refptr<webrtc::FrameDecryptorInterface> none;
        setDecryptor(receiver, selector, static_cast<void *>(&none));
    }
    objc_setAssociatedObject(receiver,
                             (__bridge const void *)kDecryptorAssociationKey,
                             nil,
                             OBJC_ASSOCIATION_RETAIN_NONATOMIC);
}

@end

// MARK: - A receiver's minimum playout delay (ADR-0070, note of 2026-10-12)

static_assert(LakkoReceiverDelayStatusOK ==
                  static_cast<NSInteger>(lakko::receiver_delay::Status::kOk),
              "LakkoReceiverDelayStatusOK must equal the bridge's kOk");
static_assert(LakkoReceiverDelayStatusNotAvailable ==
                  static_cast<NSInteger>(
                      lakko::receiver_delay::Status::kNotAvailable),
              "LakkoReceiverDelayStatusNotAvailable must equal kNotAvailable");
static_assert(LakkoReceiverDelayStatusProbeFailed ==
                  static_cast<NSInteger>(
                      lakko::receiver_delay::Status::kProbeFailed),
              "LakkoReceiverDelayStatusProbeFailed must equal kProbeFailed");

namespace {

// The framework's non-public accessor of the engine object
// (sdk/objc/api/peerconnection/RTCRtpReceiver+Private.h):
//
//   @property(nonatomic, readonly)
//       rtc::scoped_refptr<webrtc::RtpReceiverInterface> nativeRtpReceiver;
NSString * const kNativeRtpReceiverSelectorName = @"nativeRtpReceiver";

// Its Objective-C type encoding in the binary the evidence was read from. A
// framework whose accessor has any other type is not called.
const char * const kNativeRtpReceiverTypeEncoding =
    "{scoped_refptr<webrtc::RtpReceiverInterface>=^{RtpReceiverInterface}}16@0:8";

// The accessor returns the scoped_refptr by value. It is a class that is not
// trivial for the purpose of calls, so it comes back in memory: through `x8`
// on arm64 and through a hidden first argument on x86_64, with the receiver
// and the selector after it (read from the method's code in all three
// slices: `ldr x0, [x0, #0x10]; str x0, [x8]` and `movq 0x10(%rsi), %rdi;
// movq %rdi, (%rbx)`). `ReturnedRef` is returned the same way, so the
// compiler builds the right call from this type on both. The callee has
// added a reference for the value it returns; it is given back with
// `ReleaseEngineRef`.
using NativeRtpReceiverIMP = lakko::receiver_delay::ReturnedRef (*)(id, SEL);

// The package version and the LC_UUID of each slice whose machine code the
// slot and the calling convention were read from (webrtc-sdk/Specs
// 125.6422.07, WebRTC.xcframework.zip, sha256
// 827cc2f508c341367c9b0e4f6def46e5f6834251082047d5d1da5bc8fd379263; read
// 2026-11-09, see `receiver-delay.h`). Per slice, the vtable address points
// found by type name and the functions at the slots the probe and the setter
// use:
//
//   device arm64:     proxy vtable 0x96acd0 (slot 1 0xfeb5c, slot 8 0x7e58c0,
//                     slot 13 0x7e5b14, slot 16 0x7e5d38); VideoRtpReceiver
//                     0x96b078 (slot 8 0x7534, slot 13 0x7eae4c);
//                     JitterBufferDelay::Set 0x38c76c
//   simulator arm64:  proxy 0x972cd0 (slot 1 0xfc2bc, slot 8 0x7ebfc0,
//                     slot 13 0x7ec214, slot 16 0x7ec438); VideoRtpReceiver
//                     0x973078 (slot 8 0x47d8, slot 13 0x7f14bc); Set 0x38ee5c
//   simulator x86_64: proxy 0xd98968 (slot 1 0x227e40, slot 8 0xbe9190,
//                     slot 13 0xbe9420, slot 16 0xbe9760); VideoRtpReceiver
//                     0xd98d10 (slot 8 0x4ba0, slot 13 0xbf0800); Set 0x6b1a20
//
// stasel/WebRTC 125.0.0, which iOS linked until 2026-11-09, had the same
// slots and convention; its identities are no longer listed because it is no
// longer the binary the app links.
NSString * const kVerifiedPackageVersion = @"125.6422.07";
NSString * const kVerifiedUuidDeviceArm64 = @"4C4C4476-5555-3144-A12E-D71FF8AA38BD";
NSString * const kVerifiedUuidSimulatorArm64 = @"4C4C44CD-5555-3144-A1A9-7A0C4DFC7DD7";
NSString * const kVerifiedUuidSimulatorX8664 = @"4C4C440C-5555-3144-A1E2-AEA9B0483609";

// 0: not decided. 1: the static conditions hold. 2: they do not.
std::atomic<int> g_receiver_delay_static{0};

Class ReceiverClass() {
    return NSClassFromString(@"RTCRtpReceiver");
}

// The accessor's implementation when the class has it with the expected
// type, else null.
IMP NativeRtpReceiverImp(Class receiverClass) {
    if (receiverClass == nil) {
        return nullptr;
    }
    SEL selector = NSSelectorFromString(kNativeRtpReceiverSelectorName);
    Method method = class_getInstanceMethod(receiverClass, selector);
    if (method == nullptr) {
        return nullptr;
    }
    const char *encoding = method_getTypeEncoding(method);
    if (encoding == nullptr ||
        std::strcmp(encoding, kNativeRtpReceiverTypeEncoding) != 0) {
        return nullptr;
    }
    return method_getImplementation(method);
}

// The LC_UUID of the image that holds `address`, or nil.
NSString *ImageUuidOf(const void *address) {
    Dl_info info;
    if (address == nullptr || dladdr(address, &info) == 0 ||
        info.dli_fbase == nullptr) {
        return nil;
    }
    const auto *header =
        static_cast<const struct mach_header_64 *>(info.dli_fbase);
    if (header->magic != MH_MAGIC_64) {
        return nil;
    }
    const auto *cursor = reinterpret_cast<const uint8_t *>(header + 1);
    const uint8_t *end = cursor + header->sizeofcmds;
    for (uint32_t i = 0; i < header->ncmds; ++i) {
        if (cursor + sizeof(struct load_command) > end) {
            return nil;
        }
        struct load_command command;
        std::memcpy(&command, cursor, sizeof(command));
        if (command.cmdsize < sizeof(struct load_command) ||
            cursor + command.cmdsize > end) {
            return nil;
        }
        if (command.cmd == LC_UUID &&
            command.cmdsize >= sizeof(struct uuid_command)) {
            struct uuid_command uuid;
            std::memcpy(&uuid, cursor, sizeof(uuid));
            return [[[NSUUID alloc] initWithUUIDBytes:uuid.uuid] UUIDString];
        }
        cursor += command.cmdsize;
    }
    return nil;
}

}  // namespace

@implementation LakkoReceiverDelay

+ (NSString *)verifiedPackageVersion {
    return kVerifiedPackageVersion;
}

+ (NSArray<NSString *> *)verifiedEngineIdentities {
    return @[
        kVerifiedUuidDeviceArm64, kVerifiedUuidSimulatorArm64,
        kVerifiedUuidSimulatorX8664
    ];
}

+ (NSString *)loadedEngineIdentity {
    IMP imp = NativeRtpReceiverImp(ReceiverClass());
    if (imp == nullptr) {
        return nil;
    }
    return ImageUuidOf(reinterpret_cast<const void *>(imp));
}

+ (BOOL)isAvailable {
    int decided = g_receiver_delay_static.load(std::memory_order_acquire);
    if (decided == 0) {
        NSString *loaded = nil;
        bool ok = lakko::receiver_delay::PlatformSupported();
        if (ok) {
            loaded = [LakkoReceiverDelay loadedEngineIdentity];
            ok = loaded != nil &&
                 [[LakkoReceiverDelay verifiedEngineIdentities]
                     containsObject:loaded];
        }
        decided = ok ? 1 : 2;
        g_receiver_delay_static.store(decided, std::memory_order_release);
        if (ok) {
            NSLog(@"[lakko.receiver-delay] setter available: WebRTC image %@ "
                  @"is a verified slice of %@",
                  loaded, kVerifiedPackageVersion);
        } else {
            NSLog(@"[lakko.receiver-delay] setter NOT available: WebRTC image "
                  @"%@ is not a verified slice of %@; the playout delay stays "
                  @"observe-only until the evidence is redone for this binary",
                  loaded ?: @"(unreadable)", kVerifiedPackageVersion);
        }
    }
    return decided == 1 && !lakko::receiver_delay::ProbeHasFailed();
}

+ (LakkoReceiverDelayStatus)setMinimumPlayoutDelayMs:(uint32_t)delayMs
                                         forReceiver:(RTCRtpReceiver * _Nullable)receiver {
    if (receiver == nil) {
        return LakkoReceiverDelayStatusNotAvailable;
    }
    if (lakko::receiver_delay::ProbeHasFailed()) {
        return LakkoReceiverDelayStatusProbeFailed;
    }
    Class receiverClass = ReceiverClass();
    if (![LakkoReceiverDelay isAvailable] ||
        ![receiver isKindOfClass:receiverClass]) {
        return LakkoReceiverDelayStatusNotAvailable;
    }
    IMP imp = NativeRtpReceiverImp(receiverClass);
    if (imp == nullptr) {
        return LakkoReceiverDelayStatusNotAvailable;
    }

    // What the track says this receiver is: the answer `media_type()` must
    // give.
    NSString *kind = receiver.track.kind;
    int expectedMediaType = -1;
    if ([kind isEqualToString:@"audio"]) {
        expectedMediaType = lakko::receiver_delay::kMediaTypeAudio;
    } else if ([kind isEqualToString:@"video"]) {
        expectedMediaType = lakko::receiver_delay::kMediaTypeVideo;
    } else {
        return LakkoReceiverDelayStatusNotAvailable;
    }

    // The decryptor Lakko attached through THIS `RTCRtpReceiver` object:
    // the answer `GetFrameDecryptor()` must give. The wrapper is held for
    // the call, so its reference on the C++ object is too.
    //
    // The framework can hand out more than one `RTCRtpReceiver` for one
    // engine receiver (a second `didAdd` for it, or a receiver read off a
    // transceiver). A decryptor armed through another of them leaves this
    // object's association stale: the engine then answers with that newer
    // decryptor, or with none after a detach. The bridge tells that apart
    // from an answer that is not Lakko's at all
    // (`LakkoFrameDecryptor::IsLive`): a stale association skips this
    // receiver ("not available") and leaves the probe open; only an answer
    // that is not a live decryptor of Lakko's stops the setter for the
    // process.
    id associated = objc_getAssociatedObject(
        receiver, (__bridge const void *)kDecryptorAssociationKey);
    LakkoSFrameDecryptorWrapper *wrapper =
        (LakkoSFrameDecryptorWrapper *)associated;
    webrtc::FrameDecryptorInterface *decryptor =
        wrapper != nil ? wrapper->_cppDecryptor : nullptr;

    SEL selector = NSSelectorFromString(kNativeRtpReceiverSelectorName);
    auto nativeRtpReceiver = reinterpret_cast<NativeRtpReceiverIMP>(imp);
    const lakko::receiver_delay::ReturnedRef native =
        nativeRtpReceiver(receiver, selector);
    if (native.ptr == nullptr) {
        return LakkoReceiverDelayStatusNotAvailable;
    }
    const lakko::receiver_delay::Status status =
        lakko::receiver_delay::SetMinimumPlayoutDelay(
            native.ptr, delayMs, expectedMediaType, decryptor,
            &lakko::sframe::LakkoFrameDecryptor::IsLive,
            &LakkoBridgeLogSink);
    // The accessor's reference.
    lakko::receiver_delay::ReleaseEngineRef(native.ptr);
    return static_cast<LakkoReceiverDelayStatus>(status);
}

@end

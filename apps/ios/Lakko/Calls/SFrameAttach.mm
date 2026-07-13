// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

#import "SFrameAttach.h"

#import <WebRTC/RTCRtpReceiver.h>
#import <WebRTC/RTCRtpSender.h>

#include <objc/message.h>
#include <objc/runtime.h>
#include <atomic>

// The N-3 C++ bridge layer. Its symbols come in via the static
// archive `liblakko_sframe_native.a` (built by cargo with target
// `aarch64-apple-ios` for device + `aarch64-apple-ios-sim` /
// `x86_64-apple-ios` for the simulator) — wiring lands in the
// N-4-build follow-up sub-sprint (cross-compile + Xcode "Link
// Binary With Libraries" entry).
#include "../../../shared/sframe-bridge/sframe-bridge.h"

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
// IMPORTANT: this trampoline ONLY hands ownership of one refcount
// to the framework. The framework's `setFrameEncryptor:`
// implementation copies the scoped_refptr (which AddRef's), so our
// AddRef before the call balances the framework's Release on
// detach.
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

// MARK: - Public class

@implementation LakkoSFrameAttach

+ (BOOL)bootProbe {
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
                                                  seed:(NSData *_Nullable)seed
                                             localLeaf:(uint32_t)localLeaf {
    if (sender == nil) {
        return LakkoSFrameAttachStatusNullArgument;
    }
    if (seed == nil || seed.length != 32) {
        return LakkoSFrameAttachStatusBadSeedLength;
    }
    if (![LakkoSFrameAttach bootProbe]) {
        return LakkoSFrameAttachStatusSelectorMissing;
    }

    lakko::sframe::LakkoFrameEncryptor *enc =
        lakko::sframe::LakkoFrameEncryptor::Create(
            static_cast<const uint8_t *>(seed.bytes),
            static_cast<size_t>(seed.length),
            localLeaf);
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

    // objc_msgSend: handing the C++ pointer through.
    //
    // The framework's `setFrameEncryptor:` copies the scoped_refptr
    // internally (which AddRef's again, taking the count to 2);
    // ours retains 1 for the lifetime of the wrapper. When the
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
    setEncryptor(sender, selector, static_cast<void *>(enc));

    objc_setAssociatedObject(sender,
                             (__bridge const void *)kEncryptorAssociationKey,
                             wrapper,
                             OBJC_ASSOCIATION_RETAIN_NONATOMIC);
    return LakkoSFrameAttachStatusOK;
}

+ (LakkoSFrameAttachStatus)attachDecryptorToReceiver:
                                (RTCRtpReceiver * _Nullable)receiver
                                                    seed:(NSData *_Nullable)seed {
    if (receiver == nil) {
        return LakkoSFrameAttachStatusNullArgument;
    }
    if (seed == nil || seed.length != 32) {
        return LakkoSFrameAttachStatusBadSeedLength;
    }
    if (![LakkoSFrameAttach bootProbe]) {
        return LakkoSFrameAttachStatusSelectorMissing;
    }

    lakko::sframe::LakkoFrameDecryptor *dec =
        lakko::sframe::LakkoFrameDecryptor::Create(
            static_cast<const uint8_t *>(seed.bytes),
            static_cast<size_t>(seed.length));
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
    setDecryptor(receiver, selector, static_cast<void *>(dec));

    objc_setAssociatedObject(receiver,
                             (__bridge const void *)kDecryptorAssociationKey,
                             wrapper,
                             OBJC_ASSOCIATION_RETAIN_NONATOMIC);
    return LakkoSFrameAttachStatusOK;
}

// MARK: - Mid-call rekey (G-18, ADR-0029)

+ (BOOL)rekeyEncryptorOnSender:(RTCRtpSender * _Nullable)sender
                          seed:(NSData * _Nullable)seed {
    if (sender == nil) {
        return NO;
    }
    if (seed == nil || seed.length != 32) {
        return NO;
    }
    // Read the encryptor we installed on this sender at attach time. A
    // sender that was never SFrame-armed carries no association — a
    // benign no-op, not a failure.
    id obj = objc_getAssociatedObject(
        sender, (__bridge const void *)kEncryptorAssociationKey);
    LakkoSFrameEncryptorWrapper *wrapper = (LakkoSFrameEncryptorWrapper *)obj;
    if (wrapper == nil || wrapper->_cppEncryptor == nullptr) {
        return YES;
    }
    // Rekey is thread-safe against a concurrent Encrypt on the codec
    // thread (the bridge guards the seal key with `seal_mutex_`).
    const bool ok = wrapper->_cppEncryptor->Rekey(
        static_cast<const uint8_t *>(seed.bytes),
        static_cast<std::size_t>(seed.length));
    if (!ok) {
        NSLog(@"[lakko.sframe] encryptor Rekey failed — retaining the "
              @"current epoch key (media stays E2EE, invariant #4).");
    }
    return ok ? YES : NO;
}

+ (BOOL)rekeyDecryptorOnReceiver:(RTCRtpReceiver * _Nullable)receiver
                            seed:(NSData * _Nullable)seed
                           nowMs:(uint64_t)nowMs
                       overlapMs:(uint64_t)overlapMs {
    if (receiver == nil) {
        return NO;
    }
    if (seed == nil || seed.length != 32) {
        return NO;
    }
    id obj = objc_getAssociatedObject(
        receiver, (__bridge const void *)kDecryptorAssociationKey);
    LakkoSFrameDecryptorWrapper *wrapper = (LakkoSFrameDecryptorWrapper *)obj;
    if (wrapper == nil || wrapper->_cppDecryptor == nullptr) {
        return YES;
    }
    // The ring rotates the call key and retains the previous one for the
    // overlap window; thread-safe (the Rust ring is `Mutex`-guarded).
    const bool ok = wrapper->_cppDecryptor->Rekey(
        static_cast<const uint8_t *>(seed.bytes),
        static_cast<std::size_t>(seed.length),
        static_cast<uint64_t>(nowMs),
        static_cast<uint64_t>(overlapMs));
    if (!ok) {
        NSLog(@"[lakko.sframe] decryptor Rekey failed — retaining the "
              @"current ring (media stays E2EE, invariant #4).");
    }
    return ok ? YES : NO;
}

+ (void)pruneDecryptorOnReceiver:(RTCRtpReceiver * _Nullable)receiver
                           nowMs:(uint64_t)nowMs {
    if (receiver == nil) {
        return;
    }
    id obj = objc_getAssociatedObject(
        receiver, (__bridge const void *)kDecryptorAssociationKey);
    LakkoSFrameDecryptorWrapper *wrapper = (LakkoSFrameDecryptorWrapper *)obj;
    if (wrapper == nil || wrapper->_cppDecryptor == nullptr) {
        return;
    }
    wrapper->_cppDecryptor->Prune(static_cast<uint64_t>(nowMs));
}

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
        setEncryptor(sender, selector, nullptr);
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
        setDecryptor(receiver, selector, nullptr);
    }
    objc_setAssociatedObject(receiver,
                             (__bridge const void *)kDecryptorAssociationKey,
                             nil,
                             OBJC_ASSOCIATION_RETAIN_NONATOMIC);
}

@end

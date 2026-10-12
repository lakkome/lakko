# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors
#
# Consumer-side ProGuard / R8 rules for lakko-sframe AAR
# (G3-sframe-native N-5).
#
# Downstream apps that include this AAR via
# `implementation(project(":lakko-sframe"))` automatically pick
# these rules up — they tell R8 to leave the JNI surface alone.

# JNI native-method symbols are resolved by name (the symbol
# table inside libsframe_jni.so encodes the full Java path).
# Renaming the host classes would break dlsym lookup.
-keep class me.lakko.sframe.LakkoFrameEncryptor { *; }
-keep class me.lakko.sframe.LakkoFrameDecryptor { *; }
-keep class me.lakko.sframe.SFrameAvailability { *; }
-keepclasseswithmembernames class me.lakko.sframe.** {
    native <methods>;
}

# libwebrtc's Java-side glue calls `getNativeFrameEncryptor()`
# via direct method lookup (not reflection-by-name, but the
# vtable still has to match the interface declaration). Keep
# the interface implementations intact.
-keep,allowoptimization class * implements org.webrtc.FrameEncryptor { *; }
-keep,allowoptimization class * implements org.webrtc.FrameDecryptor { *; }

# ReceiverPlayoutDelay (ADR-0070, note of 2026-10-12): its native methods
# are found by name, and the accessor it calls in the SDK's package is the
# SDK's package-private `RtpReceiver.getNativeRtpReceiver()`, which a
# shrinker must neither rename nor remove.
-keep class me.lakko.sframe.ReceiverPlayoutDelay { *; }
-keep class org.webrtc.LakkoRtpReceiverAccess { *; }
-keepclassmembers class org.webrtc.RtpReceiver {
    long getNativeRtpReceiver();
}

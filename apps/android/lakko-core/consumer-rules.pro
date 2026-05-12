# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors

# Phase 1.15b — ProGuard / R8 consumer rules for the
# `lakko-core` AAR. Applied to downstream apps (the upcoming
# `:app` module in 1.15c) when they shrink their release build.
#
# UniFFI's generated Kotlin calls into the native cdylib via
# JNA + Native.load(). R8 must not rename / strip:
#
#   1. The `uniffi.lakko_ffi_uniffi.*` Kotlin classes that wrap
#      the FFI surface — JNA looks them up by name at runtime
#      via reflection.
#   2. The JNA infrastructure classes themselves.
#   3. Anything that JNA's `Native.load` discovers via Structure
#      / Library subclassing.

# UniFFI's generated bindings — keep everything in the
# uniffi.* package. Phase 2's surface bumps may add more
# subpackages; the wildcard guards against that.
-keep class uniffi.** { *; }

# JNA core — required by every UniFFI-generated binding.
-keep class com.sun.jna.** { *; }
-keep class * implements com.sun.jna.** { *; }
-keepclassmembers class * extends com.sun.jna.Structure {
    *;
}
-keepclassmembers interface * extends com.sun.jna.Library {
    *;
}

# Native methods can't be renamed — they have to match the
# `extern "C"` symbol exports in the cdylib.
-keepclasseswithmembernames class * {
    native <methods>;
}

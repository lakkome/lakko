// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

/**
 * The ring window, for the host JVM suite only.
 *
 * ## This is not the number's home
 *
 * The ring window has exactly one home —
 * [uniffi.lakko_ffi_uniffi.callRingWindowMs], over
 * `crates/lakko-ui-rules/src/ring.rs` — because it is a value the two devices
 * in a call have to agree on: if they drift, one side files a missed call
 * while the other is still ringing. Production reads it from there and never
 * from a Kotlin literal.
 *
 * A JVM unit test cannot. It does not load `liblakko_ffi_uniffi.so`, so
 * calling the core from here would only prove the library is absent. Every
 * function that needs the window therefore takes it as a parameter, and this
 * is what the suite passes.
 *
 * ## What that means for what these tests prove
 *
 * They prove the ARITHMETIC around the window and this client's own
 * corrections to it, on whatever window they are handed. They deliberately do
 * NOT prove that the window is 45 seconds; that is asserted in
 * `crates/lakko-ui-rules/src/ring.rs`, including the bounds it has to stay
 * within, which fail the Rust build rather than a test. So this constant
 * drifting from the core would not make any of these tests lie — it would
 * only mean they exercised a different number.
 */
internal const val TEST_RING_WINDOW_MS: Long = 45_000L

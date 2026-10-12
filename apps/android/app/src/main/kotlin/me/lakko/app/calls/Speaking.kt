// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import uniffi.lakko_ffi_uniffi.SpeakingSnapshot
import uniffi.lakko_ffi_uniffi.SpeakingTracker

/**
 * Who is speaking in a group call (`docs/ux/group-call-grid.md` sections 5.2
 * and 7.3): whose ring is on and who holds the floor.
 *
 * The rule is not here. It is written once, for the three clients, in
 * `crates/lakko-ui-rules/src/speaking.rs`, and reaches this client over
 * UniFFI. What this file holds is the seam: the rule behind an interface, so
 * that [SpeakingMonitor] can be driven by a host test, which cannot load the
 * native library. No threshold, no ring or floor timing and no filter on this
 * device's own microphone may be written in Kotlin; this client reads a level,
 * runs the loop that reads it, and tells the rule what it read.
 */

/**
 * The rule for one call. Levels are the root mean square of decoded samples,
 * `0..1`; times are one clock in whole milliseconds, any origin.
 */
internal interface SpeakingRule {
    /** Name this device's own person (or none): they have a ring like anyone
     *  and never hold the floor. */
    fun setLocal(person: String?)

    /** One level reading for one person. `null` is "could not be read". */
    fun sample(person: String, level: Double?, nowMs: ULong)

    /** The person's microphone is off. */
    fun silence(person: String)

    /** Forget everyone who is not one of [people], the roster as it is now. */
    fun keepOnly(people: List<String>)

    /** Settle who holds the floor. [order] is the whole roster, in order. */
    fun decideFloor(order: List<String>, nowMs: ULong): SpeakingSnapshot

    /** What counts as sound for the person now (for the diagnostics line). */
    fun threshold(person: String): Double

    /** Free the rule. Nothing is asked of it afterwards. */
    fun close()
}

/**
 * Where a call gets its [SpeakingRule]: production passes
 * [SharedSpeakingRules], which is the binding and nothing else, and a host
 * test passes a fake.
 */
internal fun interface SpeakingRules {
    fun create(): SpeakingRule
}

/** The rule every client shares, over UniFFI. Never re-implemented here. */
internal object SharedSpeakingRules : SpeakingRules {
    override fun create(): SpeakingRule {
        val tracker = SpeakingTracker()
        return object : SpeakingRule {
            override fun setLocal(person: String?) = tracker.setLocal(person)

            override fun sample(person: String, level: Double?, nowMs: ULong) =
                tracker.sample(person, level, nowMs)

            override fun silence(person: String) = tracker.silence(person)

            override fun keepOnly(people: List<String>) = tracker.keepOnly(people)

            override fun decideFloor(order: List<String>, nowMs: ULong): SpeakingSnapshot =
                tracker.decideFloor(order, nowMs)

            override fun threshold(person: String): Double = tracker.threshold(person)

            override fun close() = tracker.close()
        }
    }
}

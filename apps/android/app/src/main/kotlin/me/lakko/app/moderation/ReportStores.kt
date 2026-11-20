// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.moderation

import android.content.Context
import kotlinx.serialization.Serializable
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.json.Json

/*
 * ADR-0098 section 2, UX section 2.3 — what this device keeps about reports.
 *
 * "Your reports" lives on this device only: never synced, never in the
 * backup, removed by Forget device. Syncing it would tie the reports
 * together. The moderator-list version is the highest this device has seen
 * (ADR-0098 section 3): a lower one is treated as unavailable.
 */

/** One report sent from this device, as the reporter saw it. */
@Serializable
public data class SentReport(
    val receiptId: String,
    /** The target's name as the reporter saw it. */
    val targetName: String,
    /** The reason's wire byte. */
    val reason: Int,
    val sentAtMs: Long,
)

/** The newest first, as the list shows them. */
public fun withSentReport(existing: List<SentReport>, report: SentReport): List<SentReport> =
    (listOf(report) + existing.filterNot { it.receiptId == report.receiptId })

/** Storage of [SentReport] rows, per account. */
public interface SentReportStore {
    public fun load(accountIdB64: String): List<SentReport>
    public fun save(accountIdB64: String, reports: List<SentReport>)
}

/** The device-only preferences store. */
public class SharedPrefsSentReportStore(context: Context) : SentReportStore {
    private val prefs = context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
    private val json = Json { ignoreUnknownKeys = true }
    private val serializer = ListSerializer(SentReport.serializer())

    override fun load(accountIdB64: String): List<SentReport> {
        val raw = prefs.getString(key(accountIdB64), null) ?: return emptyList()
        return try {
            json.decodeFromString(serializer, raw)
        } catch (_: Throwable) {
            emptyList()
        }
    }

    override fun save(accountIdB64: String, reports: List<SentReport>) {
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            prefs.edit().putString(key(accountIdB64), json.encodeToString(serializer, reports)).apply()
        }
    }

    /** Forget device / Delete account: this account's rows go. */
    public fun clearForAccount(accountIdB64: String) {
        prefs.edit().remove(key(accountIdB64)).commit()
    }

    private fun key(accountIdB64: String): String =
        "reports:" + me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(accountIdB64)

    public companion object {
        public const val PREFS: String = "lakko-sent-reports"
    }
}

/** The highest moderator-list version this device has verified. */
public class ModeratorListVersionStore(context: Context) : ModeratorListFloor {
    private val prefs = context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    override fun highest(): ULong? =
        if (prefs.contains(KEY)) prefs.getLong(KEY, 0L).toULong() else null

    /** Keep [version] when it is higher than what is held. */
    override fun record(version: ULong) {
        val held = highest()
        if (held == null || version > held) prefs.edit().putLong(KEY, version.toLong()).apply()
    }

    private companion object {
        const val PREFS = "lakko-moderator-list"
        const val KEY = "highest-version"
    }
}

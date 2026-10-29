// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0074 part 1 / `docs/ux/identity-recovery.md` 18.4 - every pure
 * decision of "Was it you?" and taking the account back: which wipe a
 * refused session leads to, what the recall record holds, the screen's state
 * machine, and the meaning of every server answer. No server, no native
 * core, no Keystore.
 */
class RecallTest {

    private val accountId = ByteArray(16) { (it + 1).toByte() }
    private val seed = ByteArray(32) { (0x40 + it).toByte() }
    private val day = 24L * 60 * 60 * 1000

    private class MemoryStorage : RecallBlobStorage {
        val blobs = linkedMapOf<String, ByteArray>()
        var failWrites = false
        var corruptReads = false

        override fun list(): List<String> = blobs.keys.toList()

        override fun read(accountIdHex: String): ByteArray? {
            if (corruptReads) throw IllegalStateException("the wrap key is gone")
            return blobs[accountIdHex]?.copyOf()
        }

        override fun write(accountIdHex: String, plaintext: ByteArray) {
            if (failWrites) throw java.io.IOException("disk full")
            blobs[accountIdHex] = plaintext.copyOf()
        }

        override fun delete(accountIdHex: String) {
            blobs.remove(accountIdHex)
        }
    }

    private fun record(username: String = "alice", learnedAtMs: Long = 1_000L) =
        RecallRecord(accountId.copyOf(), seed.copyOf(), username, learnedAtMs)

    private fun status(attempts: Int = 20, expiresAtMs: Long = 31 * day, changedAtMs: Long = 500L) =
        RecallStatus(anchorSeq = 3, expiresAtMs = expiresAtMs, changedAtMs = changedAtMs, attemptsRemaining = attempts)

    private fun ui(
        stage: RecallStage = RecallStage.Question,
        availability: RecallAvailability = RecallAvailability.Checking,
    ) = RecallUi(
        accountIdB64 = base64Encode(accountId),
        username = "alice",
        learnedAtMs = 1_000L,
        availability = availability,
        stage = stage,
    )

    private fun http(status: Int, code: String?, retryAfter: Long? = null) =
        AuthHttpError(status, "/account/recall/x", "{}", code = code, retryAfterSecs = retryAfter)

    private fun offline() = AuthNetworkError("/account/recall/x", java.io.IOException("offline"))

    // ---- which wipe ---------------------------------------------------------

    @Test
    fun `a reset elsewhere is the staged wipe only on a device that holds the keys`() {
        assertEquals(
            SupersededWipe.StagedKeepingRecall,
            supersededWipeFor(KeyResetSignOut.Kind.Reset, holdsAccountKeys = true),
        )
        // A device that never held the keys has nothing to prove itself with.
        assertEquals(SupersededWipe.Full, supersededWipeFor(KeyResetSignOut.Kind.Reset, holdsAccountKeys = false))
    }

    @Test
    fun `a removed device never keeps a recall record`() {
        for (holds in listOf(true, false)) {
            assertEquals(SupersededWipe.Full, supersededWipeFor(KeyResetSignOut.Kind.Removed, holds))
            assertEquals(SupersededWipe.Full, supersededWipeFor(KeyResetSignOut.Kind.Mismatch, holds))
        }
    }

    @Test
    fun `each refusal code leads to its own wipe, whatever order the server checks in`() {
        // `key-generation-superseded` -> staged (+ record); `device-revoked`
        // -> full, no record. Nothing reads one code as the other.
        val superseded = signOutKindFor(sessionRefusalOf(401, KEY_GENERATION_SUPERSEDED)!!)!!
        val revoked = signOutKindFor(sessionRefusalOf(401, DEVICE_REVOKED)!!)!!
        assertEquals(SupersededWipe.StagedKeepingRecall, supersededWipeFor(superseded, holdsAccountKeys = true))
        assertEquals(SupersededWipe.Full, supersededWipeFor(revoked, holdsAccountKeys = true))
        // The sign-in screen cases never reach a wipe at all.
        assertNull(signOutKindFor(sessionRefusalOf(401, REFRESH_INVALID)!!))
        assertNull(signOutKindFor(sessionRefusalOf(401, null)!!))
    }

    // ---- the record ---------------------------------------------------------

    @Test
    fun `the record holds the account id, the 32-byte signing seed, the name and the time, and nothing else`() {
        val bytes = RecallRecordCodec.encode(record())
        // version + id + seed + time + name length + name.
        assertEquals(1 + 16 + 32 + 8 + 2 + "alice".length, bytes.size)
        val back = RecallRecordCodec.decode(bytes)!!
        assertArrayEquals(accountId, back.accountId)
        assertArrayEquals(seed, back.oldAccountSeed)
        assertEquals("alice", back.username)
        assertEquals(1_000L, back.learnedAtMs)
        assertEquals(base64Encode(accountId), back.accountIdB64)
        // A guest account has no name.
        assertEquals("", RecallRecordCodec.decode(RecallRecordCodec.encode(record(username = "")))!!.username)
    }

    @Test
    fun `the record refuses a phrase seed, a wrong id and a damaged blob`() {
        // The 64-byte phrase seed must never be what is kept.
        assertFalse(RecallRecordStore(MemoryStorage()).save(RecallRecord(accountId, ByteArray(64), "a", 1L)))
        assertFalse(RecallRecordStore(MemoryStorage()).save(RecallRecord(ByteArray(15), seed, "a", 1L)))
        val good = RecallRecordCodec.encode(record())
        assertNull(RecallRecordCodec.decode(good.copyOf(good.size - 1)))
        assertNull(RecallRecordCodec.decode(good + 0))
        assertNull(RecallRecordCodec.decode(good.copyOf().also { it[0] = 9 }))
        assertNull(RecallRecordCodec.decode(ByteArray(0)))
    }

    @Test
    fun `the record never prints its seed`() {
        assertEquals("RecallRecord(<redacted>)", record().toString())
        val r = record()
        r.zero()
        assertArrayEquals(ByteArray(32), r.oldAccountSeed)
    }

    @Test
    fun `the store round-trips, is found at launch, and deletes`() {
        val storage = MemoryStorage()
        val store = RecallRecordStore(storage)
        assertNull(store.loadAny())
        assertTrue(store.save(record()))
        assertEquals(listOf(RecallRecordStore.hexOf(accountId)), storage.list())
        assertArrayEquals(seed, store.load(base64Encode(accountId))!!.oldAccountSeed)
        assertEquals("alice", store.loadAny()!!.username)
        store.delete(base64Encode(accountId))
        assertNull(store.loadAny())
        assertNull(store.load(base64Encode(accountId)))
    }

    @Test
    fun `a record that cannot be written is no record, so the full wipe runs`() {
        val storage = MemoryStorage().apply { failWrites = true }
        assertFalse(RecallRecordStore(storage).save(record()))
        assertTrue(storage.blobs.isEmpty())
    }

    @Test
    fun `a record that cannot be opened at launch is dropped, not shown`() {
        val storage = MemoryStorage()
        RecallRecordStore(storage).save(record())
        // Damaged content.
        storage.blobs[RecallRecordStore.hexOf(accountId)] = ByteArray(10)
        assertNull(RecallRecordStore(storage).loadAny())
        assertTrue(storage.blobs.isEmpty())
        // Filed under another account's name.
        storage.blobs["ff".repeat(16)] = RecallRecordCodec.encode(record())
        assertNull(RecallRecordStore(storage).loadAny())
        assertTrue(storage.blobs.isEmpty())
        // A wrap key the OS invalidated.
        RecallRecordStore(storage).save(record())
        storage.corruptReads = true
        assertNull(RecallRecordStore(storage).loadAny())
        assertTrue(storage.blobs.isEmpty())
    }

    @Test
    fun `the file name is hex, which the wipe's backstop sweep does not match`() {
        val hex = RecallRecordStore.hexOf(ByteArray(16) { 0xFB.toByte() })
        assertEquals("fb".repeat(16), hex)
        val acct = base64Encode(ByteArray(16) { 0xFB.toByte() })
        val name = hex + RecallRecordStore.FILE_SUFFIX
        for (stem in me.lakko.app.account.AccountLocalWipe.fileNameStems(acct)) {
            assertFalse(name == stem || name.startsWith("$stem."))
        }
    }

    // ---- the window by this device's clock ----------------------------------

    @Test
    fun `the record expires 30 days after the device learned, or at the server's date once known`() {
        assertFalse(recallExpired(learnedAtMs = 0L, serverExpiresAtMs = null, nowMs = 30 * day - 1))
        assertTrue(recallExpired(learnedAtMs = 0L, serverExpiresAtMs = null, nowMs = 30 * day))
        // The server's date wins in both directions.
        assertTrue(recallExpired(learnedAtMs = 0L, serverExpiresAtMs = 5 * day, nowMs = 5 * day))
        assertFalse(recallExpired(learnedAtMs = 0L, serverExpiresAtMs = 40 * day, nowMs = 35 * day))
    }

    // ---- the server's answers -----------------------------------------------

    @Test
    fun `every server answer has one meaning`() {
        assertEquals(RecallFailure.Refused, recallFailureOf(http(401, "recall-refused")))
        assertEquals(RecallFailure.WrongPassword, recallFailureOf(http(401, "wrong-password")))
        assertEquals(RecallFailure.WrongPassword, recallFailureOf(RecallWrongPassword()))
        assertEquals(RecallFailure.RateLimited(42), recallFailureOf(http(429, "too-many-attempts", 42)))
        assertEquals(RecallFailure.RateLimited(null), recallFailureOf(http(429, null)))
        assertEquals(RecallFailure.Offline, recallFailureOf(offline()))
        assertEquals(RecallFailure.Other, recallFailureOf(http(400, null)))
        assertEquals(RecallFailure.Other, recallFailureOf(http(503, "rate-limiter-unavailable")))
        assertEquals(RecallFailure.Other, recallFailureOf(IllegalStateException("x")))
    }

    @Test
    fun `only the exact refusal code ends the recall`() {
        // An unlabelled 401, another code, or the code on another status
        // must never destroy the record.
        assertEquals(RecallFailure.Other, recallFailureOf(http(401, null)))
        assertEquals(RecallFailure.Other, recallFailureOf(http(401, "key-generation-superseded")))
        assertEquals(RecallFailure.Other, recallFailureOf(http(403, "recall-refused")))
        assertEquals(RecallStatusOutcome.Later(null), recallStatusOutcomeOf(http(401, null)))
        assertEquals(RecallStatusOutcome.Refused, recallStatusOutcomeOf(http(401, "recall-refused")))
        assertEquals(RecallStatusOutcome.Offline, recallStatusOutcomeOf(offline()))
        assertEquals(RecallStatusOutcome.Later(7), recallStatusOutcomeOf(http(429, "too-many-attempts", 7)))
        assertEquals(RecallStatusOutcome.Later(null), recallStatusOutcomeOf(http(500, null)))
    }

    @Test
    fun `account deletion blocked by an open window carries its date`() {
        assertEquals(123_456L, deleteBlockedUntilMs(409, """{"error":"recall-window-open","until_ms":123456}"""))
        // Anything else is not this refusal.
        assertNull(deleteBlockedUntilMs(409, """{"error":"something-else","until_ms":1}"""))
        assertNull(deleteBlockedUntilMs(403, """{"error":"recall-window-open","until_ms":1}"""))
        assertNull(deleteBlockedUntilMs(409, """{"error":"recall-window-open"}"""))
        assertNull(deleteBlockedUntilMs(409, ""))
        assertNull(deleteBlockedUntilMs(409, null))
        assertNull(deleteBlockedUntilMs(409, "not json"))
    }

    @Test
    fun `the delete dialog says until when, and keeps its other lines`() {
        // The medium date: words from the interface language, order from
        // the device's region (localization.md 5), here a US device.
        me.lakko.app.l10n.DeviceFormats.overrideForTests(java.util.Locale.US, null)
        val blocked = try {
            me.lakko.app.webauthn.deleteAccountRefusalText(
                409, """{"error":"recall-window-open","until_ms":1790000000000}""", "en",
            )
        } finally {
            me.lakko.app.l10n.DeviceFormats.overrideForTests(null, null)
        }
        assertEquals("settings.account.delete.blocked", blocked.key)
        assertEquals("Sep 21, 2026", blocked.texts["date"])
        assertEquals(
            "common.error.too_many_attempts",
            me.lakko.app.webauthn.deleteAccountRefusalText(429, "", "en").key,
        )
        assertEquals(
            "settings.account.delete.error.generic",
            me.lakko.app.webauthn.deleteAccountRefusalText(409, """{"error":"other"}""", "en").key,
        )
    }

    // ---- the screen: the question --------------------------------------------

    @Test
    fun `while checking and while offline No is disabled and no date is drawn`() {
        val checking = ui()
        assertFalse(recallNoEnabled(checking))
        assertNull(checking.untilMs)
        // The body's date is when this device learned, until the server says.
        assertEquals(1_000L, checking.resetAtMs)

        val off = (recallAfterStatus(checking, RecallStatusOutcome.Offline, nowMs = 2_000L) as RecallStep.Stay).ui
        assertEquals(RecallAvailability.Offline, off.availability)
        assertFalse(recallNoEnabled(off))
        assertNull(off.untilMs)

        // A limit, or a server fault: still "checking", asked again later.
        val later = (recallAfterStatus(off, RecallStatusOutcome.Later(30), nowMs = 2_000L) as RecallStep.Stay).ui
        assertEquals(RecallAvailability.Checking, later.availability)
    }

    @Test
    fun `once the server confirms, No is enabled and both dates are the server's`() {
        val open = (recallAfterStatus(ui(), RecallStatusOutcome.Open(status()), nowMs = 2_000L) as RecallStep.Stay).ui
        assertTrue(recallNoEnabled(open))
        assertEquals(31 * day, open.untilMs)
        assertEquals(500L, open.resetAtMs)
        // Not while a recall is being sent.
        assertFalse(recallNoEnabled(open.copy(working = true)))
    }

    @Test
    fun `a refusal on the question screen ends silently`() {
        for (stage in listOf(RecallStage.Question, RecallStage.ConfirmYes)) {
            val step = recallAfterStatus(ui(stage), RecallStatusOutcome.Refused, nowMs = 2_000L)
            assertEquals(RecallStep.End(RecallEnd.NotPossible), step)
        }
        assertNull(recallEndLine(RecallEnd.NotPossible))
        assertNull(recallEndLine(RecallEnd.ConfirmedOwn))
        // No attempt left is "not possible" too, when nothing was typed.
        assertEquals(
            RecallStep.End(RecallEnd.NotPossible),
            recallAfterStatus(ui(), RecallStatusOutcome.Open(status(attempts = 0)), nowMs = 2_000L),
        )
    }

    @Test
    fun `a failed re-check never takes back what the server already confirmed`() {
        val open = ui(RecallStage.Form, RecallAvailability.Open(status()))
        assertEquals(RecallStep.Stay(open), recallAfterStatus(open, RecallStatusOutcome.Offline, nowMs = 2_000L))
        assertEquals(RecallStep.Stay(open), recallAfterStatus(open, RecallStatusOutcome.Later(5), nowMs = 2_000L))
    }

    @Test
    fun `the window passing ends the recall, online or not`() {
        // Offline for 30 days after learning: over, by this device's clock.
        assertEquals(
            RecallStep.End(RecallEnd.NotPossible),
            recallAfterStatus(ui(), RecallStatusOutcome.Offline, nowMs = 1_000L + 30 * day),
        )
        // The form was open when the server's date passed.
        val open = ui(RecallStage.Form, RecallAvailability.Open(status(expiresAtMs = 10 * day)))
        assertEquals(
            RecallStep.End(RecallEnd.Closed),
            recallAfterStatus(open, RecallStatusOutcome.Offline, nowMs = 10 * day),
        )
        // The server still answers but its date is behind this clock.
        assertEquals(
            RecallStep.End(RecallEnd.Closed),
            recallAfterStatus(open, RecallStatusOutcome.Open(status(expiresAtMs = 10 * day)), nowMs = 10 * day + 1),
        )
        assertEquals("devices.recall.error.closed", recallEndLine(RecallEnd.Closed)!!.key)
    }

    // ---- the screen: the form ------------------------------------------------

    private val form = ui(RecallStage.Form, RecallAvailability.Open(status())).copy(working = true)

    @Test
    fun `a wrong password stays on the form and shows the tries only below five`() {
        fun after(attempts: Int) = recallAfterFailure(
            form, RecallFailure.WrongPassword, RecallStatusOutcome.Open(status(attempts = attempts)), nowMs = 0L,
        )
        val many = (after(12) as RecallStep.Stay).ui
        assertFalse(many.working)
        assertEquals(RecallLine.WrongPassword(triesLeft = null), many.line)
        assertEquals(12, (many.availability as RecallAvailability.Open).status.attemptsRemaining)
        assertEquals(RecallLine.WrongPassword(triesLeft = 4), (after(4) as RecallStep.Stay).ui.line)
        assertEquals(RecallLine.WrongPassword(triesLeft = 1), (after(1) as RecallStep.Stay).ui.line)
        assertNull(triesLeftToShow(5))
        assertNull(triesLeftToShow(0))
        assertEquals(4, triesLeftToShow(4))
    }

    @Test
    fun `no tries left is the exhausted end, with its line`() {
        assertEquals(
            RecallStep.End(RecallEnd.Exhausted),
            recallAfterFailure(form, RecallFailure.WrongPassword, RecallStatusOutcome.Open(status(attempts = 0)), 0L),
        )
        assertEquals("devices.recall.error.exhausted", recallEndLine(RecallEnd.Exhausted)!!.key)
        // The periodic check noticing it while the form is open.
        assertEquals(
            RecallStep.End(RecallEnd.Exhausted),
            recallAfterStatus(form.copy(working = false), RecallStatusOutcome.Open(status(attempts = 0)), nowMs = 0L),
        )
    }

    @Test
    fun `a wrong password whose tries could not be read still says wrong password`() {
        for (statusAfter in listOf(RecallStatusOutcome.Offline, RecallStatusOutcome.Later(3), null)) {
            val stay = recallAfterFailure(form, RecallFailure.WrongPassword, statusAfter, nowMs = 0L) as RecallStep.Stay
            assertEquals(RecallLine.WrongPassword(triesLeft = null), stay.ui.line)
            assertFalse(stay.ui.working)
        }
        // The anchor went with that attempt.
        assertEquals(
            RecallStep.End(RecallEnd.Closed),
            recallAfterFailure(form, RecallFailure.WrongPassword, RecallStatusOutcome.Refused, nowMs = 0L),
        )
    }

    @Test
    fun `a refusal while the form is open is the closed end`() {
        assertEquals(RecallStep.End(RecallEnd.Closed), recallAfterFailure(form, RecallFailure.Refused, null, 0L))
        assertEquals(
            RecallStep.End(RecallEnd.Closed),
            recallAfterStatus(form.copy(working = false), RecallStatusOutcome.Refused, nowMs = 0L),
        )
    }

    @Test
    fun `a limit, no connection and any other failure keep the form and the record`() {
        val limited = recallAfterFailure(form, RecallFailure.RateLimited(90), null, nowMs = 1_000L) as RecallStep.Stay
        assertEquals(RecallLine.RateLimited(untilMs = 91_000L), limited.ui.line)
        assertFalse(limited.ui.working)
        // A limit without a stated wait still counts down.
        val bare = recallAfterFailure(form, RecallFailure.RateLimited(null), null, nowMs = 0L) as RecallStep.Stay
        assertTrue((bare.ui.line as RecallLine.RateLimited).untilMs > 0L)
        assertEquals(
            RecallLine.Offline,
            (recallAfterFailure(form, RecallFailure.Offline, null, 0L) as RecallStep.Stay).ui.line,
        )
        assertEquals(
            RecallLine.Failed,
            (recallAfterFailure(form, RecallFailure.Other, null, 0L) as RecallStep.Stay).ui.line,
        )
    }

    @Test
    fun `the main button needs three filled fields, the sign-up rule, a match, and no wait`() {
        val open = ui(RecallStage.Form, RecallAvailability.Open(status()))
        assertTrue(recallSubmitEnabled(open, "old", "newpassword", "newpassword", nowMs = 0L))
        assertFalse(recallSubmitEnabled(open, "", "newpassword", "newpassword", nowMs = 0L))
        assertFalse(recallSubmitEnabled(open, "old", "newpassword", "", nowMs = 0L))
        assertFalse(recallSubmitEnabled(open, "old", "short", "short", nowMs = 0L))
        assertFalse(recallSubmitEnabled(open, "old", "newpassword", "newpassworD", nowMs = 0L))
        assertFalse(recallSubmitEnabled(open.copy(working = true), "old", "newpassword", "newpassword", nowMs = 0L))
        // Not before the server confirmed.
        assertFalse(recallSubmitEnabled(ui(RecallStage.Form), "old", "newpassword", "newpassword", nowMs = 0L))
        // During a limit's wait, and again after it.
        val waiting = open.copy(line = RecallLine.RateLimited(untilMs = 5_000L))
        assertFalse(recallSubmitEnabled(waiting, "old", "newpassword", "newpassword", nowMs = 4_999L))
        assertTrue(recallSubmitEnabled(waiting, "old", "newpassword", "newpassword", nowMs = 5_000L))

        assertEquals("auth.password.error.mismatch", recallFormProblem("newpassword", "x")!!.key)
        assertEquals("auth.password.error.too_short", recallFormProblem("short", "short")!!.key)
        assertNull(recallFormProblem("newpassword", "newpassword"))
        assertEquals(RECALL_MIN_PASSWORD_LEN, me.lakko.app.ui.auth.SignupViewModel.MIN_PASSWORD_LEN)
    }

    @Test
    fun `the check is repeated quickly offline, slowly when open, and never after a refusal`() {
        assertEquals(10_000L, recallRecheckDelayMs(RecallStatusOutcome.Offline))
        assertEquals(45_000L, recallRecheckDelayMs(RecallStatusOutcome.Later(45)))
        assertTrue(recallRecheckDelayMs(RecallStatusOutcome.Later(null)) > 0L)
        assertTrue(recallRecheckDelayMs(RecallStatusOutcome.Open(status())) > recallRecheckDelayMs(RecallStatusOutcome.Offline))
        assertEquals(Long.MAX_VALUE, recallRecheckDelayMs(RecallStatusOutcome.Refused))
    }

    // ---- the copy -------------------------------------------------------------

    @Test
    fun `every key the recall surfaces draw is in the catalog`() {
        val catalog = listOf("src/main/assets/l10n/source.json", "app/src/main/assets/l10n/source.json")
            .map { java.io.File(it) }.first { it.exists() }.readText()
        val keys = listOf(
            "devices.signed_out.title", "devices.signed_out.this_account",
            "devices.recall.body", "devices.recall.question", "devices.recall.yes", "devices.recall.no",
            "devices.recall.until", "devices.recall.checking", "devices.recall.yes.confirm",
            "devices.recall.yes.delete", "devices.recall.yes.back", "devices.recall.form.title",
            "devices.recall.form.lede", "devices.recall.form.old_password", "devices.recall.form.new_password",
            "devices.recall.form.repeat_password", "devices.recall.form.effect_keys",
            "devices.recall.form.effect_factors", "devices.recall.form.effect_messages",
            "devices.recall.form.submit", "devices.recall.form.working", "devices.recall.error.wrong_password",
            "devices.recall.error.tries_left", "devices.recall.error.exhausted", "devices.recall.error.closed",
            "devices.recall.done", "settings.account.delete.blocked", "recovery.error.rate_limited",
            "common.error.offline", "common.back", "common.dismiss", "auth.password.hint",
            "auth.password.error.mismatch", "auth.password.error.too_short",
            // The stand-in for a failure no key names.
            "restore_identity.reset.error.generic",
        )
        for (key in keys) assertTrue("missing catalog key $key", catalog.contains("\"$key\": {"))
        assertNotNull(recallEndLine(RecallEnd.Closed))
    }

    // ---- ADR-0075 section 8 / UX 19.7 --------------------------------------

    @Test
    fun `the phrase is offered on the form only when the server opened that arm`() {
        assertFalse(recallOffersPhrase(ui(RecallStage.Form)))
        assertFalse(recallOffersPhrase(ui(RecallStage.Form, RecallAvailability.Offline)))
        assertFalse(recallOffersPhrase(ui(RecallStage.Form, RecallAvailability.Open(status()))))
        assertTrue(
            recallOffersPhrase(ui(RecallStage.Form, RecallAvailability.Open(status().copy(phraseArm = true)))),
        )
    }
}

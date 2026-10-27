// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import java.io.File
import org.junit.Assert.assertTrue
import org.junit.Test

/** ADR-0075 - every catalog key the recovery-email screens and rules use
 *  exists in the compiled catalog (no string was added by this client). */
class RecoveryEmailCatalogTest {

    private val catalog: String = listOf("src/main/assets/l10n/source.json", "app/src/main/assets/l10n/source.json")
        .map { File(it) }.first { it.exists() }.readText()

    @Test
    fun `the keys the pure rules name are in the catalog`() {
        val keys = AccountProtection.entries.map(::protectionStatusKey) +
            AccountNoticeKind.entries.map(::accountNoticeTitleKey) +
            AccountNoticeKind.entries.mapNotNull(::accountNoticeBodyKey)
        for (key in keys) assertTrue("missing catalog key $key", catalog.contains("\"$key\": {"))
    }

    @Test
    fun `the keys the screens draw are in the catalog`() {
        val keys = listOf(
            "protect.title", "protect.body", "protect.email.title", "protect.email.badge", "protect.email.body",
            "protect.phrase.title", "protect.phrase.body", "protect.skip", "protect.skip.confirm.title",
            "protect.skip.confirm.body", "protect.skip.confirm.continue", "protect.banner.none",
            "protect.banner.action", "protect.phrase.save.body", "protect.phrase.save.effect",
            "protect.phrase.save.show", "protect.phrase.save.failed",
            "recovery_email.title", "recovery_email.body", "recovery_email.field", "recovery_email.invalid",
            "recovery_email.not_kept", "recovery_email.mailbox_warning", "recovery_email.send",
            "recovery_email.send_failed", "recovery_email.needs_username", "recovery_email.unavailable",
            "recovery_email.code.title", "recovery_email.code.sent_to", "recovery_email.code.wrong",
            "recovery_email.code.expired", "recovery_email.code.used_up", "recovery_email.code.resend",
            "recovery_email.code.resend_in", "recovery_email.code.other_address", "recovery_email.status.on",
            "recovery_email.status.off", "recovery_email.replace", "recovery_email.remove",
            "recovery_email.remove.confirm.title", "recovery_email.remove.confirm.body",
            "recovery.email.row.title", "recovery.email.row.body", "recovery.email.start.body",
            "recovery.email.check.title", "recovery.email.check.body", "recovery.email.totp.title",
            "recovery.email.totp.body", "recovery.email.totp.lost", "recovery.email.save",
            "recovery.email.restoring", "recovery.error.unavailable", "recovery.error.rate_limited",
            "notice.was_me", "notice.was_not_me", "notice.secure.new_keys",
            "recall.phrase.use", "recall.phrase.changed", "recall.phrase.take_back",
            "common.ok", "common.done", "common.dismiss", "common.sending", "common.saving", "common.removing",
            "common.try_again", "common.error.offline", "common.error.too_many_attempts",
            "common.error.unavailable", "chat.record.cancelled", "devices.rotate.step_keys",
            "recovery.new_password.title", "recovery.new_password.saved", "recovery.totp.code_label",
            "recovery.totp.error.wrong", "devices.recall.error.closed", "devices.recall.form.working",
            // The alignment pass's keys.
            "proof.recovery_email.help", "proof.save_phrase.help", "notice.secure.title",
            "notice.secure.password_saved", "protect.phrase.save.progress", "protect.phrase.save.finishing",
            "recall.phrase.wrong", "recovery_email.add_again.title", "recovery_email.add_again.body",
            "recovery.email.failed", "recovery_email.totp_warning",
            "protect.passkey.title", "protect.passkey.body", "protect.done", "protect.continue",
            "account.keys_changed.finishing",
            "username.claim.title", "username.claim.body", "username.claim.password_help",
            "username.claim.save", "auth.signup.error.username_taken",
            "username.change.title", "proof.username.help",
            "auth.password.new.label", "auth.password.new_confirm.label", "auth.signup.username.hint",
        )
        for (key in keys) assertTrue("missing catalog key $key", catalog.contains("\"$key\": {"))
    }
}

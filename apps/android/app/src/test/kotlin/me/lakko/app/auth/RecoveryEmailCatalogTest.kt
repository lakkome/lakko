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
        val keys = ChatsKeptBy.entries.map(::protectionStatusKey) +
            AccountNoticeKind.entries.map(::accountNoticeTitleKey) +
            AccountNoticeKind.entries.mapNotNull(::accountNoticeBodyKey)
        for (key in keys) assertTrue("missing catalog key $key", catalog.contains("\"$key\": {"))
    }

    @Test
    fun `the keys the screens draw are in the catalog`() {
        val keys = listOf(
            "protect.title", "protect.body", "protect.email.title", "protect.email.badge", "protect.email.body",
            "protect.phrase.title", "protect.phrase.body", "protect.phrase.save.body",
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
            "recovery.error.unavailable", "recovery.error.rate_limited",
            "notice.was_me", "notice.was_not_me",
            "recall.phrase.use", "recall.phrase.changed", "recall.phrase.take_back",
            "common.ok", "common.done", "common.dismiss", "common.sending", "common.saving", "common.removing",
            "common.try_again", "common.error.offline", "common.error.too_many_attempts",
            "common.error.unavailable", "chat.record.cancelled", "devices.rotate.step_keys",
            "recovery.new_password.title", "recovery.new_password.saved", "recovery.totp.code_label",
            "recovery.totp.error.wrong", "devices.recall.error.closed", "devices.recall.form.working",
            // The alignment pass's keys.
            "proof.recovery_email.help", "proof.save_phrase.help", "notice.secure.title",
            "notice.secure.password_saved", "recall.phrase.wrong", "recovery_email.totp_warning",
            "protect.passkey.title", "protect.passkey.body", "protect.done", "protect.continue",
            "account.keys_changed.finishing",
            "username.claim.title", "username.claim.body", "username.claim.password_help",
            "username.claim.save", "auth.signup.error.username_taken",
            "username.change.title", "proof.username.help",
            "auth.password.new.label", "auth.password.new_confirm.label", "auth.signup.username.hint",
        )
        for (key in keys) assertTrue("missing catalog key $key", catalog.contains("\"$key\": {"))
    }

    @Test
    fun `the keys of the required second way in, the reset factor and a started-over contact are in the catalog`() {
        // ADR-0075 / ADR-0073 / ADR-0101 as amended 2026-11-26 (UX 15.1b,
        // 15.1c, 16.11, 19.1a to 19.1d).
        val keys = listOf(
            "protect.title_required", "protect.totp.title", "protect.totp.body", "protect.optional",
            "protect.phrase.title_optional", "protect.continue_needs", "protect.totp_only.warning",
            "protect.totp_only.warning_phrase", "protect.passkey.cancelled", "protect.last_factor",
            "protect.username_email", "protect.totp_only.remove_notice", "protect.totp_only.remove_notice_phrase",
            "protect.status.phrase", "protect.status.passkey_seal", "protect.status.password_only",
            "protect.password_only.title", "protect.password_only.body", "protect.password_only.save_phrase",
            "protect.password_only.add_passkey", "protect.password_only.passkey_no_seal",
            "settings.passkeys.no_seal", "settings.recovery.totp.open_app", "settings.recovery.totp.on",
            "settings.recovery.totp.scan", "settings.recovery.totp.key", "settings.recovery.totp.done",
            "settings.recovery.totp.already_enabled", "settings.recovery.totp.error.wrong",
            "settings.recovery.totp.error.generic", "settings.recovery.totp.set_up", "settings.recovery.totp.lede",
            "proof.add_totp.help",
            "proof.reset.factor.heading", "proof.reset.factor.title", "proof.reset.factor.title_no_password",
            "proof.reset.factor.totp", "proof.reset.factor.email", "proof.reset.factor.passkey",
            "proof.reset.factor.none", "proof.reset.factor.totp.prompt", "proof.reset.factor.email.sent",
            "proof.reset.factor.passkey.use", "proof.reset.factor.done.totp", "proof.reset.factor.done.email",
            "proof.reset.factor.done.passkey", "proof.reset.no_factor.title", "proof.reset.no_factor.body",
            "proof.reset.no_factor.body_none", "proof.reset.cannot.what_you_can_do",
            "proof.reset.cannot.option_device", "proof.reset.cannot.option_phrase",
            "proof.reset.cannot.option_passkey", "proof.reset.cannot.otherwise", "proof.reset.cancelled",
            "proof.passkey.other_account", "proof.passkey.failed",
            "restore_identity.reset.sealed", "restore_identity.reset.unsealed",
            "restore_identity.reset.unsealed_use_passkey", "restore_identity.reset.password_help",
            "restore_identity.reset.error.wrong_password", "restore_identity.reset.error.generic",
            "restore_identity.reset.effect_devices", "restore_identity.reset.effect_passkeys",
            "restore_identity.reset.effect_safety_number", "restore_identity.reset.keeps.password",
            "restore_identity.reset.keeps.password_totp", "restore_identity.reset.keeps.password_email",
            "restore_identity.reset.keeps.password_totp_email",
            "contacts.started_over.closed", "contacts.started_over.request_note",
            "common.copy", "common.back", "common.not_now", "common.continue", "common.checking",
            "recovery.passkey.waiting", "auth.password.label",
        )
        for (key in keys) assertTrue("missing catalog key $key", catalog.contains("\"$key\": {"))
    }

    @Test
    fun `no screen or rule names a key the 2026-11-26 amendment removed`() {
        val removed = listOf(
            "protect.skip", "protect.banner.none", "protect.banner.action", "protect.status.none",
            "protect.status.email", "restore_identity.reset.effect_messages", "recovery_email.add_again.",
            "recovery.email.restoring", "recovery.email.failed", "protect.phrase.save.effect",
            "protect.phrase.save.progress", "protect.phrase.save.finishing", "notice.opened_by_email.",
            "notice.secure.new_keys",
            // 2026-11-28: the reset warning no longer claims the chats stay,
            // nor names an authenticator the account may not have.
            "restore_identity.reset.effect_contacts", "restore_identity.reset.effect_password",
        )
        val root = listOf("src/main/kotlin", "app/src/main/kotlin").map { File(it) }.first { it.exists() }
        val sources = root.walkTopDown().filter { it.isFile && it.extension == "kt" }.toList()
        assertTrue("the source tree was found", sources.size > 100)
        for (file in sources) {
            val text = file.readText()
            for (key in removed) {
                assertTrue("${file.name} still names the removed key $key", !text.contains("\"$key"))
            }
        }
    }
}

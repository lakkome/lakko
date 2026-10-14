// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

// ADR-0061 S3 — the iOS interface-language runtime: the resolution order
// (stored choice, else the system preference, else English), the per-device
// persistence of the choice, and the real compiled catalog the app bundles.

@MainActor
final class LocalizationTests: XCTestCase {
    private var suiteName = ""
    private var defaults: UserDefaults!

    override func setUp() {
        super.setUp()
        suiteName = "LocalizationTests-\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suiteName)
    }

    override func tearDown() {
        defaults.removePersistentDomain(forName: suiteName)
        super.tearDown()
    }

    private func make(system: [String]) -> Localization {
        Localization(defaults: defaults, bundle: .main, systemPreferred: { system })
    }

    func testFollowsTheFirstSupportedSystemLanguage() {
        let l10n = make(system: ["de-DE", "ru-RU", "en-US"])
        XCTAssertNil(l10n.selected)
        XCTAssertEqual(l10n.language, "ru")
    }

    func testFallsBackToEnglishWhenNoSystemLanguageIsSupported() {
        let l10n = make(system: ["de-DE", "ja-JP"])
        XCTAssertEqual(l10n.language, "en")
        XCTAssertEqual(l10n.t("nav.chats"), "Chats")
    }

    func testStoredChoiceWinsOverTheSystem() {
        defaults.set("uk", forKey: Localization.storageKey)
        let l10n = make(system: ["ru-RU"])
        XCTAssertEqual(l10n.selected, "uk")
        XCTAssertEqual(l10n.language, "uk")
    }

    func testUnknownStoredChoiceReadsAsSameAsSystem() {
        defaults.set("xx", forKey: Localization.storageKey)
        let l10n = make(system: ["es-MX"])
        XCTAssertNil(l10n.selected)
        XCTAssertEqual(l10n.language, "es")
    }

    func testSelectPersistsAndSurvivesANewInstance() {
        let first = make(system: ["en-US"])
        first.select("he")
        XCTAssertEqual(defaults.string(forKey: Localization.storageKey), "he")
        XCTAssertEqual(first.language, "he")

        let second = make(system: ["en-US"])
        XCTAssertEqual(second.selected, "he")
        XCTAssertEqual(second.language, "he")
    }

    func testSameAsSystemRemovesTheStoredChoice() {
        let l10n = make(system: ["tr-TR"])
        l10n.select("fa")
        XCTAssertEqual(l10n.language, "fa")
        l10n.select(nil)
        XCTAssertNil(defaults.object(forKey: Localization.storageKey))
        XCTAssertNil(l10n.selected)
        XCTAssertEqual(l10n.language, "tr")
    }

    func testRightToLeftLanguagesMirrorTheLayout() {
        let l10n = make(system: ["en-US"])
        XCTAssertEqual(l10n.layoutDirection, .leftToRight)
        for code in ["ar", "fa", "he"] {
            l10n.select(code)
            XCTAssertTrue(l10n.isRightToLeft, code)
            XCTAssertEqual(l10n.layoutDirection, .rightToLeft, code)
        }
        l10n.select("zh-Hans")
        XCTAssertEqual(l10n.layoutDirection, .leftToRight)
    }

    func testTheBundledCatalogTranslatesAndSwitchesLive() {
        let l10n = make(system: ["en-US"])
        XCTAssertEqual(l10n.t("settings.language.title"), "Language")
        l10n.select("ru")
        let russian = l10n.t("settings.language.title")
        XCTAssertFalse(russian.isEmpty)
        XCTAssertNotEqual(russian, "Language")
        XCTAssertNotEqual(russian, "settings.language.title")
        XCTAssertEqual(l10n.locale.identifier, "ru")
    }

    func testFormattedMessageCarriesItsArgument() {
        let l10n = make(system: ["en-US"])
        let text = l10n.t("settings.language.same_as_system_detail", texts: ["language": "Deutsch"])
        XCTAssertTrue(text.hasPrefix("Same as system"))
        XCTAssertTrue(text.contains("Deutsch"))
    }

    func testEveryLanguageHasATranslatedNameAndOwnName() {
        let l10n = make(system: ["en-US"])
        XCTAssertEqual(l10n.languages.count, 11)
        for info in l10n.languages {
            XCTAssertFalse(info.ownName.isEmpty, info.code)
            XCTAssertFalse(l10n.translatedName(of: info).hasPrefix("language.name."), info.code)
        }
    }

    // MARK: - ADR-0061 S5 batch 1 (sign-in, sign-up, recovery)

    func testALinkMessageSplitsAroundTheLink() {
        let l10n = make(system: ["en-US"])
        let parts = l10n.linkParts("auth.login.no_account")
        XCTAssertEqual(parts.lead.trimmingCharacters(in: .whitespaces), "New to Lakko?")
        XCTAssertEqual(parts.trail, "")
    }

    func testWordPositionsUseTheListFormatterAndLatinDigits() {
        let l10n = make(system: ["en-US"])
        XCTAssertEqual(l10n.positions([3, 7, 15]), "3, 7, and 15")
        l10n.select("ar")
        let arabic = l10n.positions([3, 7, 15])
        XCTAssertTrue(arabic.contains("3") && arabic.contains("7") && arabic.contains("15"), arabic)
        XCTAssertFalse(arabic.contains("and"), arabic)
    }

    func testTheOffActorLookupAnswersFromTheCatalog() {
        XCTAssertNotEqual(Localization.text("auth.error.wrong_credentials"), "auth.error.wrong_credentials")
        XCTAssertEqual(
            Localization.text("auth.password.error.too_short", counts: ["count": 8]),
            Localization.shared.t("auth.password.error.too_short", counts: ["count": 8]))
    }

    func testAuthErrorsRenderFromTheirCodeNotTheirMessage() {
        let t = { (key: String) in Localization.text(key) }
        XCTAssertEqual(
            AuthErrorCopy.login(AuthClientError.http(status: 401, endpoint: "invalid", body: "server prose")),
            t("auth.error.wrong_credentials"))
        XCTAssertEqual(AuthErrorCopy.login(AuthClientError.argument("core prose")), t("auth.error.wrong_credentials"))
        XCTAssertEqual(
            AuthErrorCopy.signup(AuthClientError.http(status: 409, endpoint: "taken", body: "")), t("auth.signup.error.username_taken"))
        XCTAssertEqual(AuthErrorCopy.signup(AuthClientError.http(status: 502, endpoint: "", body: "")), t("common.error.unavailable"))
        XCTAssertEqual(
            AuthErrorCopy.recover(FfiError.Mnemonic(message: "bad checksum")),
            t("restore_identity.error.invalid_phrase"))
        XCTAssertEqual(AuthErrorCopy.recover(AuthClientError.http(status: 401, endpoint: "", body: "")), t("recovery.error.no_match"))
        XCTAssertEqual(
            AuthErrorCopy.totp(AuthClientError.http(status: 401, endpoint: "", body: ""), typedUsername: true),
            t("recovery.totp.error.wrong_username"))
        XCTAssertEqual(AuthErrorCopy.identityRestore(PhraseMismatch()), t("restore_identity.error.phrase_mismatch"))
        XCTAssertEqual(AuthErrorCopy.deviceRestore(PhraseMismatch()), t("device_link.error.identity_mismatch"))
        XCTAssertEqual(
            AuthErrorCopy.deviceRestore(IdentityVerificationUnavailable("diagnostic", kind: .missing)),
            t("restore_identity.error.not_published"))
        XCTAssertEqual(AuthErrorCopy.deviceRestore(DeviceRestoreNotLetIn()), t("device_link.error.not_let_in"))
    }

    // MARK: - Batch 2

    /// An error on screen is kept as the way to render it, so each read
    /// renders again (in the language of that moment), and a fresh value is
    /// never equal to an older one, so setting it always publishes.
    func testUiTextRendersOnEveryReadAndIsEqualOnlyToItself() {
        var calls = 0
        let text = UiText {
            calls += 1
            return "rendered \(calls)"
        }
        XCTAssertEqual(text.text, "rendered 1")
        XCTAssertEqual(text.text, "rendered 2")
        XCTAssertEqual(text, text)
        XCTAssertNotEqual(UiText.key("common.cancel"), UiText.key("common.cancel"))
        XCTAssertEqual(UiText.key("common.cancel").text, Localization.text("common.cancel"))
    }

    /// A message split around an argument keeps no bidi isolate on either
    /// side, so a copied value (an authenticator key) types in cleanly.
    func testPartsSplitAroundAnArgumentWithoutIsolates() {
        let l10n = make(system: ["en-US"])
        let parts = l10n.parts("settings.recovery.totp.key", around: "key")
        XCTAssertEqual(parts.lead, "Key: ")
        XCTAssertEqual(parts.trail, "")
        let isolates = CharacterSet(charactersIn: "\u{2066}\u{2067}\u{2068}\u{2069}")
        for piece in [parts.lead, parts.trail] {
            XCTAssertNil(piece.rangeOfCharacter(from: isolates))
        }
    }

    /// The Settings copy the iOS screens read resolves from the bundled
    /// catalog (a missing key would render as itself).
    func testSettingsKeysResolve() {
        let l10n = make(system: ["en-US"])
        for key in [
            "settings.section.network", "settings.section.danger_zone",
            "settings.anti_censorship.title", "settings.account.forget_device.body_device",
            "settings.recovery.phrase.saved_here", "backup.rollback_refused.title",
            "settings.passkeys.remove.error.cancelled", "call.link.error.generic",
        ] {
            XCTAssertNotEqual(l10n.t(key), key, key)
        }
        XCTAssertEqual(
            l10n.t("settings.profile.name.hint", counts: ["count": 64]),
            "Shown to the people you talk to. Not unique, at most 64 characters.")
    }

    /// The notification extension reads the counted attachment line under
    /// `<key>#<count>` (`nseAttachmentsLine`, which hard-codes the same shape
    /// because it compiles without this module); pin both ends here.
    func testCountedNotificationWordKeyShape() {
        XCTAssertEqual(
            SharedStorage.countedNotificationWordKey("notification.message.attachments", count: 3),
            "notification.message.attachments#3")
        XCTAssertTrue(SharedStorage.notificationCountedWordKeys.contains("notification.message.attachments"))
        XCTAssertTrue(SharedStorage.notificationWordKeys.contains("notification.message.new"))
        let l10n = make(system: ["en-US"])
        XCTAssertEqual(
            l10n.t("notification.message.attachments", counts: ["count": 3]), "Sent 3 attachments")
        XCTAssertEqual(l10n.t("notification.message.new"), "New message")
    }

    /// A message held as ``UiText`` renders at every read, so a notice kept
    /// in state (a call error, the line under a call's status row) follows a
    /// language switch instead of keeping the words of the moment it was
    /// produced.
    func testHeldCallTextRendersAtReadTime() {
        var word = "first"
        let state = CallUiState()
        state.setMediaNotice(UiText { word })
        word = "second"
        XCTAssertEqual(state.mediaNotice?.text, "second")
        XCTAssertEqual(
            CallSlot.busyText(.group, verb: .start).text,
            CallSlot.busyMessage(.group, verb: .start))
        XCTAssertEqual(CameraPermission.deniedText.text, CameraPermission.deniedMessage)
    }
}

extension String {
    /// The text without the bidi isolates the catalog formatter puts around
    /// text arguments (ADR-0061), for tests that compare rendered English.
    var withoutIsolates: String {
        let isolates = CharacterSet(charactersIn: "\u{2066}\u{2067}\u{2068}\u{2069}")
        return String(unicodeScalars.filter { !isolates.contains($0) })
    }
}

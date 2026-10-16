// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// The pure half of the system incoming-call experience (`docs/ux/calls.md`
/// sections 2.1, 2.2 and 6): what a wake payload says, what the system call
/// screen is told, which surface shows, which UUID a call is reported under,
/// and which push token goes where.
///
/// Everything here runs with no CallKit, no PushKit, no APNs certificate and no
/// device — which is the point of keeping the decisions out of the classes that
/// own the platform objects. What CANNOT be tested here is the platform
/// behaviour itself: that a report rings the phone, that a VoIP push arrives at
/// all. Neither is reachable without a provisioned device and push credentials.
@MainActor
final class CallKitIncomingTests: XCTestCase {

    private let callId = Data((0..<16).map { UInt8($0) })

    // MARK: - What the callee is told

    func testCallKindLabelSaysWhatTheCallerChose() {
        XCTAssertEqual(CallKindLabel.text(video: false), "Voice call")
        XCTAssertEqual(CallKindLabel.text(video: true), "Video call")
    }

    func testPlaceholderIsUnnamedAndNotVideo() {
        let placeholder = IncomingCallReport.placeholder()
        XCTAssertEqual(placeholder.localizedCallerName, "Incoming call")
        // Offering video for a call that turns out to be voice is a promise the
        // call cannot keep; adding it in the update a moment later is free.
        XCTAssertFalse(placeholder.hasVideo)
    }

    func testResolvedUpdateCarriesNameAndKind() {
        let update = IncomingCallReport.resolved(callerName: "Anna", video: true)
        XCTAssertEqual(update.localizedCallerName, "Anna")
        XCTAssertTrue(update.hasVideo)
    }

    func testBlankNameFallsBackToTheUnnamedWording() {
        for name in [nil, "", "   ", "\n"] as [String?] {
            let update = IncomingCallReport.resolved(callerName: name, video: false)
            XCTAssertEqual(update.localizedCallerName, "Incoming call")
        }
    }

    func testNameIsTrimmedRatherThanShownWithItsWhitespace() {
        XCTAssertEqual(
            IncomingCallReport.resolved(callerName: "  Anna  ", video: false).localizedCallerName,
            "Anna"
        )
    }

    // MARK: - The call's identity

    func testCallIdRoundTripsThroughTheCallKitUuid() {
        let uuid = IncomingCallReport.uuid(forCallId: callId)
        XCTAssertNotNil(uuid)
        XCTAssertEqual(IncomingCallReport.callId(forUuid: uuid!), callId)
    }

    func testTwoCallsNeverShareAUuid() {
        let other = Data(repeating: 0xFF, count: 16)
        XCTAssertNotEqual(
            IncomingCallReport.uuid(forCallId: callId),
            IncomingCallReport.uuid(forCallId: other)
        )
    }

    func testAMalformedCallIdHasNoUuid() {
        XCTAssertNil(IncomingCallReport.uuid(forCallId: Data(repeating: 1, count: 15)))
        XCTAssertNil(IncomingCallReport.uuid(forCallId: Data()))
    }

    func testHexIsLowercaseAndFullWidth() {
        XCTAssertEqual(
            IncomingCallReport.hex(Data([0x00, 0x0A, 0xFF])),
            "000aff"
        )
    }

    // MARK: - The wake payload

    func testCallWakeIsRecognised() {
        let wake = VoipWakePayload.parse(["g": "abc", "p": NSNumber(value: 42), "k": "c"])
        XCTAssertEqual(wake.groupHex, "abc")
        XCTAssertEqual(wake.pointer, 42)
        XCTAssertTrue(wake.isCallWake)
    }

    func testMessageWakeIsNotACallWake() {
        XCTAssertFalse(VoipWakePayload.parse(["g": "abc", "p": NSNumber(value: 1)]).isCallWake)
        XCTAssertFalse(VoipWakePayload.parse(["k": "m"]).isCallWake)
    }

    func testPointerIsAcceptedAsAStringToo() {
        XCTAssertEqual(VoipWakePayload.parse(["p": "7"]).pointer, 7)
    }

    func testAnEmptyPayloadIsParsedRatherThanRejected() {
        // A VoIP push that is not reported to CallKit terminates the app, so no
        // missing field may make the handler bail before it reports.
        let wake = VoipWakePayload.parse([:])
        XCTAssertNil(wake.groupHex)
        XCTAssertNil(wake.pointer)
        XCTAssertFalse(wake.isCallWake)
    }

    func testAnEmptyGroupReadsAsAbsent() {
        XCTAssertNil(VoipWakePayload.parse(["g": ""]).groupHex)
    }

    // MARK: - Which surface shows (docs/ux/calls.md section 2.1)

    func testAVoipPushAlwaysReportsToTheSystem() {
        // Not a preference: iOS kills an app that takes a VoIP push without
        // reporting a call, foreground or not.
        for active in [true, false] {
            for reported in [true, false] {
                XCTAssertEqual(
                    IncomingCallPresentation.decide(
                        source: .voipPush,
                        appIsActive: active,
                        alreadyReported: reported
                    ),
                    .systemCallUi
                )
            }
        }
    }

    func testAnOpenAppShowsItsOwnIncomingCallScreen() {
        XCTAssertEqual(
            IncomingCallPresentation.decide(
                source: .signalingRow,
                appIsActive: true,
                alreadyReported: false
            ),
            .inApp
        )
    }

    func testABackgroundedAppRingsThePhone() {
        XCTAssertEqual(
            IncomingCallPresentation.decide(
                source: .signalingRow,
                appIsActive: false,
                alreadyReported: false
            ),
            .systemCallUi
        )
    }

    func testACallAlreadyRingingIsNeverRungTwice() {
        // The signaling row for the call the VoIP push already reported arrives
        // moments later on the app's own receive path.
        for active in [true, false] {
            XCTAssertEqual(
                IncomingCallPresentation.decide(
                    source: .signalingRow,
                    appIsActive: active,
                    alreadyReported: true
                ),
                .none
            )
        }
    }

    // MARK: - Adopting the placeholder

    func testACallWithNoPlaceholderUsesItsOwnUuid() {
        let registry = CallKitCallRegistry()
        XCTAssertEqual(
            registry.uuid(forCallId: callId),
            IncomingCallReport.uuid(forCallId: callId)
        )
    }

    func testTheCallIdAdoptsThePlaceholderTheVoipPushRangUnder() {
        let registry = CallKitCallRegistry()
        let placeholder = UUID()
        registry.placeholderReported(placeholder)

        // The offer, decrypted moments later, must keep ringing under the UUID
        // the phone is already showing — not be reported again under its own.
        XCTAssertEqual(registry.uuid(forCallId: callId), placeholder)
        XCTAssertNil(registry.pendingPlaceholder)
        // And it stays adopted for every later question about the same call.
        XCTAssertEqual(registry.uuid(forCallId: callId), placeholder)
    }

    func testOnlyTheFirstCallAdoptsThePlaceholder() {
        let registry = CallKitCallRegistry()
        let placeholder = UUID()
        registry.placeholderReported(placeholder)
        _ = registry.uuid(forCallId: callId)

        let second = Data(repeating: 0x0B, count: 16)
        XCTAssertEqual(
            registry.uuid(forCallId: second),
            IncomingCallReport.uuid(forCallId: second)
        )
    }

    func testASecondPlaceholderSupersedesTheFirstAndNamesIt() {
        let registry = CallKitCallRegistry()
        let first = UUID()
        let second = UUID()
        XCTAssertNil(registry.placeholderReported(first))
        // The caller has to end the superseded one, or the phone is left with a
        // call ringing that nothing will ever finish.
        XCTAssertEqual(registry.placeholderReported(second), first)
        XCTAssertEqual(registry.uuid(forCallId: callId), second)
    }

    func testReleasingByUuidForgetsTheCall() {
        let registry = CallKitCallRegistry()
        let uuid = registry.uuid(forCallId: callId)!
        registry.release(uuid: uuid)
        let placeholder = UUID()
        registry.placeholderReported(placeholder)
        // Forgotten, so the next question adopts the new placeholder instead of
        // answering with the stale adoption.
        XCTAssertEqual(registry.uuid(forCallId: callId), placeholder)
    }

    func testAnAbandonedPlaceholderIsDroppable() {
        let registry = CallKitCallRegistry()
        registry.placeholderReported(UUID())
        registry.releasePlaceholder()
        XCTAssertNil(registry.pendingPlaceholder)
        XCTAssertEqual(
            registry.uuid(forCallId: callId),
            IncomingCallReport.uuid(forCallId: callId)
        )
    }

    // MARK: - What the call history says (section 6)

    func testEndReasonsMapToWhatActuallyHappened() {
        XCTAssertEqual(CallKitEndReason.map(.timedOut), .unanswered)
        XCTAssertEqual(CallKitEndReason.map(.declined), .declinedElsewhere)
        XCTAssertEqual(CallKitEndReason.map(.busy), .unanswered)
        XCTAssertEqual(CallKitEndReason.map(.error), .failed)
        XCTAssertEqual(CallKitEndReason.map(.hungUp), .remoteEnded)
        XCTAssertEqual(CallKitEndReason.map(nil), .remoteEnded)
    }

    // MARK: - Token kinds

    func testTheTwoTokensAreRegisteredUnderDifferentProviders() {
        XCTAssertEqual(PushTokenKind.alert.provider, .apns)
        XCTAssertEqual(PushTokenKind.voip.provider, .apnsVoip)
        // The whole reason the kind is a type: a VoIP token in the alert row
        // does not degrade, it silently stops calls arriving.
        XCTAssertNotEqual(PushTokenKind.alert.provider, PushTokenKind.voip.provider)
    }

    func testTheVoipTokenRidesTheSubscriptionBesideTheAlertToken() {
        let suite = "push-tokens-\(UUID().uuidString)"
        let tokens = PushDeviceTokens(defaults: UserDefaults(suiteName: suite)!)
        defer { UserDefaults().removePersistentDomain(forName: suite) }
        tokens.setAlert(Data([1]))
        tokens.setVoip(Data([2]))
        XCTAssertEqual(tokens.entries().map(\.provider), [.apns, .apnsVoip])
    }

    func testAnUnchangedTokenIsNotAChange() {
        let suite = "push-tokens-\(UUID().uuidString)"
        let tokens = PushDeviceTokens(defaults: UserDefaults(suiteName: suite)!)
        defer { UserDefaults().removePersistentDomain(forName: suite) }
        XCTAssertTrue(tokens.setAlert(Data([1, 2, 3])))
        XCTAssertFalse(tokens.setAlert(Data([1, 2, 3])), "the same token again is not a change")
        XCTAssertTrue(tokens.setAlert(Data([9])))
        XCTAssertFalse(tokens.setAlert(Data()) && tokens.alert != nil, "an empty token is never stored")
        XCTAssertNil(tokens.alert)
    }

    // MARK: - The name mirror

    func testTheMirrorReadsBackWhatTheAppWrote() {
        let defaults = transientDefaults()
        CallPeerNameMirror.mirror(accountIdB64: "acct", groupIdHex: "AA11", name: "Anna", defaults: defaults)
        // Keyed case-insensitively, because the group hex reaches the two
        // processes from different places.
        XCTAssertEqual(
            CallPeerNameMirror.name(accountIdB64: "acct", groupIdHex: "aa11", defaults: defaults), "Anna")
    }

    func testABlankNameRemovesTheRowRatherThanStoringEmptiness() {
        let defaults = transientDefaults()
        CallPeerNameMirror.mirror(accountIdB64: "acct", groupIdHex: "aa11", name: "Anna", defaults: defaults)
        CallPeerNameMirror.mirror(accountIdB64: "acct", groupIdHex: "aa11", name: "   ", defaults: defaults)
        XCTAssertNil(CallPeerNameMirror.name(accountIdB64: "acct", groupIdHex: "aa11", defaults: defaults))
    }

    /// Two accounts on one phone: one account's lock screen never shows the
    /// name the other mirrored, even for the same group, and no account reads
    /// nothing.
    func testTwoAccountsNeverSeeEachOthersNames() {
        let defaults = transientDefaults()
        CallPeerNameMirror.mirror(accountIdB64: "acct-A", groupIdHex: "aa11", name: "Anna", defaults: defaults)
        XCTAssertNil(CallPeerNameMirror.name(accountIdB64: "acct-B", groupIdHex: "aa11", defaults: defaults))
        XCTAssertNil(CallPeerNameMirror.name(accountIdB64: nil, groupIdHex: "aa11", defaults: defaults))
        CallPeerNameMirror.mirror(accountIdB64: "acct-B", groupIdHex: "aa11", name: "Bo", defaults: defaults)
        XCTAssertEqual(
            CallPeerNameMirror.name(accountIdB64: "acct-A", groupIdHex: "aa11", defaults: defaults), "Anna")
        XCTAssertEqual(
            CallPeerNameMirror.name(accountIdB64: "acct-B", groupIdHex: "aa11", defaults: defaults), "Bo")
    }

    /// Forget-device drops the forgotten account's names (and the old
    /// device-wide rows) and nothing else.
    func testForgetLeavesNoNameOfThatAccountBehind() {
        let defaults = transientDefaults()
        CallPeerNameMirror.mirror(accountIdB64: "acct-A", groupIdHex: "aa11", name: "Anna", defaults: defaults)
        CallPeerNameMirror.mirror(accountIdB64: "acct-A", groupIdHex: "bb22", name: "Bo", defaults: defaults)
        CallPeerNameMirror.mirror(accountIdB64: "acct-B", groupIdHex: "aa11", name: "Cy", defaults: defaults)
        defaults.set("Old", forKey: CallPeerNameMirror.legacyKeyPrefix + "aa11")
        CallPeerNameMirror.clear(accountIdB64: "acct-A", defaults: defaults)
        XCTAssertNil(CallPeerNameMirror.name(accountIdB64: "acct-A", groupIdHex: "aa11", defaults: defaults))
        XCTAssertNil(CallPeerNameMirror.name(accountIdB64: "acct-A", groupIdHex: "bb22", defaults: defaults))
        XCTAssertEqual(
            CallPeerNameMirror.name(accountIdB64: "acct-B", groupIdHex: "aa11", defaults: defaults), "Cy")
        XCTAssertNil(defaults.object(forKey: CallPeerNameMirror.legacyKeyPrefix + "aa11"))
    }

    func testAnUnprovisionedAppGroupIsNotAFailure() {
        // A build without the App Group entitlement has no shared suite; the
        // mirror is then simply empty and the alert unnamed.
        CallPeerNameMirror.mirror(accountIdB64: "acct", groupIdHex: "aa11", name: "Anna", defaults: nil)
        XCTAssertNil(CallPeerNameMirror.name(accountIdB64: "acct", groupIdHex: "aa11", defaults: nil))
    }

    private func transientDefaults() -> UserDefaults {
        let suite = "me.lakko.tests.callpeer.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        defaults.removePersistentDomain(forName: suite)
        return defaults
    }
}

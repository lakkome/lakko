// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import BackgroundTasks
import Foundation

/// Channel posts read while Lakko is in the background
/// (`docs/ux/channel-notifications.md` section 3, iOS): a `BGAppRefreshTask`
/// wakes the app, one channel post sync runs, and the notifier shows what the
/// rule picks. The handler is registered on the scene via
/// `.backgroundTask(.appRefresh(_:))` in ``LakkoApp``.
///
/// Opportunistic, not guaranteed: iOS decides if and when the task runs
/// (usage, battery, network), so a notification can come late or not at all
/// until the app is opened; Settings > Notifications says so in one line.
///
/// The identifier must be in `Info.plist`'s
/// `BGTaskSchedulerPermittedIdentifiers` (`project.yml`), with the `fetch`
/// background mode, or the OS refuses to launch the task.
enum ChannelPostBackgroundRefresh {

    /// BGTask identifier, reverse-DNS under the app bundle id.
    static let taskIdentifier = "me.lakko.dev.channel-posts"

    /// Earliest the OS should consider the next run. 15 minutes is the
    /// practical floor iOS honours for an app refresh task.
    private static let earliestInterval: TimeInterval = 15 * 60

    /// How long one run may take. An app refresh task gets about 30 seconds
    /// (Apple's documentation, not measured here); the run stops before.
    static let budgetSeconds: UInt64 = 25

    /// Submit a request for the next run. Best-effort: a refusal (too many
    /// pending requests, Low Power Mode, a simulator) is logged and
    /// swallowed; the foreground sync still covers the open app.
    static func schedule() {
        let request = BGAppRefreshTaskRequest(identifier: taskIdentifier)
        request.earliestBeginDate = Date(timeIntervalSinceNow: earliestInterval)
        do {
            try BGTaskScheduler.shared.submit(request)
        } catch {
            NSLog("[feed] channel BGAppRefresh submit failed")
        }
    }

    /// What one run needs, injectable for tests.
    struct Inputs {
        /// The signed-in account, or `nil` (no session: nothing runs).
        var accountIdB64: () -> String?
        /// The started session sync, when the session root is alive.
        var activeSync: () -> ChannelPostSync?
        /// One sync for `accountIdB64` built for this run when no session
        /// sync is alive (a launch into the background).
        var makeSync: (_ accountIdB64: String) -> ChannelPostSync
        /// Whether this run may use the network: anti-censorship off, or on
        /// and armed in this process. On but not armed (a launch into the
        /// background before the app armed it), nothing is sent: a request
        /// would go direct, around the tunnel the person chose.
        var mayUseNetwork: () -> Bool = { true }
    }

    /// One run: the session's own sync when it is alive for the account,
    /// else a sync built for this run; either way one writer of the log at a
    /// time (``ChannelPostLogWriter``). Stops at the
    /// budget; nothing is reported, a failure waits for the next run.
    /// Returns whether a sync ran.
    @MainActor
    @discardableResult
    static func run(_ inputs: Inputs, budgetSeconds: UInt64 = budgetSeconds) async -> Bool {
        guard let account = inputs.accountIdB64() else { return false }
        guard inputs.mayUseNetwork() else { return false }
        let sync: ChannelPostSync
        if let active = inputs.activeSync(), active.account == account {
            sync = active
        } else {
            sync = inputs.makeSync(account)
        }
        await withTaskGroup(of: Void.self) { group in
            group.addTask { @MainActor in _ = await sync.syncAndWait() }
            group.addTask {
                try? await Task.sleep(nanoseconds: budgetSeconds * 1_000_000_000)
            }
            // The first to finish ends the run; the other is cancelled. A
            // sync cut short keeps what it read before the cut (each channel
            // it had not reached is read from the same cursor next time).
            // Built or not, the sync runs under the process-wide writer lock
            // (``ChannelPostLogWriter``): a session sync that starts while
            // this one runs waits for it, and reads the log it saved.
            await group.next()
            group.cancelAll()
        }
        return true
    }

    /// One run with the live inputs (the handler in ``LakkoApp``).
    @MainActor
    static func runLive(authRepository: AuthRepository, deviceGroupSync: DeviceGroupSyncRepository) async {
        await run(live(authRepository: authRepository, deviceGroupSync: deviceGroupSync))
    }

    /// The live inputs: the account from the session (or the persisted one
    /// when the app was launched into the background and the session has not
    /// been restored), and a sync built with the session's notifier wiring.
    @MainActor
    static func live(
        authRepository: AuthRepository,
        deviceGroupSync: DeviceGroupSyncRepository
    ) -> Inputs {
        Inputs(
            accountIdB64: {
                if case let .session(account) = authRepository.state { return account.accountIdB64 }
                return SessionStore().load()?.accountIdB64
            },
            activeSync: { ChannelPostSync.active },
            makeSync: { account in
                let channels = ChannelsUiState(accountIdB64: account)
                let sync = ChannelPostSync(accountIdB64: account, channels: channels)
                sync.accountIdentitySeed = { authRepository.identitySecrets?.accountIdentitySeed }
                let reading = FeedReadingModel(accountIdB64: account)
                // As in the session: a channel read for the first time gets
                // its read marker at the newest post (held strongly here, so
                // the model lives as long as this run's sync).
                sync.onSlotSynced = { channelId, slot in
                    reading.update { $0.seedingChannelReads([channelId: slot]) }
                }
                ChannelPostNotifier.attach(
                    to: sync, channels: channels, reading: reading,
                    flags: { deviceGroupSync.ready ? deviceGroupSync.flags : nil })
                return sync
            },
            mayUseNetwork: {
                !AntiCensorshipManager.shared.isEnabled || AntiCensorshipConfig.current.armed
            })
    }
}

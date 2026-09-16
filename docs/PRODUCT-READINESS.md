<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Product readiness — can an ordinary person use Lakko today?

Date: 2026-09-15. Scope: the three clients as built in this repository and the
test deployment on `test.lakko.me`. Method: five independent static audits of
the code and the deployment configuration, each forbidden from citing an ADR,
CLAUDE.md or the roadmap as evidence and required to cite `file:line` for every
verdict. Two findings were additionally observed live with two browsers on one
account. Nothing here is a sprint or phase status. The only question asked is
whether a non-technical user can rely on the feature.

**Verdict: no.** Lakko is not usable by ordinary people on any platform today.
The security core is real and largely well built, but the product around it
loses user data on routine actions, does not exist on a second device, cannot
ring a phone that is in a pocket, cannot be installed from a store, and runs
on a backend with no backups, no rate limits, no CI on merge and unauthenticated
write endpoints.

This document replaces per-ADR "built end-to-end" claims as the statement of
what users get. Where the two disagree, this document is correct and the ADR
row describes the scope of one change, not the product.

## 1. The acceptance standard

Written down because it was never written down before, which is how the gaps
below survived four phases of work.

1. A user who signs in on a **second device** gets a full client: identity,
   contacts, every existing 1x1 and group chat with history, channels, call
   history, and incoming messages and calls, without the first device being
   online.
2. A user who **loses every device** restores everything the server can carry
   from the recovery phrase (or three words, or a passkey): identity, contacts,
   channels, and whatever history the backup held. Chats they were in are
   re-joined by their contacts; the UI says so.
3. Nothing a user does in the app is **silently undone** by a reload, a
   restart, or a second tab.
4. A **call rings** on a phone in a pocket and on a laptop with the tab in the
   background, with sound.
5. The app is **installable** from the App Store, Google Play and as a PWA,
   with an icon, a name, signing, and deep links that open it.
6. An operator can run the service for the public: backups, rate limits,
   monitoring, an account deletion that deletes, terms and privacy pages, and
   a CI gate on what gets deployed.

## 2. Readiness by capability

Updated 2026-09-16 after the client redesign (docs/ux/client-redesign.md,
phases A and B on all three clients) and the native stabilisation pass: the
rows for 1x1 text, group creation, settings, second device and store install
changed; every other row is as observed on 2026-09-15.

Legend: **OK** = complete chain verified in code. **PARTIAL** = works with a
named defect a user will hit. **NO** = not usable or does not exist. **?** =
not verifiable from the repository.

| Capability | Web | iOS | Android | Evidence and defect |
|---|---|---|---|---|
| Sign up, log in, silent resume | OK | PARTIAL | PARTIAL | Web: 6 sequential proof-of-work solves per signup with only an elapsed-seconds indicator (`apps/web/src/auth/client.ts`, `discovery/client.ts`); a tab closed mid-signup leaves a half-created account (`AuthContext.tsx`). Natives: until 2026-09-16 the recovery phrase was NEVER shown after sign-up on either platform, because `AuthRepository.signup` switched the state to the session before the phrase screen could render, so every native account created so far has no saved phrase (`md0916i`, `md0916d` included); fixed the same day by parking the phrase in a gate the session root renders first, verified by build and tests, not yet by a live sign-up on a device |
| Recovery: 24 words, TOTP, email/SMS code, Shamir | OK | OK | OK | Email/SMS delivery is `LogOnly` on the test stack: codes appear only in `journalctl` (`services/auth/src/otp_sender.rs:117-123`); no SMTP/SMS vars in `infra/test-stack/setup-server.sh` |
| Recovery: three words, confirm on another device, passkey vault (ADR-0049) | OK | NO | NO | No UniFFI surface for `word_vault_*` / `passkey_vault_*`; no native client for `/device/requests` |
| Change password | NO | NO | NO | Does not exist. `/password/reset` is wired only to the forgot-password flow, which requires the phrase and **deletes the local MLS state** (`AuthContext.tsx:2601-2616`) |
| Change username, claim username as guest | NO | NO | NO | `registerUsername` is called only at signup (`AuthContext.tsx:1239`); guests stay undiscoverable (`:1498-1503`) |
| Recovery phrase shown again after signup | NO | NO | NO | No screen; a reload during the signup upload window loses it for good (`AuthContext.tsx`). Since 2026-09-16 the sign-up phrase step on all three asks the user to re-enter three random words before continuing, so a phrase that was never written down no longer passes the step |
| **Second device: chats and groups** | PARTIAL | PARTIAL | PARTIAL | **G-40, ADR-0050.** Web: every device is its own conversation leaf bound to the account; a chat created after the second device exists reaches both devices; a device added by QR or by the ADR-0049 confirmation is admitted to every existing chat by the confirming device (`chat/BackgroundReceiveContext.tsx` `admitDevice`). A device restored by three words / phrase / passkey is admitted by any member's scheduled re-admission check once it has published a KeyPackage (`chat/device-readmission.ts`); history and contacts reach a paired device through the pairing bundles, a device restored with no other device alive gets neither. Natives (ADR-0050 S5, 2026-09-16): per-device leaves, all-devices group create, QR-pairing fan-out, removal fan-out, re-admission and the state bundle are built and unit-tested; on 2026-09-16 both natives ran live for ordinary chat (contact request, one-commit chat creation, messages both ways with web and with each other) but the pairing fan-out and the re-admission arm were NOT exercised on a device (the only native entry is a QR scan, and the simulator and emulator have no camera); no ADR-0049 confirmation prompt on either native, and iOS re-admits only chats that get opened |
| Second device: contacts, channels, call history | PARTIAL | PARTIAL | PARTIAL | Web, iOS, Android (natives unit-tested only): contacts, renames and pins travel in the pairing state bundle (ADR-0050 2.9); a contact accepted after the pairing does not reach the other device (DeviceGroup sync kinds unbuilt). Channels: the list does not survive a reload on this device, so nothing is carried (`state/channels-state.ts:98`). Call history does not exist on any client (`routes/Calls.tsx:5`) |
| Backup that reflects current state | NO | NO | NO | Uploaded exactly twice per account, at signup (empty) and at recover (`AuthContext.tsx:1456,2862`); `uploadFromCache` has no callers; no button, no schedule |
| Restore gate (ADR-0037) on a same-device recover | NO | ? | ? | Device key rotation makes the floor unreadable → "Restore this older backup?" fires on every recover; Cancel silently skips the restore and overwrites the server blob with an empty baseline (`backup/floor-store.ts:148-154`, `head-gate.ts:113-116`, `AuthContext.tsx:2804-2811,2862`) |
| Log out / remove device revokes the server session | NO | NO | NO | Stateless 30-day HMAC token, no revocation list (`crates/lakko-auth-token/src/lib.rs:49-55`); `removeDeviceAt` calls no auth endpoint |
| Delete account deletes everything | NO | NO | NO | Backup blob never deleted (`deleteBackup` has no callers on any client); attachment blobs have no delete route; push tokens and DS journals untouched; discovery purge is best-effort in a `catch {}` (`auth/AccountSections.tsx:136-171`) |
| 1x1 text: send, receive, order, unread | OK | OK | OK | Receive loop pauses when the tab is hidden and the push wake it receives is ignored by the app (`Chat.tsx:3012`, `public/sw.js:269` vs `push/WakeNavigator.tsx:105`). iOS-Android and web-Android exchanges in both directions verified live on 2026-09-16 on the redesigned clients; two native defects found that day are fixed (an iOS chat that lost typed characters and flickered because the shell rebuilt its view model on every repository publish; an Android ANR on opening a chat because the poll loop spun on the main thread) |
| Failed send | NO | ? | ? | Bubble is deleted and a raw `ds ... network error: TypeError` is shown; no outbox, no retry for text or files (`Chat.tsx:3146-3156`) |
| Offline indicator | NO | NO | NO | No `navigator.onLine` or equivalent anywhere |
| Two tabs / same account | NO | n/a | n/a | Known broken, stated in code: tab B clobbers tab A's MLS snapshot (`mls/group-lock.ts:133-139`); no guard |
| Edit, delete, reactions | PARTIAL | OK | OK | Web: applied live, **never persisted**; after a reload the pre-edit text and deleted messages come back on every device, forever (`Chat.tsx:558-570,7708-7712`) |
| Read receipts | PARTIAL | OK | OK | Web: session state, all revert to "Sent" on reload (`Chat.tsx:586-592`) |
| Typing indicator | OK | OK | OK | |
| Reply/quote, forward, copy, in-chat search, pin message | NO | NO | NO | None exist |
| Group: create with a name | PARTIAL | NO | NO | Web creates with the first pick and adds the rest one by one; the name lives in memory only, becomes `Group aB3xY9` on reload and is never sent to members (`chat/rehydrate.ts:248`, `contacts/MailboxContext.tsx:165`); no rename anywhere. Android (2026-09-16): a New group screen that stores the name locally only and still adds members one by one; iOS: New group opens a contact picker and then the members panel, no name at all; neither native shows hex ids any more, an unnamed group is titled from its members |
| Group: add, remove, roles (admin) | OK | OK | OK | Owner/admin only, as designed |
| Group: leave | PARTIAL | OK | OK | Web: leave is a proposal an admin must apply; the owner's Leave button is permanently disabled until transfer (`Chat.tsx:1528,5854-5866`) |
| Group invite link `/g/` | PARTIAL | PARTIAL | OK | Web: the join completes only while the join page stays open (`JoinGroup.tsx:149` is the only poller); pending-request list lost on reload (`Chat.tsx:5286-5297`). iOS: Universal Links cannot resolve, no `apple-app-site-association` exists, no URL scheme |
| Contacts: search, invite link, accept | PARTIAL | PARTIAL | PARTIAL | Three defects found and fixed on 2026-09-16, the natives' fix not yet re-run live: (1) web signed a contact request with the DEVICE identity whenever the route's client had no account-identity provider (`contacts/client.ts` `resolveAccountIdentity` fell back to the canonical device identity), so the acceptor's acknowledgement went to the device mailbox where only Welcomes were read and the requester never got the contact (verified fixed live: web request, iOS accept, contact on web); (2) iOS ignored acknowledgements outright (`ContactsRepository.swift` `.contactAck` was a `continue`), so a requester got the chat but never the contact; (3) both natives kept the stranger-inbox cursor in memory only, so every relaunch re-fetched every request ever received, accepted people came back under Requests, declined ones returned, and duplicated rows crashed the redesigned Android list. Still true: web decline sends nothing (`Contacts.tsx`); unanswered incoming requests are lost on a web reload; no badge for pending requests; `/i/` for anonymous visitors requires re-opening the link after login (`routes/Invite.tsx`) |
| Block, report, delete contact | NO | NO | NO | Block and report do not exist anywhere in the repo; web "delete contact" edits `localStorage` only |
| Attachments: images, files | OK | OK | OK | Web: no thumbnails for images, nothing renders until "Open" (`Chat.tsx:7446-7462`) |
| Voice message, video note | PARTIAL | OK | OK | Web: MP4/AAC only; unavailable on browsers that cannot record MP4 (`media/capture-format.ts:20-35`) |
| GIF picker | OK (code) | OK (code) | OK (code) | Provider key commented out on the test stack → 503 (`setup-server.sh:81`) |
| Emoji picker, profile display name | OK | OK | OK | Avatar images do not exist on any client |
| Drafts, pin, mute, archive | NO | ? | ? | Web: synced to sibling devices, **never stored locally**; all lost on reload (`devices/sync/DeviceGroupSyncContext.tsx:163-165`) |
| Notifications: web push with decrypted preview, tap opens chat | OK | n/a | n/a | Requires VAPID at build time (test stack has it). Mute flag not honoured by the service worker; no aggregate unread badge; no message sound |
| Notifications: APNs / FCM | n/a | NO | NO | Client code complete; server reads `LAKKO_APNS_*` / `LAKKO_FCM_SA_PATH`, the production unit sets `APNS_KEY_PATH` / `FCM_SERVICE_ACCOUNT` (dead names, `infra/systemd/lakko-svc-push-gw.service:26-28`), the test stack sets neither → `LogOnly` |
| 1x1 audio/video call, both apps in foreground | PARTIAL | PARTIAL | PARTIAL | Web: connects; **no ringtone plays** (cue exists, never called, `CallHostContext.tsx:420-425`); **no TURN in the web path at all**, only public Google STUN (`call-session.ts:219-222`); natives never call `setTurnClient` |
| Call rings in background / on a locked phone | NO | NO | NO | Push preview drops call signaling (`public/sw-push-decrypt.js:459-460`, `NsePushDecrypt.swift:595`, `PushServiceEnvelopeFilter.kt:121`); no CallKit/PushKit, no ConnectionService |
| Missed calls, call history | NO | NO | NO | No store; `/calls` is a placeholder; natives have no Calls tab |
| Group call via the SFU | OK (code) | OK (code) | OK (code) | Never verified against a deployed SFU from a browser or a phone; the only deployed-SFU test checks a WebSocket welcome frame and is `#[ignore]` (`services/sfu/tests/e2e_dockerized.rs:142`); no quality indicator on the SFU path |
| Screen share, speaker selection, raise hand, in-call chat | NO | NO | NO | Local booleans only; no `getDisplayMedia`, no `setSinkId`; deployment header forbids display capture |
| Standalone `/call/:id` route | NO | n/a | n/a | Entirely mock, including a fabricated green E2EE fingerprint (`routes/Call.tsx:1777-1849`); reachable by URL |
| Call links `/c/` | PARTIAL | PARTIAL | PARTIAL | Works via a guest account that must save a phrase first; no TURN; Android deep link registered for `lakko.me` only |
| Channels: create, post, subscribe, read, co-publish | PARTIAL | PARTIAL | PARTIAL | Redesigned on 2026-09-16 (list rows, post cards, manage screen; no ids, epochs or keys shown), behaviour unchanged. **Channel list does not survive a reload on any client**; on web the creator's publisher key is not persisted, so they cannot post to their own channel after a reload (`state/channels-state.ts:342-345`); **unsubscribe does not exist** on the server (`services/channels/src/api.rs:1017-1042`) |
| Settings: theme, notifications, privacy | PARTIAL | PARTIAL | PARTIAL | Since 2026-09-16 all three have a Settings screen with profile, passkeys, devices, recovery, theme (system / light / dark), autoplay and, on the natives, the anti-censorship toggle; the home screen with the raw account id is gone. Notification and privacy settings do not exist on any client |
| Safety number / contact verification | PARTIAL | ? | ? | Web shows 60 digits; no QR, no "mark verified", no badge. Key-transparency check is off unless two build vars are set; unset by default and not set by the test-stack deploy |
| Localisation | NO | NO | NO | English literals only, on all three |
| Accessibility | PARTIAL | ? | ? | Web: no live region on the message list, no keyboard handling in the chat at all |
| Install from a store | n/a | NO | NO | Bundle id `me.lakko.dev` on both; no signing team, no keystore, no fastlane, no CI archive; **Release builds point at `http://localhost:8081`** (`project.yml:66-67`, `build.gradle.kts:340-341`); no app icon (the iOS asset catalogue added on 2026-09-16 has an empty icon set, Android uses the system default); `aps-environment: development` |
| Install as PWA | OK | n/a | n/a | |

## 3. Backend and operations

| Area | State | Evidence |
|---|---|---|
| Database backups, migration rollback | **None.** No script, cron or timer; all migrations forward-only and auto-applied at boot | `find infra -type f`; `services/auth/src/main.rs:254` |
| Authentication on write endpoints | **Missing on three services.** `channels` (create, post, subscribe, rotate), `storage` blob `PUT`, `ds` journal `POST`/`GET` take no token | `services/channels/src/api.rs:1015-1043`; `services/storage/src/main.rs:200-212`; `services/ds/src/main.rs:152-158` |
| Edge rate limiting | **One `limit_req_zone` in nginx, for the SFU handshake.** Signup, login, OPRF, blob upload, channel publish have none; PoW is 8 bits in prod and **0 on the test stack** | `infra/nginx/sites-available/lakko.conf`; `infra/test-stack/setup-server.sh:67-69` |
| CI on merge | **Zero checks run on a push to `main`.** Every workflow but `spec-drift.yml` (PR only) is `workflow_dispatch` | `.github/workflows/ci.yml:44-49` |
| Monitoring / alerting | **API tier has none.** Only the SFU exposes `/metrics`; the Prometheus config is not installed, points at hosts that do not exist and at a LiveKit metric the str0m SFU never emits | `infra/prometheus/prometheus.yml:39,102-104`; `rules/calls.yml:84` |
| Storage quota, blob GC, disk alarm | None; unauthenticated 50 MB uploads with no cap | `services/storage/src/main.rs:20-28,91` |
| Account deletion server-side | Auth and discovery purge; backup blob, attachment blobs, push tokens, DS journals survive | see section 2 |
| Terms, privacy policy, consent, data export UI | **None** | `apps/web/src/routes/` |
| Production env template | Omits every boot-required secret (`LAKKO_SESSION_TOKEN_SECRET`, TOTP/vault/pepper keys, WebAuthn, blob dir); push-gw unit uses dead variable names | `infra/etc-lakko/lakko.env.example`; `lakko-svc-push-gw.service:26-28` |
| Logging | Recovery OTP codes at `info` when no sender is configured (the current state); `account_id` at `info` in auth; no journald cap or logrotate in repo | `services/auth/src/otp_sender.rs:117-123`; `opaque.rs:823` |
| Key transparency, witness, obfs-proxy | Complete code, **no systemd unit, no vhost, not in any deploy script**, in either environment | `infra/systemd/`, `infra/scripts/deploy.sh`, `infra/test-stack/deploy.sh` |
| External cryptographic review of `lakko-mls` | Not done; release gate G-39 | `docs/security/known-gaps.md` |

## 4. What is not in doubt

So that the list above is not read as "nothing works". Verified in code and,
where marked, live:

- OPAQUE signup and login, WebAuthn passkeys, TOTP, email/SMS OTP plumbing,
  Shamir shares, the 24-word phrase, the three-word vault (live), the
  camera-less device confirmation (live, two devices), the passkey PRF vault
  (code only).
- 1x1 and group MLS messaging with the proposal-based membership model,
  peer-verified roles, author-bound edit/delete, typing and read receipts.
- Encrypted attachments, voice messages and video notes, GIF and emoji
  pickers, profile names carried end-to-end encrypted.
- Web push with a real decrypted preview in the service worker; iOS NSE and
  Android background decrypt code paths.
- 1x1 WebRTC calls on all three clients with real mute/camera controls; the
  SFU group-call path with SFrame on all three clients (in-process media
  tests pass; a live browser-to-SFU round trip has never been run).
- Channels create/post/subscribe/co-publish within a session.
- The transport stack (REALITY, Shadowsocks-2022, WebTransport, ECH) with the
  obfs proxy, wired into the native HTTP and WebSocket clients.
- The key-transparency log, witness co-signing, supersession proofs, genesis
  attestation and backup-head primitives, all with tests and fuzz targets.

Almost everything in this list is a protocol or a primitive. Almost
everything in sections 2 and 3 is the product around it.

## 5. What has to happen, in order

No dates and no sizes; the order is by what blocks what and by how much a
normal user loses.

**A. Stop losing data on routine actions (web first).** Persist edit, delete,
reaction and read-receipt ops; persist group names and send them to members;
store drafts, pins, mutes, archive locally; persist pending contact requests
and pending join requests; persist the channel list and the publisher key;
add a send outbox with retry and an offline indicator; guard against a second
tab. Each is a contained change with no protocol work.

**B. Make a second device a real client.** ADR-0007 Phase 2b: per-device
conversation leaves with a peer-verified device-to-account binding,
per-device KeyPackages in discovery, the existing device adding the new one to
every conversation on pairing or confirmation, and device-to-account
resolution at every site that today equates a leaf with a member (ADR-0022
roles, ADR-0023 authorship, ADR-0042 routing authority, ADR-0021 peer tags).
Then: history bundle landing under the joined groups; contacts and channels
carried by the DeviceGroup or by a backup that actually runs. `lakko-mls`
change: ADR plus property tests, and the G-39 review scope grows.

**C. Make recovery and backup trustworthy.** Automatic backup on change plus a
manual button and a visible last-backup time; a change-password flow that does
not destroy local state; fix the ADR-0037 floor so a same-device recover does
not raise a rollback warning, and make Cancel visible; a screen that shows the
recovery phrase again; a visible strike counter on the three-word vault;
server-side session revocation on logout, device removal and account deletion;
account deletion that deletes the backup blob and push rows; a username
cooldown after deletion.

**D. Make calls ring.** Play the ringtone; carry call signaling through push
(web notification with actions; iOS PushKit + CallKit; Android
ConnectionService or a foreground service); request TURN credentials from the
web and native clients; store call history and missed calls; remove or gate
the mock `/call/:id` route and the fake E2EE state; run one live browser-to-SFU
group call on the test stack and keep the test.

**E. Ship the natives.** Production bundle ids, signing, provisioning,
fastlane or equivalent, a CI archive; Release configuration pointing at real
endpoints; icons and launch screens; `apple-app-site-association` and
`assetlinks.json`, a URL scheme, links pointing at the deployed host; a real
home and settings surface; the two web-only ADR-0049 routes on UniFFI and in
both apps; APNs and FCM provider configuration under the names the code reads;
`allowBackup=false` on Android.

**F. Run it for the public.** Tokens on every write endpoint; per-IP rate
limits at nginx and real PoW effort; nightly database and blob backups with a
tested restore; CI on push and PR with branch protection; metrics and alerts
for the API tier and MariaDB; log caps and no secrets at `info`; terms,
privacy and consent; deploy transparency and witness with the client pins set;
a correct production env template.

**G. Table-stakes messenger features.** Block and report; reply/quote,
forward, copy, in-chat search; avatars; group rename; member self-leave that
completes; badges and a message sound; localisation.

**H. The G-39 external review** before any build reaches users, with the
surface after B included.

## 6. How this was missed

Every ADR reported its own scope as done, and each was. The roadmap's Phase 2a
row reads "multi-device sync via DeviceGroup substrate", which the substrate
satisfied. No document stated the standard in section 1, no phase gate tested
it, and the assistant's end-of-sprint menus offered the next roadmap item
rather than a product walk-through. `docs/security/known-gaps.md` tracked
security residuals and never product gaps, so a user-visible defect with no
security consequence had nowhere to be recorded. This file is that place until
the items in section 5 are closed; it must be updated with every change that
moves a row in section 2, and it must never say a row is OK on the strength of
an ADR.

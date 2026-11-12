<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# In-app icon inventory

One row per Lakko icon: what it means, and what each client draws for that meaning today. The Lakko name is the file in [`svg/`](svg/); the drawing rules are in `.claude/skills/ux-ui/design-system.md`, section "Icons". Gathered on 2026-11-12 from `main`.

How the counts were taken:

- **iOS**: every Swift string literal in `apps/ios/Lakko` and `apps/ios/LakkoNotificationService` that `NSImage(systemSymbolName:)` accepts, with comments and non-icon strings (dictionary keys, media kinds, file-name fallbacks) removed by reading each site. This catches names held in variables, ternaries and `switch` returns, so a ternary such as `on ? "mic.fill" : "mic.slash.fill"` counts once for each name. A count is the number of literals, not the number of screens.
- **Android**: every `Icons.<style>.<Name>` reference in `apps/android/app/src/main` (305). Where one Material icon serves two meanings it is split by meaning, marked `@meaning`, with the count read at each site. The two drawables `ic_call_pip_mic` / `ic_call_pip_mic_off` are listed too. The notification actions use `android.R.drawable.sym_action_call` and `ic_menu_close_clear_cancel`; those are system resources inside a system notification and are not part of this set.
- **Web**: names from `apps/web/src/ui/icons.tsx`; the count (~) is the number of source lines that mention `icon` and the name in quotes, so it is approximate (it misses names returned from helper functions and may include a few unrelated strings, notably for `video` and `phone`).
- `*name` (iOS): the tab bar draws the `.fill` form of a symbol when the tab is selected; the code names only the outline.

175 icons: 140 outline and 35 filled variants.

## Tab bar

| Lakko icon | Meaning | iOS (SF Symbol) | Android (Material) | Web |
|---|---|---|---|---|
| `chat` | Chats tab; New chat; message a contact; speech-bubble feed icon | `bubble.left` (3) | `Outlined.ChatBubbleOutline` (1), `Filled.ChatBubble` (2), `AutoMirrored.Filled.Chat` (1) | `chat` (~5) |
| `chat-fill` | Chats tab when selected (drawn by the iOS tab bar); Message button on a contact | `bubble.left.fill` (1) | none | none |
| `feed` | Feed tab; Latest preset; feed rule icon | `newspaper` (3) | `Outlined.DynamicFeed` (1), `Filled.DynamicFeed` (2) | `feed` (~3) |
| `feed-fill` | Feed tab when selected | `newspaper.fill` (selected tab, drawn by the tab bar) | none | none |
| `person-square` | Contacts tab; Profile row; account rows | `person.crop.circle` (5) | `Outlined.People` (1), `Filled.People` (1), `Filled.Person@profile` (1) | `contacts` (~2), `user@profile` (~1) |
| `person-square-fill` | Contacts tab when selected | `person.crop.circle.fill` (selected tab, drawn by the tab bar) | none | none |
| `phone` | Calls tab; voice call; voice call in history | `phone` (4) | `Outlined.Call` (1), `Filled.Call` (8) | `calls` (~2), `phone` (~11) |
| `phone-fill` | Calls tab when selected; accept a call; voice call kind | `phone.fill` (5) | none | none |
| `gear` | Settings tab | `gearshape` (1) | `Outlined.Settings` (1), `Filled.Settings` (2) | `settings` (~2) |
| `gear-fill` | Settings tab when selected; call settings control | `gearshape.fill` (1) | none | none |

## Navigation

| Lakko icon | Meaning | iOS (SF Symbol) | Android (Material) | Web |
|---|---|---|---|---|
| `chevron-back` | Back; collapsed call window tucked at the end edge | `chevron.backward` (5), `chevron.left` (1) | `AutoMirrored.Filled.ArrowBack` (40), `AutoMirrored.Filled.KeyboardArrowLeft` (1) | `back` (~20) |
| `chevron-forward` | Row disclosure; collapsed call window tucked at the start edge | `chevron.forward` (17), `chevron.right` (3) | `AutoMirrored.Filled.KeyboardArrowRight` (5), `Filled.ChevronRight` (1) | `chevron` (~14) |
| `chevron-up` | Collapse a section; slide up to lock recording | `chevron.up` (2) | `Filled.ExpandLess` (2) | none |
| `chevron-down` | Expand a section; collapse the call | `chevron.down` (3) | `Filled.ExpandMore` (3), `Filled.KeyboardArrowDown@call` (1) | none |
| `chevron-up-down` | Picker field | `chevron.up.chevron.down` (1) | `Filled.KeyboardArrowDown@picker` (1) | none |

## Marks and status

| Lakko icon | Meaning | iOS (SF Symbol) | Android (Material) | Web |
|---|---|---|---|---|
| `xmark` | Close; dismiss; cancel reply | `xmark` (11) | `Filled.Close` (12) | `close` (~12) |
| `xmark-circle-fill` | Remove a pending attachment; clear search text | `xmark.circle.fill` (2) | `Filled.Clear` (1) | none |
| `checkmark` | Chosen row; copied; message sent | `checkmark` (17) | `Filled.Check` (8) | `check` (~10) |
| `checkmark-double` | Message read (iOS stacks two checkmarks) | `checkmark x2` (1) | `Filled.DoneAll` (1) | `dcheck` (~3) |
| `circle` | Unselected item in a multi-select list | `circle` (8) | none | none |
| `checkmark-circle` | Select (message menu) | `checkmark.circle` (1) | none | `select` (~1) |
| `checkmark-circle-fill` | Selected item; a step done | `checkmark.circle.fill` (13) | `Filled.CheckCircle` (2) | none |
| `radio-on` | Chosen option in a single-choice list | `largecircle.fill.circle` (1) | none | none |
| `bullet` | Bullet in a feed rule summary | `circle.fill` (1) | none | none |
| `plus` | Add | `plus` (4) | `Filled.Add` (3) | `plus` (~12) |
| `minus-circle` | Remove (reaction, reading language) | `minus.circle` (2) | none | none |
| `minus-circle-fill` | Remove a chat from a folder being edited | `minus.circle.fill` (1) | none | none |
| `ellipsis` | More, inside a row | none | none | `more` (~3) |
| `ellipsis-circle` | More, in a toolbar | `ellipsis.circle` (1) | `Filled.MoreVert` (1) | none |
| `info-circle` | About; information | `info.circle` (3) | `Filled.Info` (2) | `info` (~3) |
| `exclamation-circle` | Error; something needs attention | `exclamationmark.circle` (3) | `Outlined.ErrorOutline` (1) | none |
| `exclamation-circle-fill` | Attention badge (new device) | `exclamationmark.circle.fill` (1) | none | none |
| `warning` | Refresh failed | `exclamationmark.triangle` (1) | none | none |
| `warning-fill` | Warning in a device or passkey flow | `exclamationmark.triangle.fill` (7) | none | none |
| `circle-slash` | No background (calls) | `circle.slash` (1) | none | none |
| `nosign` | Ban from a channel | `nosign` (1) | `Filled.Block` (1) | none |

## Arrows

| Lakko icon | Meaning | iOS (SF Symbol) | Android (Material) | Web |
|---|---|---|---|---|
| `arrow-down` | Jump to newest (call chat) | `arrow.down` (1) | none | none |
| `arrow-up-right` | Open (feed item) | `arrow.up.right` (1) | none | none |
| `arrow-up-down` | Reorder folders | `arrow.up.arrow.down` (1) | none | none |
| `drag-handle` | Drag to reorder | none | `Filled.DragHandle` (1) | `grip` (~1) |
| `arrow-clockwise` | Retry a failed download | `arrow.clockwise` (1) | none | none |
| `arrow-up-circle-fill` | Send (compact composers: call chat, comments, pending chat) | `arrow.up.circle.fill` (3) | none | none |
| `arrow-down-circle-fill` | Update available | `arrow.down.circle.fill` (1) | `Filled.ArrowCircleDown` (1) | none |
| `arrow-down-square` | Message from a newer app version | `arrow.down.app` (1) | none | none |

## Message and edit actions

| Lakko icon | Meaning | iOS (SF Symbol) | Android (Material) | Web |
|---|---|---|---|---|
| `pencil` | Edit; rename | `pencil` (9) | `Filled.Edit` (8) | `pencil` (~12) |
| `compose` | New chat / create menu | `square.and.pencil` (1) | `Filled.Edit@chat-list` (1) | `compose` (~2) |
| `trash` | Delete; deleted message; forget this device | `trash` (12) | `Filled.Delete` (10), `Filled.DeleteForever` (1) | `trash` (~10) |
| `copy` | Copy | `doc.on.doc` (8) | `Filled.ContentCopy` (7) | `copy` (~8) |
| `paste` | Paste the recovery phrase | `doc.on.clipboard` (1) | `Filled.ContentPaste` (1) | none |
| `search` | Search | `magnifyingglass` (1) | `Filled.Search` (3) | `search` (~11) |
| `zoom-in` | Zoom in (photo crop) | `plus.magnifyingglass` (1) | none | none |
| `zoom-out` | Zoom out (photo crop) | `minus.magnifyingglass` (1) | none | none |
| `link` | Invite link | `link` (3) | `Filled.Link` (2) | `link` (~4) |
| `link-plus` | New invite link | `link.badge.plus` (1) | none | none |
| `qrcode` | Show a QR code | `qrcode` (1) | `Filled.QrCode` (2) | `qr` (~2) |
| `qrcode-scan` | Scan a QR code | `qrcode.viewfinder` (1) | `Filled.QrCodeScanner` (2) | none |
| `share` | Hand something out (defined on web, no use found) | none | none | `share` (~0) |
| `download` | Save to this device | `square.and.arrow.down` (2) | none | none |
| `reply` | Reply | `arrowshape.turn.up.left` (2) | `AutoMirrored.Filled.Reply` (1) | `reply` (~2) |
| `forward` | Forward; forwarded label | `arrowshape.turn.up.right` (4) | `AutoMirrored.Filled.Forward` (3) | `forward` (~4) |
| `pin` | Pin; pin a call tile | `pin` (7) | `Filled.PushPin` (6), `Outlined.PushPin` (1) | `pin` (~9) |
| `pin-fill` | Pinned mark; pinned call tile | `pin.fill` (4) | none | none |
| `pin-slash` | Unpin | `pin.slash` (5) | none | none |
| `bookmark` | Save message; Saved | `bookmark` (4) | `Filled.BookmarkBorder` (2), `Filled.Bookmark` (3) | `bookmark` (~5) |
| `bookmark-fill` | Saved mark on a message | `bookmark.fill` (1) | none | none |
| `bookmark-slash` | Remove from Saved | `bookmark.slash` (2) | none | none |
| `translate` | Translate; translation settings | `character.bubble` (4) | `Filled.Translate` (1) | none |
| `text-bubble` | Show original | `text.bubble` (1) | none | none |
| `checkmark-message` | Mark all as read | `checkmark.message` (2) | none | none |
| `list` | All pinned messages; list | `list.bullet` (1) | `AutoMirrored.Filled.List` (1) | `lines@list` (~2) |
| `archive` | Archive | `archivebox` (2) | `Filled.Archive` (2) | `archive` (~2) |
| `archive-fill` | Archived mark | `archivebox.fill` (1) | none | none |
| `unarchive` | Unarchive | `tray.and.arrow.up` (2) | `Filled.Unarchive` (2) | none |
| `bell` | Unmute; bell feed icon | `bell` (3) | `Filled.Notifications` (1) | `bell` (~3) |
| `bell-slash` | Mute | `bell.slash` (2) | `Filled.NotificationsOff` (3) | `bellOff` (~3) |
| `bell-slash-fill` | Muted mark | `bell.slash.fill` (1) | none | none |
| `folder` | Chat folders; add to folder | `folder` (2) | `Filled.Folder` (2) | `folder` (~4) |
| `folder-plus` | New folder | `folder.badge.plus` (4) | `Filled.CreateNewFolder` (1) | none |
| `folder-minus` | Remove from folder | `folder.badge.minus` (1) | `Filled.FolderOff` (1) | none |
| `clock` | Waiting to be sent | `clock` (1) | `Filled.Schedule` (1) | none |
| `eye` | Show password; reveal | `eye` (2) | `Filled.Visibility@password` (1) | `eye` (~4) |
| `eye-slash` | Hide password; hide from feed | `eye.slash` (3) | `Filled.VisibilityOff` (1) | `eyeOff` (~3) |
| `logout` | Log out; leave the call | `rectangle.portrait.and.arrow.right` (2) | `AutoMirrored.Filled.Logout` (2) | `logout` (~4) |

## Composer and media

| Lakko icon | Meaning | iOS (SF Symbol) | Android (Material) | Web |
|---|---|---|---|---|
| `paperclip` | Attach | `paperclip` (3) | `Filled.AttachFile` (1) | `clip` (~5) |
| `send` | Requests sent | `paperplane` (1) | none | none |
| `send-fill` | Send | `paperplane.fill` (3) | `AutoMirrored.Filled.Send` (4) | `send` (~4) |
| `face-smiling` | Emoji picker button; reactions setting row | `face.smiling` (4) | `Filled.Mood` (1) | `smile` (~2) |
| `mic` | Microphone (outline) | none | none | `mic` (~2) |
| `mic-fill` | Microphone on; record a voice message | `mic.fill` (4) | `Filled.Mic` (2), `drawable/ic_call_pip_mic` (1) | none |
| `mic-slash` | No microphone access | `mic.slash` (1) | none | none |
| `mic-slash-fill` | Microphone off; muted participant | `mic.slash.fill` (5) | `Filled.MicOff` (3), `drawable/ic_call_pip_mic_off` (1) | `micOff` (~3) |
| `stop-fill` | Stop recording | `stop.fill` (2) | `Filled.Stop` (1) | none |
| `play-fill` | Play | `play.fill` (4) | `Filled.PlayArrow` (4) | none |
| `pause-fill` | Pause | `pause.fill` (2) | `Filled.Pause` (2) | none |
| `play-circle` | Autoplay video messages | `play.circle` (1) | `Filled.PlayCircle` (1) | none |
| `speaker` | Call speaker (defined on web, no use found) | none | none | `speaker` (~0) |
| `speaker-slash-fill` | Video playing without sound | `speaker.slash.fill` (1) | `AutoMirrored.Filled.VolumeOff` (1) | none |
| `photo` | Photo attachment; photo placeholder | `photo` (4) | none | none |
| `photo-stack` | Choose a photo | `photo.on.rectangle` (5) | `Filled.PhotoLibrary` (4) | none |
| `camera` | Take a photo; camera feed icon | `camera` (4) | `Filled.PhotoCamera` (5) | `camera` (~2) |
| `camera-flip` | Switch camera | `arrow.triangle.2.circlepath.camera` (3) | `Filled.Cameraswitch` (2), `Filled.FlipCameraAndroid` (1) | `cameraFlip` (~2) |
| `video` | Video call; video in history | `video` (4) | none | `video` (~17) |
| `video-fill` | Camera on; video call kind; record a video message | `video.fill` (6) | `Filled.Videocam` (8) | none |
| `video-slash` | No camera access; missed video call | `video.slash` (2) | none | none |
| `video-slash-fill` | Camera off | `video.slash.fill` (1) | `Filled.VideocamOff` (1) | `videoOff` (~1) |
| `display` | Screen sharing | `display` (3) | `Filled.ScreenShare` (3) | `monitor` (~9) |
| `doc` | File attachment | `doc` (4) | none | none |
| `doc-text` | Licences | `doc.text` (1) | `Filled.Gavel` (1) | none |
| `layout-grid` | Grid call layout | none | `Filled.GridView` (1) | `layoutGrid` (~1) |
| `layout-focus` | Stage call layout | none | `Filled.ViewDay` (1) | `layoutFocus` (~1) |

## Calls

| Lakko icon | Meaning | iOS (SF Symbol) | Android (Material) | Web |
|---|---|---|---|---|
| `phone-down` | Missed or ended call in history | `phone.down` (2) | `AutoMirrored.Filled.CallMissed` (1) | `missed` (~1) |
| `phone-down-fill` | End call; decline | `phone.down.fill` (4) | `Filled.CallEnd` (4) | `phoneEnd` (~4) |
| `phone-outgoing` | Outgoing call | `phone.arrow.up.right` (2) | `AutoMirrored.Filled.CallMade` (1) | `outgoing` (~1) |
| `phone-incoming` | Incoming call | `phone.arrow.down.left` (2) | `AutoMirrored.Filled.CallReceived` (1) | `incoming` (~1) |
| `phone-connection-fill` | Call in progress banner | `phone.connection.fill` (1) | none | none |

## People

| Lakko icon | Meaning | iOS (SF Symbol) | Android (Material) | Web |
|---|---|---|---|---|
| `person` | For me preset | `person` (1) | `Filled.Person@feed` (1) | `user` (~2) |
| `person-fill` | Avatar placeholder | `person.fill` (25) | none | none |
| `person-2` | Participants; group type | `person.2` (5) | none | none |
| `person-2-fill` | Participants control in a call | `person.2.fill` (1) | `Filled.Group` (2) | none |
| `person-3` | New group | `person.3` (1) | `Filled.Groups@menu` (1) | `groups` (~7) |
| `person-3-fill` | Group avatar placeholder | `person.3.fill` (1) | `Filled.Groups@avatar` (1) | none |
| `person-plus` | Add contact; add member; invite to call | `person.badge.plus` (5) | `Filled.PersonAdd` (5) | `userPlus` (~3) |
| `person-circle-plus` | Add contact menu | `person.crop.circle.badge.plus` (1) | none | none |
| `person-minus` | Remove contact | `person.badge.minus` (1) | `Filled.PersonRemove@contact` (1) | none |
| `person-circle-xmark` | Delete account | `person.crop.circle.badge.xmark` (1) | `Filled.PersonRemove@account` (1) | none |
| `person-2-gear` | Who can find me | `person.2.badge.gearshape` (1) | `Filled.Visibility@privacy` (1) | none |
| `person-question` | Same answer for a wrong name or password (explainer) | `person.fill.questionmark` (1) | none | none |

## Security and recovery

| Lakko icon | Meaning | iOS (SF Symbol) | Android (Material) | Web |
|---|---|---|---|---|
| `passkey` | Passkeys | `person.badge.key` (6) | `Filled.Key@passkeys` (4), `Filled.Fingerprint` (1) | none |
| `key` | Recovery phrase | `key` (8) | `Filled.Key@phrase` (4), `AutoMirrored.Filled.Notes` (2) | `key` (~7) |
| `lock` | Password | `lock` (1) | `Filled.Password@password` (1) | `lock` (~13) |
| `lock-fill` | End-to-end encrypted call; recording lock | `lock.fill` (5) | `Filled.Lock` (4) | none |
| `lock-slash-fill` | Call not end-to-end encrypted | `lock.slash.fill` (1) | none | none |
| `lock-rotation` | Authenticator code | `lock.rotation` (2) | `Filled.Password@totp` (2) | none |
| `lock-shield` | Protection status | `lock.shield` (1) | `Filled.Shield@protection` (2) | none |
| `shield` | Security section (web) | none | none | `shield` (~4) |
| `shield-check` | Verify security code | `checkmark.shield` (2) | `Filled.VerifiedUser` (1) | none |
| `shield-exclamation` | Keys changed; new device warning | `exclamationmark.shield` (3) | none | `shieldAlert` (~1) |
| `network-shield` | Anti-censorship | `network.badge.shield.half.filled` (1) | `Filled.Shield@network` (1) | none |
| `lifepreserver` | Recovery | `lifepreserver` (2) | `Filled.Restore` (1) | none |
| `words` | Three words of the phrase | `text.word.spacing` (2) | `AutoMirrored.Filled.ShortText` (2) | `lines@recovery` (~2) |
| `envelope` | Recovery email | `envelope` (7) | `Filled.Email` (3), `Filled.Key@email-code` (1) | `mail` (~3) |

## Devices and places

| Lakko icon | Meaning | iOS (SF Symbol) | Android (Material) | Web |
|---|---|---|---|---|
| `devices` | Linked devices | `laptopcomputer.and.iphone` (4) | `Filled.Devices` (3) | `devices` (~6) |
| `smartphone` | A phone in the devices list | `iphone` (1) | `Filled.PhoneAndroid` (1) | `smartphone` (~0) |
| `tablet` | A tablet in the devices list | `ipad` (1) | `Filled.TabletAndroid` (1) | `tablet` (~1) |
| `laptop` | A computer in the devices list | `laptopcomputer` (1) | `Filled.Computer` (1) | none |
| `smartphone-waves` | Restore from another device | `iphone.gen3.radiowaves.left.and.right` (2) | `Filled.Devices@recover` (3) | none |
| `smartphone-arrow` | Your password never leaves the device (explainer) | `iphone.and.arrow.forward` (1) | none | none |
| `radiowaves` | Channels I share | `dot.radiowaves.left.and.right` (1) | `Filled.Share` (1) | none |
| `globe` | Language; a browser device; Discover; globe feed icon | `globe` (5) | `Filled.Language` (3), `Filled.Public` (1) | `globe` (~7) |
| `location` | Show my city | `mappin.and.ellipse` (1) | `Filled.Place` (1) | none |

## Settings and channels

| Lakko icon | Meaning | iOS (SF Symbol) | Android (Material) | Web |
|---|---|---|---|---|
| `appearance` | Appearance | `circle.lefthalf.filled` (1) | `Filled.Palette` (1) | `palette` (~1) |
| `sun` | Light theme option | none | none | `sun` (~1) |
| `moon` | Dark theme option | none | none | `moon` (~1) |
| `text-size` | Text size | `textformat.size` (1) | `Filled.FormatSize` (1) | none |
| `thumbsup` | Quick reaction | `hand.thumbsup` (1) | none | none |
| `database` | Data section | none | none | `db` (~2) |
| `megaphone` | New channel; a channel in search | `megaphone` (2) | `Filled.Campaign` (2) | `channels` (~2) |
| `megaphone-fill` | Channel avatar placeholder | `megaphone.fill` (6) | none | none |
| `bubbles` | No conversations yet | `bubble.left.and.bubble.right` (1) | none | none |
| `bubbles-fill` | Chat in a call | `bubble.left.and.bubble.right.fill` (1) | `AutoMirrored.Filled.Chat@call` (1) | none |

## Feed rule icons

| Lakko icon | Meaning | iOS (SF Symbol) | Android (Material) | Web |
|---|---|---|---|---|
| `star` | Feed icon | `star` (1) | `Filled.Star` (1) | `star` (~1) |
| `heart` | Feed icon | `heart` (1) | none | `heart` (~6) |
| `heart-fill` | Reactions button; unseen reactions mark | `heart.fill` (2) | `Filled.Favorite` (1) | none |
| `bolt` | Popular preset; feed icon | `bolt` (2) | `Filled.Bolt` (1) | `lightning` (~1) |
| `book` | Feed icon | `book` (1) | `Filled.Book` (1) | `book` (~1) |
| `code` | Feed icon | `chevron.left.forwardslash.chevron.right` (1) | `Filled.Code` (1) | `code` (~1) |
| `music-note` | Feed icon | `music.note` (1) | `Filled.MusicNote` (1) | `music` (~1) |
| `flag` | Feed icon | `flag` (2) | `Filled.Flag` (1) | `flag` (~1) |

## Icons drawn by one client only

Icons that only one client draws today. For many of these the other clients show the meaning another way: with the icon of a neighbouring row (Android draws both pin and pinned with `PushPin`, and its selected tabs with the `Filled` form listed in the outline row), with text, with a system control, or not at all.

- **iOS only**: `chat-fill`, `phone-fill`, `gear-fill`, `circle`, `radio-on`, `bullet`, `minus-circle`, `minus-circle-fill`, `exclamation-circle-fill`, `warning`, `warning-fill`, `circle-slash`, `arrow-down`, `arrow-up-right`, `arrow-up-down`, `arrow-clockwise`, `arrow-up-circle-fill`, `arrow-down-square`, `zoom-in`, `zoom-out`, `link-plus`, `download`, `pin-fill`, `pin-slash`, `bookmark-fill`, `bookmark-slash`, `text-bubble`, `checkmark-message`, `archive-fill`, `bell-slash-fill`, `send`, `mic-slash`, `photo`, `video-slash`, `doc`, `phone-connection-fill`, `person-fill`, `person-2`, `person-circle-plus`, `person-question`, `lock-slash-fill`, `smartphone-arrow`, `thumbsup`, `megaphone-fill`, `bubbles`.
- **Android only** (or Android and web, not iOS): `drag-handle (and web)`, `layout-grid (and web)`, `layout-focus (and web)`.
- **Web only**: `ellipsis`, `share`, `mic`, `speaker`, `shield`, `sun`, `moon`, `database`.

## Where the clients disagree today

The same meaning drawn differently on Android, chosen without the iPhone in mind. The new set follows the iPhone column in each case:

- Back is an arrow on Android (`ArrowBack`, 40 uses) and a chevron on iOS and web.
- The Contacts tab is `People` on Android and a person in a circle on iOS.
- Password is `Password` (asterisks) on Android and a lock on iOS; the authenticator code is `Password` on Android and `lock.rotation` on iOS.
- Passkeys are `Key` / `Fingerprint` on Android and `person.badge.key` on iOS; the recovery phrase is `Key` / `Notes` on Android and `key` on iOS. The same `Key` icon means both on Android.
- Recovery is `Restore` on Android and `lifepreserver` on iOS; licences are `Gavel` and `doc.text`; Channels I share is `Share` and `dot.radiowaves.left.and.right`; Who can find me is `Visibility` and `person.2.badge.gearshape`; Appearance is `Palette` and `circle.lefthalf.filled`.
- Missed calls are `CallMissed` (an arrow) on Android and `phone.down` on iOS.
- More is `MoreVert` (vertical dots) on Android, `ellipsis.circle` on iOS and horizontal dots on web.
- Restoring with an email code uses `Key` on Android and `envelope` on iOS.

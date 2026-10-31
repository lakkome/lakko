<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Desktop: Lakko on macOS, Windows and Linux

Date: 2026-10-16. Status: draft, written before any desktop code. Scope: the
desktop client of [ADR-0085](../adr/0085-desktop-client.md) (Tauri 2, the
Rust core in the host process, the screens of `apps/web`).

**How to read this.** The desktop client shows the same screens as the web
client at desktop width: the same destinations
([`navigation.md`](navigation.md)), the same chat, call, contact and settings
flows, the same copy from the shared catalog. This spec states only what a
desktop app adds or changes: a window that is not a browser tab, a process
that keeps running when the window closes, system notifications, keyboard,
files, links opened from outside, updates, and the keys kept by the operating
system. Where the three operating systems have to differ, the difference is
written inline and marked **Per OS**.

## 0. Why the desktop app is not "the website in a window"

A person who installs a messenger expects three things a browser tab does not
give: messages and calls arrive while the window is closed, the app knows the
computer (notifications, Dock or taskbar badge, files dragged in, links from
other apps), and closing the window does not sign anything out or lose
anything. Everything below follows from those three.

## 1. Install and first launch

- The app installs like any other app on the OS: **Per OS** a `.dmg` with the
  app to drag into Applications on macOS; an installer (`.msi` or `.exe`) on
  Windows; an AppImage and a `.deb` on Linux. Which formats ship is the ADR's
  decision, not this spec's.
- First launch shows the same sign-in / create-account screens as the web
  client, in a window (section 2), not in a browser.
- A desktop is usually a **second device**. The sign-in screen offers, as on
  the web, "Sign in with another device" first: the desktop shows the six-digit
  code flow of [`identity-recovery.md`](identity-recovery.md) section 3 and
  [`second-device.md`](second-device.md); the phone confirms. A desktop rarely
  has a usable camera, so the code is the default route and the QR is offered
  beside it, never instead of it.
- The device is named "Lakko on Mac", "Lakko on Windows" or "Lakko on Linux"
  in Settings > Security > Devices, the person can rename it there.

### 1.1 This computer cannot keep the keys

The keys of this device are kept by the operating system (Keychain on macOS,
the TPM-backed key store on Windows, the Secret Service keyring on Linux). If
that store is missing or refuses (Linux with no keyring service running; a
locked keyring the person declines to unlock), Lakko does not sign in and does
not fall back to keeping keys in a plain file:

> **Lakko can't store your keys safely on this computer.**
> Your system keyring isn't available. Unlock it or install one, then try
> again.
> [Try again]

This is a first-class screen, not an error toast. What counts as "safe" on a
Windows computer without a TPM is an open decision of the ADR (section 11).

## 2. The window

- One main window. It opens at the size and position it had when it last
  closed, on the same display if that display is still connected, else
  centred on the primary display.
- Minimum size: the width at which the web layout still shows the nav rail,
  the conversation list and the open conversation side by side. Below that the
  window does not shrink; the narrow (drawer) layout of the web is never shown
  in the desktop app.
- **The window title is always "Lakko"**, whatever is open. A title is read
  by the task switcher, the window list, screen recorders and screen-sharing
  pickers; it never carries a contact name, a group name or a message. This is
  the desktop form of the web rule that the address bar shows the origin only.
- There is no address bar, no browser back button, no browser menu, no
  "Inspect element", no reload. Back inside a screen is the app's own back
  control and the keyboard shortcut of section 5.
- The unread count appears on the app icon: **Per OS** the Dock badge on
  macOS; the taskbar overlay badge on Windows; on Linux where the desktop
  environment supports a launcher count, nowhere otherwise. The count is a
  number only, never a name.

### 2.1 A call in the window

A call takes over the main window exactly as on the web, and the call bar of
[`calls.md`](calls.md) section 4.5 lets the person put it away and keep
chatting. A separate call window is not in this spec (section 12).

## 3. Closing is not quitting

- Closing the window (the red button, the X, `Ctrl+W` / `Cmd+W`) hides it.
  Lakko keeps running, keeps receiving messages and keeps ringing for calls.
- **Per OS** where it keeps living:
  - macOS: in the Dock, as every Mac app does. Clicking the Dock icon opens
    the window again. No menu bar icon.
  - Windows: an icon in the notification area (system tray). Click opens the
    window; right click offers "Open Lakko" and "Quit Lakko".
  - Linux: a tray icon where the desktop environment shows one. **Where it
    does not** (stock GNOME has none), closing the window minimises it to the
    taskbar instead of hiding it, so the person always has a way back.
- The first time the window is closed while Lakko keeps running, one banner
  in the system notification says so, once per device:

  > **Lakko is still running.** You'll get messages and calls. Quit from the
  > tray icon to stop. (Windows / Linux)

  On macOS nothing is shown: keeping running after the window closes is what
  Mac apps do.
- **Quit** (`Cmd+Q` on macOS, "Quit Lakko" in the tray menu, the app menu's
  Quit): stops the app. During a call it first asks:

  > **End the call and quit?**
  > [Cancel] [End call and quit]

- After a quit nothing arrives until Lakko is opened again: there is no server
  push to a desktop that is not running (section 4.3).

### 3.1 Settings that belong to this

In Settings > Notifications, desktop only, two switches:

- **Keep running when the window is closed** - on by default. Off: closing the
  window quits.
- **Open Lakko when I sign in to this computer** - off by default. On: Lakko
  starts hidden (no window) at login and keeps running.

Both are stored on this device only.

## 4. Notifications

- Lakko uses the operating system's notifications (allowed by the design
  system rule on system surfaces). Their content follows the existing
  Notifications settings: with previews on, the sender name and the message
  text; with previews off, "New message". Content is decrypted on this
  computer; nothing is sent to a notification server.
- Clicking a notification opens the window on that conversation.
- No notification for the conversation that is open and visible in a focused
  window. A muted conversation never notifies.
- **Per OS** permission: macOS asks once on first need (the system prompt);
  Windows and Linux need no prompt. If the system has notifications for Lakko
  turned off, Settings > Notifications says so in one line with "Open system
  settings" where the OS has a link for it.

### 4.1 An incoming call

- With the window hidden, an incoming call raises a system notification with
  the caller's name and, **Per OS** where the notification supports actions
  (macOS, Windows), "Accept" and "Decline" buttons. It rings with the Lakko
  ringtone until answered, declined, or the ring times out, as on the other
  clients ([`calls.md`](calls.md) section 2).
- Accept from the notification opens the window straight into the call.
- With the window open, the in-app incoming-call banner shows as on the web,
  and the window is brought forward (it does not steal focus from a typing
  person; **Per OS** it flashes in the taskbar / bounces the Dock icon once).

### 4.2 Microphone, camera, screen

**Per OS** the first call asks the system for the microphone (and the camera
for video): macOS shows its own prompt; Windows and Linux follow their privacy
settings. A voice call has no screen before it ([`calls.md`](calls.md) section
3.1), so the refusal is a dialog raised when the call is placed, accepted or
joined:

> **Lakko can't use your microphone.** Allow it in your system's privacy
> settings, then try again.
> [Cancel] [Open settings] [Try again]

The camera has the same dialog with its own title. "Open settings" is shown
only where the OS has a link to that page (macOS, Windows). Cancel on the
microphone does not place or accept a one-to-one call; a group call goes on,
listening and watching. Cancel on the camera goes on as a voice call.

### 4.3 When messages arrive

| Lakko is | Messages and calls |
|---|---|
| open, window visible | arrive live |
| running, window hidden | arrive live, notify |
| quit, or the computer asleep | arrive the next time Lakko runs and is online |

After a computer wakes from sleep or comes back online, the existing
"Synchronizing..." label shows while Lakko catches up, then goes away. A call
that rang while the computer slept shows as missed, as on the phone.

## 5. Keyboard

Shortcuts use `Cmd` on macOS and `Ctrl` on Windows and Linux (written `Mod`):

| Shortcut | Does |
|---|---|
| `Mod+K` | Search conversations and people |
| `Mod+N` | New chat (the create menu) |
| `Mod+Shift+1` .. `Mod+Shift+5` | Chats, Feed, Contacts, Calls, Settings |
| `Mod+1` .. `Mod+9` | Chat folder tabs (as on the web, unchanged) |
| `Alt+Up` / `Alt+Down` | Previous / next conversation in the list |
| `Mod+,` | Settings |
| `Esc` | Close a menu, dialog or sheet; leave a search |
| `Enter` / `Shift+Enter` | Send / new line in the composer |
| `Up` in an empty composer | Edit my last message |
| `Mod+W` | Close the window (section 3) |
| `Mod+Q` (macOS), "Quit" in the tray (Windows / Linux) | Quit |

Every action reachable by a shortcut is also reachable by a visible control.
Shortcuts are listed in Settings > About > Keyboard shortcuts.

**Per OS** menus: on macOS the standard menu bar (Lakko: About, Settings,
Quit; Edit: undo, cut, copy, paste, select all; Window: minimise, zoom,
bring all to front). Windows and Linux have no menu bar; everything is in the
app.

## 6. Files

- A file dragged from the desktop or a file manager onto an open conversation
  attaches it, as the attach button would. While dragging over a conversation
  the pane shows "Drop files to attach"; over any other screen the drop is
  refused (no drop target is shown).
- Pasting an image from the clipboard into the composer attaches it. A copy
  from a document, which holds text beside a picture of it, pastes as text.
- "Save" on an attachment opens the system save dialog (allowed: it is the
  file picker); the default folder is the system Downloads folder.
- Opening an attachment with another app (the system's "open with") is not in
  this spec: a decrypted file handed to another app leaves Lakko's control.

## 7. Links from outside

- The invite, call-link and group-invite pages on `lakko.me` gain an **Open
  in the app** button beside what they show today. It opens the desktop app
  (through its `lakko://` scheme). On macOS the app opens on the same screen
  the web would show. On Windows and Linux the page copies the link, says
  "Link copied. Paste it in the app.", and the app opens the Open a link dialog
  for it (ADR-0085 section 5). Phones and tablets are not offered the button.
- On macOS in Chrome and Firefox the page also tries the app by itself, once
  per link, when it opens, as a meeting link does: the browser asks whether to
  open Lakko. Safari is left out, as is any browser not named here: with no app
  installed it answers with its own "address is invalid" message, so there the
  button is the only way. Windows and Linux stay on the button too, since
  copying the link needs a press.
- A page cannot tell whether the app opened. With no app installed the web page
  keeps working as today; pressing the button in Safari shows that browser's
  message and nothing else.
- Inside the app, the create menu has **Open a link**: it takes a pasted
  Lakko link (`https://lakko.me/i/`, `/c/`, `/g/`) and shows its screen. This
  is the route that always works. Anything else stays in the dialog with "This
  is not a Lakko link. Check it and try again."
- A link that is not a Lakko link is never opened inside Lakko; links in
  messages open in the system's default browser.

## 8. Updates

- When a newer version is available and downloaded, a banner (the
  update-notice banner of [`update-available-notice.md`](update-available-notice.md))
  says:

  > **A new version of Lakko is ready.**
  > [Restart to update]

- The app never restarts on its own, and never during a call: "Restart to
  update" is hidden while a call is live.
- An update whose signature does not check is not installed, and nothing is
  shown to the person beyond the absence of the banner (the update notice is
  an availability feature, not a security indicator).
- **Per OS**: where an app store delivers the update (none planned for v1),
  the store does it and Lakko shows the store variant of the banner.

## 9. Anti-censorship

Settings > Security > Anti-censorship works as on iOS and Android: when on, all
of Lakko's traffic goes through the obfuscated connection, call media excepted.
The browser cannot do this; the desktop app can. Same states and copy as the
native clients, with one difference: on the desktop the switch on means nothing
goes out directly. While the connection is being set up, or when it could not
be, requests fail until it works or the switch is turned off. A build made
without the setting says so, and stays direct.

## 10. Signing out and forgetting this device

Same as the other clients: Settings > Account. "Forget this device" also
removes the keys from the system key store and the app's local data folder;
the app then shows the first-launch screen. Uninstalling the app does not
remove the local data on every OS; Forget this device is the way to remove it.

## 11. Data the UI needs (input to the ADR)

From the host process, over the IPC boundary, beyond what the web screens
already read:

- **Key store state**: available / missing / locked / refused, to drive 1.1.
- **Window and lifecycle**: hidden / visible / focused, a "close requested"
  event the UI can answer during a call, quit requested.
- **Background state**: whether the receive connection is up, to drive
  "Synchronizing...".
- **Unread total** for the badge, computed where the messages are.
- **Notification events**: clicked (with the conversation), call accepted /
  declined from a notification.
- **Deep link intake**: a Lakko link handed over by the OS at launch or while
  running.
- **Update state**: none / downloading / ready / failed-silently.
- **Settings stored by the host**: keep running, start at login, tray
  available (Linux).
- **Permission state** for microphone, camera and notifications, and whether
  "Open settings" can be offered.

Messages are decrypted in the window's page, never in the host (ADR-0085
section 4): a notification has text only when the page could run while the
window was hidden; otherwise it reads "New message".

The decisions this spec depended on are recorded in ADR-0085 section 9
(2026-10-16): a computer without hardware key storage keeps the keys in the
operating system's software store and shows nothing different (1.1 applies
only when there is no store at all); links use `lakko://` plus "Open a link"
(section 7); updates are verified with TUF (section 8).

## 12. Not in this spec

- A separate call window, picture-in-picture over other apps.
- Several accounts in one app; several main windows.
- Screen sharing (not built on any client).
- An app lock (password or system authentication to open the window).
- Mac App Store / Microsoft Store / Flathub distribution.
- A menu bar (macOS) or tray quick-reply.

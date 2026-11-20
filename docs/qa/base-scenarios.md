<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Base scenarios

What a person tries in their first hour. This list is walked whole, on running
clients, before anything is handed to the maintainer: a pass over the fix at hand
is not a pass over the product. Each scenario runs on web, iOS and Android, as sender
and as receiver, with the other side on a different client. A step counts only when
its visible result was seen on both sides. Scenarios marked (design) wait for
ADR-0087. Add a scenario whenever a defect shows one was missing.

## Account
1. Sign up, sign out, sign in, on a second device of the same account.
2. Set a name and a photo; both reach a contact and a group.
3. Change the username; a contact still reaches the person.
4. Lose the device: recover with another device, a passkey, the phrase.
4a. Save a recovery phrase on a fresh account, on every client: the words appear, the
    three-word check passes, the account stays signed in after a relaunch, and the
    device that did it is not asked "It was me". Then a chat works in both directions,
    and a device that lost everything restores from those 12 words.
4b. Reset account keys, on every client, on a device with no keys: the new phrase
    appears only after the publish, `Try again` finishes an interrupted publish, and
    every other device of the account ends on the signed-out screen.
4c. An account with an email vault saves a phrase, which changes its keys. Then, on
    every client in both roles: someone new adds them, one contact row on each side, a
    chat both ways; they forget the device and restore from the 12 words; the chat comes
    back by itself and works both ways.

## Contacts
5. Find a person by @username and by name; send a request; accept; decline; ask again.
6. Invite link: open it signed in, signed out, offline inviter, dead link. Dead means
   expired, revoked and used up; each reads as dead before the person signs up.
6b. A link in a message, on every client in both roles, in text around it and alone:
   a Lakko link opens its screen in place, any other link opens in the browser, a long
   press is the message menu. Nobody copies a link anywhere.
6c. A link from outside on a computer with the desktop app: clicked in a browser, the page
   offers the app and the app opens on the same screen, signed in and on the same account.
6d. With the desktop app's window closed, and in a web tab with notifications refused: a
   contact request through a link is answered, a message arrives, and it is all there when
   the window opens. No other device of the account is running.
7. Remove a contact, then find and add them again, from each side.
8. Who can find me: each setting, and what a searcher sees under it.
9. One person is one row in every list and every search.

## One-to-one
10. Text, reply, forward, edit, delete, react, pin, read state, typing. Fast typing keeps
    every letter; a hardware keyboard works in the composer and in every dialog.
11. Photo, file, voice message, video note, GIF, both ways, on every pair.
12. Call and video call: ring, answer, decline, miss, hang up, app closed, screen share.
    Each right after a message was sent in that chat. A first video call with the camera
    prompt on screen. Ending a call leaves the app responsive at once. Sound and picture
    both ways on every pair of clients (web and iOS, web and Android, iOS and Android),
    on a real iPhone and a real Android phone: a simulator accepted the call audio
    setup an iPhone refuses, so iPhone calls had no sound and nobody saw it there
    (2026-11-10). With the app sent to the home screen the call goes on, with the
    system's floating window.
13. A message sent while the other side is offline arrives; push wakes a closed app.

## Groups
14. Create a group with nobody else, then add people. Alone, it shows no call buttons;
    they appear with the second person and go when that person is removed. A group just
    created or joined, with no message yet, sits at the top of Chats on both sides, and
    stays there after a restart.
15. Create a group with contacts and with people found by search.
16. Name, photo, roles, admins, ownership transfer.
17. Add, remove, leave, and re-add the same person; the removed person loses the group
    and its history on every device; a re-added person is back and sees new messages.
    Each of these, on every client: removed with the chat open and a draft in the
    composer (the notice carries the draft and `Copy`); removed with the app closed;
    removed, written to and re-added while the app is closed (the group comes back with
    only what was written after the return, and the person can answer); a person with
    two devices, one of them closed; leaving while no owner or admin is online
    (`Waiting to leave.`, then the row goes); a fast send leaves an empty composer.
18. Everything in 10 and 11 in a group, including a member who joined late.
19. Group call: join, leave, camera, pin, a late joiner. A picture from each client's
    camera reaches a tile on the other two; the tile is not dark. A joiner changes the
    call key: after each admission every participant still hears and sees every other,
    with each client in turn as the one who started the call (an iPhone that started a
    call kept sealing under the key of before a web joiner's admission and the web got
    neither sound nor picture, 2026-11-10). Call links (`/c/`) the same.
20. Invite link and approval; a request to join, approved and rejected.
21. Public and private group: found or not found, join by request or by invitation. (design)

## Channels
22. Create a channel; the Chats list stays whole and the channel sits in it.
23. Open a channel: the group's screen shape, no call buttons, a subscriber has no composer.
24. Post text, photo, file, voice, video note, GIF; a subscriber sees each. (design)
24b. A subscriber reacts and takes it off on two devices; comments under a post, opens and
    collapses the thread; a publisher deletes a comment and bans. (design)
25. Find a channel by @handle and by name; subscribe; unsubscribe. Opening a channel
    found by search shows Subscribe and changes nothing: the owner's count moves only
    when Subscribe is pressed. A channel already followed opens straight to its posts.
    (design)
26. Invite a person to a channel, public and private. (design)
27. A subscriber who joins later; one who was offline across a rotation.
28. Publishers of a channel post and moderate, only the owner renames and adds them;
    names in the subscriber list, no machinery. (design)

## Lists and navigation
29. Chats holds one-to-ones, groups and channels; unread, mute, archive, pin, folders.
30. Every screen has a way back, on every width.
31. Search from Chats and Contacts finds what the person expects.
32. Feed, Saved, Calls: each shows real content or a clear empty state.
    Discover in Feed shows public channels; subscribing from a card works.
    A Feed card reads left aligned at any width: nothing centred, nothing cut by
    the card's edge, a long text ending in an ellipsis after 8 lines, a photo in
    place. Tapping the text opens the chat; tapping the photo opens the photo.
33. Second device: messages, contacts, folders, read state, removal of a device.

## Quality
34. No protocol word and no meaningless line on any screen.
35. Dark and light; a phone width; an Android, an iPhone, and Chrome, Safari, Firefox.

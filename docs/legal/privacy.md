<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

<!--
DRAFT, 2026-11-19. Not legal advice; have it read by someone who knows Dutch and EU
data-protection law before publishing. Every statement below was checked against the
code on 2026-11-19 (data inventory: the "Facts" notes at the end of this file). Fill
every TODO. Items marked [FIX BEFORE PUBLISHING] describe how Lakko must behave; the
code does not do it yet (docs/beta-launch-checklist.md).
-->

# Lakko Privacy Policy

Last updated: TODO (date of publication)

This policy is in English only during the beta.

## Who we are

Lakko is run by **VVG**, a sole proprietorship registered in the Netherlands
(KvK TODO), TODO postal address. VVG is the controller of the personal data described
here. Contact: TODO privacy@lakko.me.

Lakko is in **public beta**. Its security has not yet been checked by an independent
audit. Do not rely on it where your safety depends on it.

## The short version

- Your messages, calls, files, contact list and group memberships are end-to-end
  encrypted. We cannot read them.
- You sign up with a username and a password. We never ask for your phone number or
  your address book.
- Our servers are built to keep as little as possible: no message content, no list of
  your contacts, no IP addresses in our access logs, no analytics and no crash
  reporting.
- What we do keep, and for how long, is listed below.

## What we cannot see

Messages, calls, voice and video messages, files, photos, profile names and photos,
reactions, read receipts, typing indicators and the members of private groups are
encrypted on your device and can only be read by the people in the conversation. Our
servers pass and store them only as ciphertext.

Our servers are also designed not to hold your social graph: they do not keep a list of
who your contacts are, which groups you are in, whom you call, or which channels you
publish to.

## What we keep

**Your account.** A random account id, when the account was created, your public
identity keys, and a record that lets you log in with your password without the password
ever reaching us (OPAQUE). If you set them up: your passkeys (public keys, and the name
you give each passkey), and your two-factor (TOTP) secret, which we store encrypted under
a key on our server so we can check your codes.

**Your username.** We store a one-way key derived from your username, not the username
itself, plus a keyed hash of it for logging in by name. If you choose to be findable by
everyone ("Everyone" in Settings), your public record and search terms for your name are
stored so others can find you; the operator of the server can read the names of people
who chose "Everyone".

**Recovery email (optional).** We never store your email address. We store a keyed hash
of it together with your username, so we can recognise it when you type it again. When
we send you a code, our server and our mail provider handle the address for that moment.
If you have not saved a recovery phrase, we also store your recovery phrase encrypted
under a key made from your username and email address; someone who knows both, or our
operator while sending you a letter, could open it. Saving a recovery phrase ends this.

**Messages in transit.** Encrypted messages wait on our server until your devices fetch
them, for at most **30 days**, then they are deleted.

**Files.** Encrypted files you send (photos, videos, voice messages and the like) are
stored as ciphertext under a random-looking name, with their size and the day they were
last opened. They are deleted **365 days after they were last opened** (sooner if the
sender chose a shorter life), or when the sender deletes them. TODO: if files are kept
in Hetzner Object Storage, say so here.

**Encrypted backup of your account.** If you use it, one encrypted backup of your account
state, which only your recovery phrase opens.

**Channels.** For each channel: its public key, settings, encrypted posts (kept **30
days**) and, for subscribers, a subscriber key and the last time it was active (removed
after **90 days** of inactivity). If a channel owner lists a channel in the directory, its
name, handle, description, topic and language are public.

**Push notifications.** Your device's push token, encrypted so that our push server can
only use it when a message for you arrives. We keep it until you turn notifications off,
remove the device, or the push provider reports it invalid.

**Supporters.** If you backed Lakko and got a supporter code, our server keeps a one-way
hash of the code, not the code or your email address. The place the code holds is used
through an anonymous token our server cannot connect to the code. The supporters list in
the app shows only the names backers chose to show.

**Security and abuse limits.** Counters that limit how often an account or a network
address can try things (in memory for addresses; in our database for accounts, keyed by
a hash), and anonymous rate-limit tokens. Network addresses are not written to our
database.

**Logs.** Our services do not log message content, account ids, usernames or IP
addresses. Our web server does not keep access logs, and our call relays keep no
connection log. The web server's log of critical errors can contain an IP address; it,
and every other server log, is deleted after **7 days**.

**Server backups.** We back up our databases and settings every night. A backup is kept
for at most **3 months**, encrypted, at our hosting provider.

## What stays on your device

Your message history, keys and settings are stored on your device, encrypted under a key
held by your device's secure hardware where it has one. Lakko contains no analytics,
advertising or crash-reporting code.

## Who else is involved

- **Hetzner Online GmbH** (Germany) hosts our servers, file storage and backups, in the
  EU.
- **Apple, Google, and your browser's push service** deliver push notifications. They
  learn your device's push token and when a notification is sent, not the message.
- **GIPHY** answers GIF searches. Our server sends your search words to GIPHY; GIPHY does
  not see your IP address or your account. Our server sees the searches of your account.
- **Our mail provider** (TODO name) sends recovery-email codes and sees the address.
- **Calls** go through our own relay servers. They see the network addresses of the
  participants while a call runs, not its content, and do not log them.
- **Cloudflare, Inc.** carries the traffic to lakko.me and api.lakko.me in front of our
  server (a proxy and protection against attacks). It sees your IP address and the
  encrypted connection to us, and, because it terminates the connection, the requests
  your app makes to those addresses; message content stays end-to-end encrypted inside
  them. Its processing may take place outside the EU, under the EU's standard
  contractual clauses. TODO: confirm the Cloudflare settings (proxied hostnames, logging,
  data localisation) and link its data processing terms.

We do not sell personal data and do not share it with advertisers.

## Why we may process it (legal basis)

To provide the service you signed up for (GDPR Art. 6(1)(b)); to keep the service secure
and prevent abuse (Art. 6(1)(f), our legitimate interest); and where the law requires it
(Art. 6(1)(c)).

## Deleting your account

You can delete your account in Settings. We then delete at once your account, login
records, passkeys, two-factor secret and recovery email data. Your username, public keys,
search entries and the encrypted backup of your account are deleted by our servers right
after, retried until it succeeds. Your device removes its push notification
subscriptions. Encrypted messages still waiting for other people expire within 30 days,
files you sent within their lifetime unless you deleted them, and our server backups
within 3 months.

## Your rights

You can ask us for access to your data, correction, deletion, restriction, portability,
and object to processing, by writing to TODO privacy@lakko.me. Because almost everything
we hold is encrypted or hashed, we may not be able to identify which data is yours unless
you are signed in. [FIX BEFORE PUBLISHING: an export of the account data our server holds
exists on the server (`GET /account/export`) but no client offers it yet.] You
can complain to the Dutch data protection authority, the Autoriteit Persoonsgegevens
(autoriteitpersoonsgegevens.nl).

## Age

You must be at least 16 to use Lakko.

## Changes

This page always shows the current version and its date. We will tell users about
material changes before they take effect.

<!--
Facts (2026-11-19, file:line in this repository; re-check before publishing):
- DS message TTL 30 days hard-coded: services/ds/src/journal.rs:61-63.
- Blob lifetime 365 days idle: services/storage/src/sweep.rs:4-10 (LAKKO_STORAGE_BLOB_LIFETIME_DAYS).
- Channel posts 30 days, subscribers 90 days: services/channels/src/config.rs:36,44.
- Recovery email stored only as keyed hash: services/auth/src/recovery_email.rs:9-29.
- TOTP secret sealed under a server key: services/auth/migrations/0012, src/totp.rs:103.
- Passkey nickname plaintext: services/auth/migrations/0006.
- nginx access_log off: infra/nginx/sites-available/lakko.conf:56,81,338; sfu.conf:53.
- No analytics/crash SDKs in apps/ (grep, 2026-11-19).
- Operator can read names of people in Everyone: docs/adr/0060-scoped-discovery.md:599-604.
- Backups 7 daily / 4 weekly / 3 monthly: infra/scripts/backup.sh.
- Account deletion: services/auth/src/webauthn.rs (delete_account queues account_purges,
  migration 0040), services/auth/src/purge.rs (worker to discovery and storage internal
  routes), migration 0039 (backup-head rows cascade); clients unsubscribe push and delete
  the backup after the auth delete (2026-11-19).
-->

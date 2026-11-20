<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0065: Attachment files have a lifetime, an owner who can delete them, and a gate

- **Status:** Accepted (maintainer, 2026-10-02). The saved-message register
  (section 4) and the delete-key derivation (section 6) are built in the core.
  **Sprint 2 (the server) built 2026-10-26, host-tested only, not deployed:**
  `services/storage` has the blob index (MariaDB via
  `LAKKO_STORAGE_DATABASE_URL`, migration `0001`, or an append-only index file
  under the blob root) with the one-time import, last-access accounting, the
  daily sweep, `POST /blob/pass/{upload,read}`, the pass gate on `PUT` / `GET`
  / `HEAD`, `POST /blob/keep` and the signed `DELETE`; the contract and every
  number chosen are in `services/storage/src/blob.rs` and `pass.rs`. The
  nginx blob locations carry a per-address `limit_req`. Pass budgets are
  counted in memory per instance (forgotten on restart, multiplied by the
  instance count). **Sprints 3 and 4 (the clients) built 2026-10-26/27 on web,
  iOS and Android, host-tested only, never run against the server or on a
  device:** passes and the delete key on the storage client (iOS uploads
  through the anti-censorship transport when armed), the gone and limit
  states, deletes on delete-for-everyone, of unsent uploads and of a replaced
  profile photo with a durable retry list, the keep tick (foreground, every
  10 min, at most 4 sets, each every 30 days, first due spread by FNV-1a of
  the set key `pins:<group hex>` / `saved` / `profile`), Save and the Saved
  screen. Two decisions made while building: the blob delete key is derived
  from the account's LINEAGE seed (the one the DeviceGroup id uses), so a key
  rotation (ADR-0074) does not orphan earlier uploads; and a saved register
  names a message by the MLS group of the SEGMENT that holds it and its DS id
  there (maintainer, 2026-10-27), so a message of older history can be saved
  and a saved message still opens after its conversation moves to a
  successor group. The snapshot carries the message time (`sent_at_ms`,
  synced-state version 6: the link record's `shared` flag had taken 5 on
  main when this branch was merged). The maintainer set the
  direction the same day:
  - a file nobody accesses for a year is deleted;
  - files of pinned messages and of messages the person **saved** are kept;
    Save is a new feature;
  - upload and download are both gated: an anonymous token buys an hour's
    pass with a budget, in each direction.
- **Date:** 2026-10-02
- **Context:** storage hardening. UX spec:
  [`docs/ux/saved-messages-and-file-lifetime.md`](../ux/saved-messages-and-file-lifetime.md).
  Shares the storage index of
  [ADR-0064](0064-conversation-backup-and-device-re-entry.md). Extends
  [ADR-0054](0054-anti-abuse-without-client-work.md) (two token contexts),
  [ADR-0058](0058-chat-folders.md) (one synced register),
  [ADR-0047](0047-media-messages-and-gif-proxy.md) and
  [ADR-0023](0023-message-edit-delete-reactions.md) (delete).

## Context

Every photo, video, voice message, video note, GIF and file is an encrypted
blob on `services/storage`, named by the SHA-256 of its ciphertext, with the
key carried inside the message (`lakko-attachments`). Read in the tree on
2026-10-02:

- `PUT`, `GET` and `HEAD /blob/{hex}` take no credential of any kind
  (`services/storage/src/main.rs`, `auth.rs`: "leaving the content-addressed
  blob routes auth-free"). The size cap is 50 MiB. The nginx locations carry
  no rate limit, although `main.rs` and the web client's comments say they do.
  Anyone can store 50 MiB objects without limit, and anyone who is handed an id
  and a key can download: the service is usable as anonymous file hosting.
- There is no `DELETE`, no index, no owner, no expiry. A blob lives forever.
  Deleting a message for everyone (ADR-0023) removes it from people's screens
  and leaves its ciphertext on the server. An upload whose message was never
  sent is an orphan nobody remembers.
- No client keeps attachment bytes across launches. Web and Android download
  every visible blob again each time a chat is opened; iOS keeps them in
  memory for the app session. A photo is two blobs (file and thumbnail).
- A missing blob (`404`) is shown as "Couldn't load photo. Try again." on all
  three clients; the existing text for a file that is gone is reachable only
  from two web paths.
- No "saved messages" feature exists. A personal pin is a flag on one device.

Constraints: invariant #2 (the server holds ciphertext only), invariant #5
(the server must not learn who uploaded or who reads a file), no production
data (flag day).

## Decision

### 1. An index row per blob

The storage index of ADR-0064 (MariaDB in deployment, a file-backed
implementation for one instance; objects remain the truth for content) gains
one row per blob: id, size, storage node, the day it was created, the day it
was last accessed, and a delete public key. Every blob request is answered
from the index. Nothing in a row names an account.

A one-time import lists the existing blob files into rows dated the import
day, with no delete key; it skips the `backup/` directory and the service's
temporary and probe files.

### 2. Lifetime: a year without access

A blob whose last access day is more than 365 days back is deleted, file and
row, by a daily sweep driven by the index. The number is the maintainer's.

An **access** is a download, a keep (below), or a repeated upload of the same
ciphertext. The last access day is written at most once per blob per day, so a
busy file costs one index write a day and the server keeps no finer record of
when a file is read than the day.

Because the clients fetch what is on screen each time a chat is opened, a file
people still look at stays; a file in a conversation nobody scrolls back to
goes after a year. The message is untouched: only the file goes, and the
clients show one "no longer available" state for it (UX 4.1), distinct from a
failed download.

### 3. Keep: pinned and saved messages

`POST /blob/keep` takes a list of blob ids (bounded) and marks each existing
one accessed today. It answers the same whether the ids exist or not.

Each device, about once a month on a slow background tick, sends keeps for the
files and thumbnails of:

- messages pinned for everyone or for me in the conversations it holds;
- the account's saved messages.

Keeps are sent per conversation and spread over time, not in one burst.
Anyone who knows a blob id and holds a read pass (section 5) can keep a file alive;
blob ids are known only to the members who received the message, and keeping
a file they can already read costs the service a year of storage and nothing
else.

### 4. Saved messages

Save is a personal bookmark that belongs to the account, not the device.

- The DeviceGroup synced state (ADR-0058, `lakko-sync::synced`) gains one
  register per saved message: conversation ref, message id, when it was
  saved, a display snapshot (author name, excerpt, attachment kind), and the
  blob ids of the message's files and thumbnails. Add and remove are
  last-writer-wins by the existing clock; a remove is kept as a tombstone. The
  account backup carries it with the rest of the synced state.
- Carrying the blob ids in the register is what lets any device of the
  account keep the files alive, including one that does not hold the message
  yet.
- A saved message deleted for everyone by its author leaves the saved list on
  each device as the delete is applied, like a pin.
- The client-side bound on the number of saved messages is chosen at build
  time and is not a decode bound.

Personal pins stay per device, as ADR-0052 decided; the device that holds the
pin sends the keeps for it.

### 5. A gate on upload and download: a token buys an hour's pass

People send hundreds of media messages a day, and every file sent is
downloaded by every recipient on every device. A token per file would mean
thousands of issuer evaluations and spent-set rows per person per day. So a
token buys a budget, in both directions.

Two anonymous token contexts (ADR-0054), which ration per account without
telling the storage service which account:

- **`BlobUpload`** (wire value 9). One token buys an **upload pass**
  (`POST /blob/pass/upload`): valid for an hour, good for a bounded number of
  files and a bounded number of bytes. `PUT /blob/{hex}` requires it and is
  charged against it before the body is read.
- **`BlobRead`** (wire value 10). One token buys a **read pass**
  (`POST /blob/pass/read`): valid for an hour, good for a bounded number of
  downloads and keeps. `GET`, `HEAD` and `keep` require it.

A pass is a bearer value that names no account: an HMAC under a service
secret over a random pass id, its kind and its expiry. The storage service
verifies it without a lookup and counts what was spent against the pass id for
the hour it lives. What a person can do in a day is the number of tokens the
issuer grants times the pass budget, so the daily ceiling can be generous
while the token machinery stays small.

Operations under one pass can be linked to each other by the server, as
operations over one connection already can. A device takes a new pass each
hour it is used, and each device of an account takes its own.

The token quotas, the pass lifetime and the two budgets are chosen at build
time, with room for heavy media use, and stated as chosen; they are to be
measured on real traffic. A refused upload or an exhausted budget is a
distinct state in the UI (UX 4.2), not a generic failure.

The nginx locations gain a per-address rate limit as a backstop against
floods of invalid passes and tokens, and the comments that claim one exists
become true.

ADR-0054 said that chat messages never spend a token. That remains true of
the message; sending and opening files now spend a pass an hour each.

### 6. Delete

An upload carries a **delete public key**, stored in the blob's index row.
`DELETE /blob/{hex}` requires a signature under it.

The key is derived, not stored on the client:

```text
blob_delete = BLAKE3-derive-key("lakko/blob/delete/v1", vault_root || blob_id)
```

with `vault_root` from the account identity seed (ADR-0064). So any device of
the uploading account can delete the file, including one signed in later, and
no other member can, although every member knows the blob id. Two blobs'
delete keys are unrelated to each other and to the account for anyone without
`vault_root`.

When it is used:

- **Delete for everyone.** The author's device collects the blob ids of the
  message (file and thumbnail) before it tombstones the row, publishes the
  delete, then deletes the blobs. Blob deletes that could not be sent are kept
  in a small durable list and retried.
- **An attachment that was uploaded and never sent** (removed from the
  composer, a cancelled or abandoned media send) is deleted at once where the
  device still knows the id; the rest is reclaimed by the lifetime.

A forwarded copy is a separate blob with its own key and its own uploader
(ADR-0052); deleting the original does not touch it.

### 7. What the server learns

Per blob: size, the day it appeared, the last day it was accessed, and a key
that authorises its deletion. Not the uploader, not the readers, not the
conversation. Tokens are unlinkable to the account by construction. The
residuals are the existing ones: network address and timing, and now the
grouping of downloads under one pass.

### 8. Saved history and expired files

A conversation vault (ADR-0064) stores attachment references, not files. A
device that restores history shows the "no longer available" state for a
reference whose file has gone. The vault does not keep files alive: that is
what Pin and Save are for.

## Alternatives considered

1. **Keep every file some device still has in its history.** Devices would
   refresh everything they hold; storage would shrink only when a message is
   gone from every device. Offered to the maintainer, who chose pinned and
   saved.
2. **Only pinned messages keep files.** Offered; the maintainer added Save.
3. **A token per file, with larger quotas.** Thousands of issuer evaluations
   and spent-set rows per active person per day, for uploads as well as
   downloads. Considered with the maintainer; rejected for the pass.
4. **A rate limit by address only on download.** Leaves the service usable as
   file hosting by anyone holding an id and a key. Rejected by the maintainer.
5. **A session token on blob routes.** Tells the server who uploads and who
   reads which file. Rejected: invariant #5.
6. **A delete secret carried in the message.** Every member could delete the
   author's file. Rejected.
7. **The file system's access time as "last access".** Unreliable under the
   usual mount options and invisible to the index. Rejected.
8. **Deleting an account deletes the files it uploaded.** It would break the
   histories of the people it wrote to, and the client cannot reliably
   enumerate its uploads. Not done; those files age out. To revisit with the
   account-deletion work.

## Consequences

- Storage stops growing without bound, and a deleted message's file is really
  deleted.
- A person without an account can no longer upload or download.
- Media a year old that nobody opened, pinned or saved is gone for everyone,
  including a device that restores history later. The UI says so and says how
  to prevent it.
- Media stops working when the token issuer cannot be reached and the device
  has no token or pass in hand; text is unaffected.
- `services/storage` gains the blob index, the sweep, the pass, the keep and
  delete routes and two token contexts; `lakko-sync` gains the saved register;
  the three clients gain Save and the Saved screen, the keep tick, the delete
  on delete-for-everyone, token and pass handling on the storage client, and
  one honest state for a missing file.

## Residuals and open points

1. Operations under one pass are linkable to each other.
2. A member can keep a file alive indefinitely by sending keeps.
3. A device holding a personal pin on a message it has not downloaded cannot
   keep its files until it has the message.
4. An upload whose device died before sending the message stays until the
   lifetime removes it.
5. **Found by the recon, outside this ADR:** the iOS upload goes through
   `URLSession.upload` directly and not through the anti-censorship transport,
   so an armed iOS device uploads attachments outside the tunnel
   (`apps/ios/Lakko/Storage/StorageClient.swift`). To be fixed with the
   storage client changes of this ADR.
6. No client keeps media across launches. A persistent encrypted media cache
   would cut downloads sharply and is a separate decision; this ADR does not
   depend on it.

## Sprints

1. `lakko-anon-token`: `BlobUpload` and `BlobRead`; the `auth` quotas; the
   three client mirrors. `lakko-sync`: the saved register; both FFI surfaces.
2. `services/storage`: the blob index and import, last-access accounting, the
   sweep, the pass, keep and delete, the gate on `PUT` / `GET` / `HEAD`; nginx
   rate limit. Built on the ADR-0064 index.
3. Clients: token and pass on the storage client (and the iOS transport fix);
   the "no longer available" and limit states; delete on delete-for-everyone
   and for unsent uploads; the keep tick.
4. Clients: Save, the mark, Select mode, the Saved screen.
5. A `lakko-security` pass; documents.

## Amendment (2026-11-19): blob bytes in S3-compatible object storage

**Decided by the maintainer 2026-11-19.** The production server has an 80 GB disk that
also holds MariaDB, the logs and the backup staging area; blob bytes are what grows. They
move to an S3-compatible bucket (Hetzner Object Storage, location NBG1, the same network
zone as the server, where traffic between the two is free).

1. **The storage service stays the only door.** Clients keep talking to
   `storage.lakko.me` exactly as before: the passes, budgets, the index, the delete key,
   the sweep and every route and status are unchanged. Only where the bytes rest changes.
   Clients are not given bucket URLs (presigned or otherwise): the bucket operator would
   then see each client's address next to each blob it fetches (the linkage ADR-0078
   removes), and a client behind the anti-censorship tunnel (ADR-0026) reaches only our
   server.
2. **Two backends behind one interface:** the local directory (as today; for development
   and a single small server) and S3, chosen by configuration
   (`LAKKO_STORAGE_S3_*`; unset = local). The node name in the index rows does not
   change with the backend.
3. **Upload order is unchanged in what it guarantees:** the body is streamed to a local
   temporary file and checked against its content address, then written to the bucket,
   then the index row is written. A failed bucket write leaves no row. The free-space
   guard still covers the local temporary files and the account backups, which stay on
   the local disk.
4. **Delete and sweep** delete the row first, then the object, as with files; an object
   left behind is unreachable and is reported in the log.
5. **Moving existing blobs** is a one-time command that copies every local blob file into
   the bucket (skipping objects already there, checking each copy) and deletes nothing;
   the operator removes the local files after checking.
6. **Backups.** Blob bytes leave the nightly restic backup; their durability is the
   bucket's. Losing them loses attachments, not accounts or conversations. The database
   (with the index) and the secrets are still backed up nightly.
7. **What the bucket operator learns:** the number, sizes and timing of opaque
   content-addressed ciphertext objects, all requested from one server address. The same
   as the server's own disk tells the server operator; nothing about accounts.

Library: `rusty-s3` (request signing only, `rustcrypto` feature) with the `reqwest` the
workspace already uses on `ring`; no `aws-lc` (ADR-0026's build lesson).

<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# UX spec: group and channel names that everyone sees

**Status:** design, the input to ADR-0051 and the three client builds.
**Owner surface:** cross-platform (web SolidJS / iOS SwiftUI / Android Compose):
New group, group info and members panel, chat list and header, New channel,
channel manage screen, channel list and reader header.
**Decision:** the maintainer's (2026-09-17): a group name reaches every member
and a channel name reaches every subscriber, end-to-end encrypted, and both
survive a relaunch.

## 1. Why this exists

Today a group name typed by the creator stays on the creator's device (web
loses it on reload too, iOS has no name field at all), so the same group is
"Trio" for Anna and "Anna, Web Wanda" for Ivan. A channel name stays on the
publisher's device, so subscribers see the bare handle `lakkonews0916` as the
channel's name. Both were observed live on `test.lakko.me` on 2026-09-16.

## 2. The two names

| | Group name | Channel name (+ description) |
|---|---|---|
| Who sets it | the owner or an admin (ADR-0022 tier) | the publisher (owner or co-publisher) |
| Who sees it | every member, including people added later | every subscriber, including people who subscribe later |
| Server sees it | never (an encrypted in-group message) | never (an encrypted post) |
| Unique | no | no; the `@handle` stays the unique thing |
| Length | 1 to 64 characters, one line | name 1 to 64, description 0 to 300, one paragraph |
| When absent | the title is built from member names ("Anna, Ivan, Maria", "+2" beyond three); a two-person group shows the other person | the `@handle` |

## 3. Flow

### 3.1 Group

1. **New group** (all three, iOS gains the field): the name field is optional;
   the group is created, and the name is sent into the group right after the
   first commit so the first Welcome recipients get it with their first poll.
2. **Rename**: group info / members panel, a `Group name` row visible to
   everyone and editable by the owner and admins; Save sends the new name into
   the group. A plain member sees the name read-only with no edit affordance.
3. **A newcomer** cannot read messages from before their admission, so the
   owner and admins re-send the current name when the group grows (throttled;
   the same rule the profile republish uses).
4. Everyone re-titles the chat list row, the chat header and the group info
   in place when a name arrives. There is no "X renamed the group" row in the
   transcript in this version.

### 3.2 Channel

1. **New channel**: name (required) and description (optional) as today; the
   publisher posts a metadata record into the channel right after creation.
2. **Manage channel**: name and description editable; Save posts a new record.
3. Subscribers show the newest record's name and description; until one
   lands (a brand-new subscription, or a record older than the seeds the
   subscriber holds) the row shows the `@handle`.
4. The publisher re-posts the current record after every key rotation and
   whenever the record would otherwise fall out of the server's retention
   window, so a late subscriber always finds one it can read.

## 4. States

| State | Group | Channel |
|---|---|---|
| No name yet | member-derived title | `@handle` as the title, no description |
| Name arrives | row, header and info re-title in place | row and reader header re-title in place |
| Viewer is a plain member / subscriber | name shown read-only | name shown read-only |
| Viewer is owner / admin / publisher | `Group name` row editable | name and description editable in Manage |
| Name cleared by an admin | back to the member-derived title | not offered; a channel keeps a name |
| Two admins rename at once | the newer stamp wins on every device; equal stamps keep what is stored | one publisher normally; the newer stamp wins |
| Offline while renaming | the send fails with the same error copy as a failed message; the local title does not change until the send succeeds | same |
| Relaunch | the name is stored per group on the device and shown at once | stored per channel on the device |
| Second device paired later | not carried in the pairing bundle in this version; it learns the name at the next re-send on growth or rename | not carried; it learns it from the newest record |

Validation, both kinds: trimmed; no line breaks; no invisible or
text-direction control characters (the same rule as the profile display name,
docs/ux/profile-names.md section 4); an empty group name clears the name; an
empty channel name is refused ("A channel needs a name").

## 5. Controls and data the UI needs

- Group: per group, `{ name, updatedAtMs }` in a device store that survives a
  relaunch; the own role from the live group (`ownRole`) to decide the edit
  affordance; the resolver order for the title: shared group name, then the
  member-derived title.
- Channel: per channel, `{ name, description, updatedAtMs }` from the newest
  metadata record; the publisher's own copy is the source of the next record.
- The receive side never trusts the sender's word about who they are: a group
  name is accepted only when the MLS sender is the owner or an admin at the
  moment of receipt, a channel record is accepted only because it carries a
  valid publisher signature like any post.
- Copy: `Group name`, `Save`, `A channel needs a name`, `Only admins can
  rename this group` (shown only if a stale edit control is tapped).

## 6. Cross-platform shape

One mental model on all three clients: a name is a fact shared inside the
conversation, set by the people who manage it, shown to everyone, and never
seen by the server. Web builds first; iOS and Android implement the same
rows and the same precedence. iOS adds the missing New group name field and
the group info rename row in the same change.

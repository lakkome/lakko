<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# DRAFT: Kickstarter campaign for Lakko

**Status: draft, not launched.** Written 2026-11-20, checked against Kickstarter's "Our
Rules" page as saved by the maintainer on 2026-11-20 (fees and the creator form not
checked). Fill every `TODO` before launching. The plan agreed for funding: launch the beta
(1000 accounts), wait for at least a few hundred people to use it, then run this campaign,
FLOSS/fund (`funding.json`) and the OTF Security Lab request (`otf-security-lab-draft.md`).

## Decisions taken

- **The goal pays for the production servers and the first year of running them**, not
  for the audit (maintainer, 2026-11-20). The audit is planned and funded separately
  (OTF Security Lab, FLOSS/fund, grants); the campaign says so and promises to publish
  the report when the audit is done.
- **The 1000-account limit stays until the audit**, and is lifted after it (ADR-0046).
  The campaign says this plainly, so nobody pledges expecting an open launch right after
  the campaign.

## What Kickstarter's rules mean for this campaign

From "Our Rules" (saved 2026-11-20):

1. **"Projects cannot be centered around establishing and providing services."** This is
   the main risk: "run a messenger service for a year" reads as a service. The project
   is therefore framed as the thing it creates, **Lakko 1.0: the apps and the network
   they run on**, with the deliverables listed below; the servers and their first year
   are part of its budget. Kickstarter may still judge it a service. `TODO: ask
   Kickstarter support before launching whether a messenger app with its servers fits.`
2. **App projects must demonstrate a prototype.** The beta is the prototype: a video of
   real use on the phone, web and desktop, no mock-ups.
3. **"Creators must be transparent about how AI is used."** The rules require a
   statement. `TODO: the maintainer writes it. A short factual line is enough, for
   example: "Lakko is designed and reviewed by its developer, who writes the code with
   AI coding tools."`
4. **Entity information.** VVG (sole proprietorship) and its owner must be shown.
5. **Rewards must be new and made by the project.** The digital rewards below are.
6. **Updates and honesty.** Regular updates are expected, and a failure has to be told to
   backers early.

## Title

Lakko: a private messenger you decide who can reach

## Subtitle (short blurb)

An open-source messenger where every chat, group and call is end-to-end encrypted. Help
launch it and keep it running for its first year.

## The story

### What Lakko is

Lakko is a messenger for anyone who wants to talk privately and decide for themselves
who can reach them.

- **Everything is end-to-end encrypted:** one-to-one chats, groups, channels, voice and
  video calls, files. Our servers pass only ciphertext they cannot read.
- **No phone number.** You sign up with a username and a password.
- **You choose who can find you:** nobody, contacts of your contacts, people in your
  groups and channels, or everyone. Anyone else reaches you only through an invitation
  link you share. A conversation with a new person starts only through such a link or
  when you accept their request.
- **Our servers do not keep your contact list** or the list of groups you are in.
- **It works where messengers are blocked:** the apps can reach our server through
  connections built to get past blocking.
- **Open source** under the AGPL. Anyone can read the code and check these claims.

Lakko runs on iOS, Android and the web, all on one shared core; a desktop app is in
progress.

### Where it is today

Lakko is in public beta, open to the first 1000 accounts. `TODO: number of people using
it and since when, at launch of the campaign.`

### What this campaign makes

Lakko 1.0, delivered as:

- the iOS and Android apps released in the App Store and Google Play;
- the desktop app as installers for macOS, Windows and Linux;
- the production network they run on: message delivery, calls (media servers and
  relays), file storage, push notifications, the public key log, and the relays that get
  past blocking;
- one year of running that network after launch.

If the stretch goal is reached, also call relays and anti-blocking relays in regions
outside Europe (see below).

`TODO: check each deliverable against the state at launch of the campaign; today the
desktop installers, store releases and native push are not built (docs/PRODUCT-READINESS.md,
docs/beta-launch-checklist.md).`

### What your money pays for

**Goal: about EUR 40 000** (maintainer, 2026-11-20: the base goal covers the launch and
the first year; EUR 100 000 is the stretch goal). `TODO: final figures; both rows are
upper-bound estimates from funding.json, not quotes.`

| Item | Amount |
|---|---|
| Launch: production servers set up, app store accounts, legal review of the terms and privacy policy, an EU trademark for the name, Windows and Linux machines to test the desktop app | EUR 15 000 |
| Running the network for 12 months (hosting, bandwidth for calls and files, push delivery, maintenance) | EUR 21 600 |
| Kickstarter and payment fees | `TODO` |
| **Goal** | `TODO: the sum, about EUR 40 000` |

### Stretch goal: EUR 100 000

Everything above, and:

| Item | Amount |
|---|---|
| Servers in other regions for 12 months: call relays and the relays that get past blocking, closer to people outside Europe | about EUR 6 500 (estimate below) |
| The developer's work at EUR 50 an hour: finishing Lakko 1.0 (store releases, desktop installers, native push) and keeping it running | what remains, about EUR 50 000 (about 1 000 hours: half-time for a year) |
| Kickstarter and payment fees on the amount above the goal | `TODO` |

Regions (maintainer, 2026-11-20): **the US, Asia and the Middle East.** Only the relays
(call media relays, TURN, the anti-censorship relays) can run in another region today;
message delivery, accounts and storage are one deployment in the EU, and spreading them
across regions is not designed.

**Estimate (2026-11-20, from published price pages, not quotes).** Per region two
servers (a call relay and an anti-blocking relay) at up to about EUR 40 a month each, and
10 TB a month of traffic beyond what the plans include at about EUR 10 per TB: about
EUR 180 a month, about EUR 2 160 a year, **about EUR 6 500 for the three regions**. The
10 TB is an assumption, not a measurement: the real traffic depends on how many calls go
through each region. Proposed places:

| Region | Place | Why | Price notes |
|---|---|---|---|
| US | OVHcloud, US data centre | traffic included in the plan | VPS-4 listed from USD 23.37 a month with unlimited traffic (https://us.ovhcloud.com/vps/); check the order page for a quota |
| Asia | Singapore (Hetzner or Vultr) | close to South-East and East Asia; does not block messengers | Hetzner lists USD 8.49 per extra TB (https://www.hetzner.com/cloud-singapore/); Vultr USD 0.01 per GB (https://docs.vultr.com/support/platform/billing/what-is-the-bandwidth-overage-rate). OVHcloud Singapore caps traffic at 10 Mbps after a 1-3 TB monthly quota (https://www.ovhcloud.com/asia/vps/vps-singapore/), too little for calls |
| Middle East | Cyprus (Limassol) | EU law, short distance to the region; a host inside a country that blocks messengers would itself be blocked or compelled. An address in Israel can be blocked or dangerous to reach from several countries of the region | VPS from about EUR 8 a month (https://www.edisglobal.com/vps-hosting/cyprus-limassol); plans seen cap traffic at 1-3 TB, overage price not found |

`TODO: confirm each price and traffic term on the provider's order page before the
campaign.`

The developer's work is paid at EUR 50 an hour (maintainer, 2026-11-20) and takes what
remains of EUR 100 000: about EUR 50 000, about 1 000 hours, worked half-time over the
year after launch. `TODO: the exact hours once
the Kickstarter fees are known.` Money between the goal and the stretch goal goes to the regional relays
first, then to the developer's work.

### The security audit

A messenger's promises ("we cannot read your messages") can only be trusted if someone
other than its authors has checked them. Lakko is built by one developer, so an
independent security firm will review the design and the code before Lakko leaves the
beta. This audit is planned and funded separately, through grants and security programmes
for open-source privacy tools; this campaign does not pay for it.

Until the audit, Lakko stays a beta limited to 1000 accounts and says that it has not
been audited. After it:

- the audit report is published in full, after the fixes;
- every finding rated medium or higher is fixed first, and the auditors check the fixes;
- the 1000-account limit is lifted.

The scope of the audit is already written and public:
https://github.com/lakkome/lakko/blob/main/docs/security/external-review-scope.md

## Rewards

All rewards are digital. Amounts are proposals.

| Pledge | Reward |
|---|---|
| any amount | Our thanks and project updates |
| `TODO` | Your name (or a name you choose) on the supporters page at lakko.me |
| `TODO` | The above, and your name in the supporters list inside the app (Settings > About) |
| `TODO` | The above, and a guaranteed place in the beta even when the 1000 places are taken |

The last two tiers are being built (ADR-0097, `docs/ux/supporters.md`): the list is a
signed file on lakko.me that the apps show in Settings > About, and the beta place is a
code mailed to the backer, exchanged in the app for a token that our server cannot
connect to the code or to the email address it was sent to. Both are built on every
client (2026-11-20) and not yet walked. `TODO: walk both before the campaign.`

Not offered on purpose: paid features, ads, or anything that needs us to know who you
are inside the app.

## Timeline

`TODO: dates. Order: campaign ends -> payment received -> production servers set up ->
apps released to the stores and desktop installers published -> the beta moves to the
production servers -> one year of operation. The audit runs on its own schedule; the
limit is lifted when it is done.`

## Risks and challenges

- **One developer.** The design, the reasons for it and its open problems are written
  down in the repository, so the work can be checked and continued. There is no second
  maintainer today.
- **The audit is funded separately.** If its funding is late, Lakko runs longer as a
  beta limited to 1000 accounts. The servers this campaign pays for run either way.
- **App store review** can delay a release or ask for changes.
- **Blocking.** Getting past blocking is a race; a transport that works today may be
  blocked later.
- **The goal is all-or-nothing:** if the base goal is not reached, Kickstarter pays nothing.
- **Costs** are estimates; if traffic grows faster than planned, the money covers fewer
  months, and we will say so in an update.

## FAQ

**Can you read my messages?** No. They are encrypted on your device and only the people
in the conversation can read them.

**Is it free?** Yes. The money here pays for the servers and the launch, not for access.

**Why is it still limited to 1000 accounts after the campaign?** Because it has not been
independently audited yet. We would rather keep it small than ask many people to trust
an unchecked tool.

**Who are you?** VVG, a sole proprietorship in the Netherlands (Valentin Goncharov).
Contact: info@lakko.me. `TODO: KvK number once registered.`

## Facts to collect before launching

- [ ] ZZP registered (Kickstarter pays out to a bank account and checks the creator)
- [ ] Kickstarter support's answer on the "service providers" rule
- [ ] Current fees for the Netherlands
- [ ] The AI statement
- [ ] Beta user count and launch date
- [x] Repository public (https://github.com/lakkome/lakko, since 2026-11-20)
- [ ] Final budget figures
- [ ] Reward amounts; the supporters list and supporter codes (ADR-0097) working
- [ ] A prototype video of real use

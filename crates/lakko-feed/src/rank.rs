// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The one ranking every client draws (ADR-0061 section 1).
//!
//! The client hands over its conversations and one candidate per message in
//! the window; [`rank`] returns which candidates to show, in which order,
//! with replies folded under the message they answer. The client draws the
//! indices it gets back and re-derives nothing.
//!
//! ## Integer arithmetic only
//!
//! Every score is a `u64` built from integer operations. Floating-point
//! `ln` and `exp` go through the platform's maths library, and wasm, iOS and
//! Android are not required to agree on the last bit, which is enough to
//! reorder two items with nearly equal scores on one phone and not on
//! another. That disagreement is what this crate exists to prevent.
//!
//! ## The score
//!
//! Each signal yields a component in `0..=SCALE` and the score is the sum of
//! `weight * component`:
//!
//! * recency: `2^(-age / half_life)`, from a 16-step table with linear
//!   interpolation between steps;
//! * reactions and replies (groups only): `n / (n + 1 + isqrt(audience))`,
//!   so three reactions in a five-person group count for more than three in
//!   a two-hundred-person group; a channel post has neither;
//! * from a contact, mentions or replies to the person, has media: all or
//!   nothing.
//!
//! These shapes are chosen, not measured (ADR-0061). They are constants
//! here so a change to them is one reviewed diff.
//!
//! The order is total: score descending, then time descending, then
//! conversation index, then message id. Two clients given the same input
//! return the same order.

use std::collections::BTreeMap;

use crate::rule::FeedRule;

/// Fixed-point scale of every score component.
pub const SCALE: u64 = 1 << 16;

/// Longest reply chain [`rank`] follows to find the message a reply belongs
/// under. Replies are one level deep on the wire (ADR-0052), so this only
/// bounds malformed or hostile chains.
pub const MAX_REPLY_DEPTH: usize = 8;

const MS_PER_DAY: i64 = 86_400_000;

/// `2^(-k/16)` in units of [`SCALE`], `k = 0..=16`.
const DECAY_TABLE: [u64; 17] = [
    65536, 62757, 60097, 57549, 55109, 52773, 50535, 48393, 46341, 44376, 42495, 40693, 38968,
    37316, 35734, 34219, 32768,
];

/// What kind of conversation a message comes from. One-to-one chats have
/// no value here: they are never in a feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConversationKind {
    /// A group.
    Group,
    /// A channel.
    Channel,
}

/// One conversation the client offers messages from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FeedConversation {
    /// The client's id for it, compared against the rule's include and
    /// exclude lists.
    pub id: Vec<u8>,
    /// Group or channel.
    pub kind: ConversationKind,
    /// Muted in this client.
    pub muted: bool,
    /// Number of members; ignored for a channel.
    pub audience: u32,
}

/// One message, reduced to what ranking needs.
// Each bool is an independent fact about the message that crosses the FFI
// as-is; folding them into an enum would invent states that do not exist.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FeedCandidate {
    /// Index into the conversations slice. Out of range = dropped.
    pub conversation: u32,
    /// The message id within its conversation.
    pub message_id: i64,
    /// The message it replies to, in the same conversation.
    pub reply_to: Option<i64>,
    /// When it was sent, milliseconds since the epoch.
    pub time_ms: i64,
    /// Written by the person.
    pub from_self: bool,
    /// Written by one of the person's contacts.
    pub from_contact: bool,
    /// Mentions the person (see [`crate::mentions_username`]).
    pub mentions_me: bool,
    /// Replies to one of the person's messages.
    pub replies_to_me: bool,
    /// Distinct people who reacted.
    pub reactions_distinct: u32,
    /// Carries an attachment.
    pub has_media: bool,
    /// The text, for the word filters. Never logged.
    pub text: String,
}

/// One item to draw: a candidate and the replies folded under it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FeedEntry {
    /// Index into the candidates slice.
    pub index: u32,
    /// Indices of its replies, oldest first.
    pub replies: Vec<u32>,
}

/// Rank `candidates` under `rule` at time `now_ms`.
///
/// Total: any input returns an order and nothing panics. A candidate whose
/// conversation index is out of range is dropped, and of two candidates with
/// the same conversation and message id only the first is kept.
#[must_use]
pub fn rank(
    rule: &FeedRule,
    conversations: &[FeedConversation],
    candidates: &[FeedCandidate],
    now_ms: i64,
) -> Vec<FeedEntry> {
    let window_start =
        now_ms.saturating_sub(i64::from(rule.window_days).saturating_mul(MS_PER_DAY));

    // 1. Filter. `kept` holds candidate indices in input order; `by_key`
    //    finds a kept candidate by (conversation, message id).
    let mut first_by_key: BTreeMap<(u32, i64), usize> = BTreeMap::new();
    let mut by_key: BTreeMap<(u32, i64), usize> = BTreeMap::new();
    let mut kept: Vec<usize> = Vec::new();
    for (i, c) in candidates.iter().enumerate() {
        let Some(conv) = usize::try_from(c.conversation)
            .ok()
            .and_then(|k| conversations.get(k))
        else {
            continue;
        };
        let key = (c.conversation, c.message_id);
        if first_by_key.contains_key(&key) {
            continue;
        }
        first_by_key.insert(key, i);
        if passes(rule, conv, c, window_start) {
            kept.push(i);
            by_key.insert(key, i);
        }
    }

    // 2. Fold replies under the message they answer.
    let root_of = |start: usize| -> usize {
        let mut current = start;
        let mut visited = vec![start];
        for _ in 0..MAX_REPLY_DEPTH {
            let c = &candidates[current];
            let Some(&parent) = c.reply_to.and_then(|p| by_key.get(&(c.conversation, p))) else {
                return current;
            };
            if visited.contains(&parent) {
                return start;
            }
            visited.push(parent);
            current = parent;
        }
        start
    };
    let mut children: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    let mut tops: Vec<usize> = Vec::new();
    for &k in &kept {
        let root = root_of(k);
        if root == k {
            tops.push(k);
        } else {
            children.entry(root).or_default().push(k);
        }
    }

    let mut entries: Vec<(u64, usize, Vec<usize>)> = Vec::with_capacity(tops.len());
    for k in tops {
        let mut replies = children.remove(&k).unwrap_or_default();
        replies.sort_by(|&a, &b| {
            candidates[a]
                .time_ms
                .cmp(&candidates[b].time_ms)
                .then(candidates[a].message_id.cmp(&candidates[b].message_id))
        });
        let conv = &conversations[candidates[k].conversation as usize];
        let score = score(rule, conv, &candidates[k], replies.len(), now_ms);
        entries.push((score, k, replies));
    }

    // 3. Total order.
    entries.sort_by(|a, b| {
        let (ca, cb) = (&candidates[a.1], &candidates[b.1]);
        b.0.cmp(&a.0)
            .then(cb.time_ms.cmp(&ca.time_ms))
            .then(ca.conversation.cmp(&cb.conversation))
            .then(ca.message_id.cmp(&cb.message_id))
    });

    entries
        .into_iter()
        .map(|(_, k, replies)| FeedEntry {
            index: to_u32(k),
            replies: replies.into_iter().map(to_u32).collect(),
        })
        .collect()
}

fn to_u32(i: usize) -> u32 {
    u32::try_from(i).unwrap_or(u32::MAX)
}

fn passes(rule: &FeedRule, conv: &FeedConversation, c: &FeedCandidate, window_start: i64) -> bool {
    let kind_ok = match conv.kind {
        ConversationKind::Group => rule.groups,
        ConversationKind::Channel => rule.channels,
    };
    if !kind_ok {
        return false;
    }
    if !rule.include.is_empty() && !rule.include.contains(&conv.id) {
        return false;
    }
    if rule.exclude.contains(&conv.id) {
        return false;
    }
    if conv.muted && !rule.include_muted {
        return false;
    }
    if c.time_ms < window_start {
        return false;
    }
    if rule.media_only && !c.has_media {
        return false;
    }
    if rule.hide_own && c.from_self {
        return false;
    }
    if rule.for_me
        && !(c.mentions_me || c.replies_to_me || (c.from_self && c.reactions_distinct > 0))
    {
        return false;
    }
    if c.reactions_distinct < u32::from(rule.min_reactions) {
        return false;
    }
    if rule.hide_words.is_empty() && rule.only_words.is_empty() {
        return true;
    }
    let text = c.text.to_lowercase();
    if rule.hide_words.iter().any(|w| contains_word(&text, w)) {
        return false;
    }
    rule.only_words.is_empty() || rule.only_words.iter().any(|w| contains_word(&text, w))
}

fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

/// Whether `word` occurs in `text` with no word character directly before
/// or after it. Both are expected lowercase.
pub(crate) fn contains_word(text: &str, word: &str) -> bool {
    if word.is_empty() {
        return false;
    }
    let mut from = 0;
    while let Some(off) = text[from..].find(word) {
        let start = from + off;
        let end = start + word.len();
        let before_ok = text[..start]
            .chars()
            .next_back()
            .map_or(true, |ch| !is_word_char(ch));
        let after_ok = text[end..]
            .chars()
            .next()
            .map_or(true, |ch| !is_word_char(ch));
        if before_ok && after_ok {
            return true;
        }
        // Advance by one character, not one byte, to stay on a boundary.
        from = start + text[start..].chars().next().map_or(1, char::len_utf8);
    }
    false
}

fn score(
    rule: &FeedRule,
    conv: &FeedConversation,
    c: &FeedCandidate,
    reply_count: usize,
    now_ms: i64,
) -> u64 {
    let w = rule.weights;
    let age = now_ms.saturating_sub(c.time_ms).max(0);
    let recency = decay(age, rule.half_life.millis());
    let (reactions, replies) = match conv.kind {
        ConversationKind::Group => (
            engagement(u64::from(c.reactions_distinct), u64::from(conv.audience)),
            engagement(reply_count as u64, u64::from(conv.audience)),
        ),
        ConversationKind::Channel => (0, 0),
    };
    let flag = |b: bool| if b { SCALE } else { 0 };
    u64::from(w.recency) * recency
        + u64::from(w.reactions) * reactions
        + u64::from(w.replies) * replies
        + u64::from(w.from_contacts) * flag(c.from_contact)
        + u64::from(w.mentions_me) * flag(c.mentions_me || c.replies_to_me)
        + u64::from(w.has_media) * flag(c.has_media)
}

/// `2^(-age/half_life)` in units of [`SCALE`], integer only.
pub(crate) fn decay(age_ms: i64, half_life_ms: i64) -> u64 {
    if half_life_ms <= 0 {
        return 0;
    }
    let age = u128::try_from(age_ms.max(0)).unwrap_or(0);
    let hl = u128::try_from(half_life_ms).unwrap_or(1);
    let halvings = age / hl;
    if halvings >= 64 {
        return 0;
    }
    // Position within the current half-life, in 1/16 steps plus remainder.
    let frac = age % hl;
    let step = frac * 16 / hl;
    let rem = frac * 16 % hl;
    let i = usize::try_from(step).unwrap_or(15);
    let hi = u128::from(DECAY_TABLE[i]);
    let lo = u128::from(DECAY_TABLE[i + 1]);
    let within = hi - (hi - lo) * rem / hl;
    u64::try_from(within >> halvings).unwrap_or(0)
}

/// Integer square root, rounded down. `u64::isqrt` is newer than the
/// workspace's minimum Rust version.
pub(crate) fn isqrt(n: u64) -> u64 {
    if n < 2 {
        return n;
    }
    // Newton's method from an over-estimate; strictly decreasing until it
    // reaches the floor of the root.
    let mut x = n;
    let mut y = x.div_ceil(2);
    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }
    x
}

/// `n / (n + 1 + isqrt(audience))` in units of [`SCALE`].
pub(crate) fn engagement(n: u64, audience: u64) -> u64 {
    if n == 0 {
        return 0;
    }
    let denom = n.saturating_add(1).saturating_add(isqrt(audience));
    let num = u128::from(n) * u128::from(SCALE);
    u64::try_from(num / u128::from(denom)).unwrap_or(SCALE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preset::{preset_rule, Preset};

    const NOW: i64 = 1_800_000_000_000;
    const HOUR: i64 = 3_600_000;

    fn group(id: u8, audience: u32) -> FeedConversation {
        FeedConversation {
            id: vec![id],
            kind: ConversationKind::Group,
            muted: false,
            audience,
        }
    }

    fn channel(id: u8) -> FeedConversation {
        FeedConversation {
            id: vec![id],
            kind: ConversationKind::Channel,
            muted: false,
            audience: 0,
        }
    }

    fn msg(conversation: u32, message_id: i64, age: i64) -> FeedCandidate {
        FeedCandidate {
            conversation,
            message_id,
            reply_to: None,
            time_ms: NOW - age,
            from_self: false,
            from_contact: false,
            mentions_me: false,
            replies_to_me: false,
            reactions_distinct: 0,
            has_media: false,
            text: String::new(),
        }
    }

    fn order(entries: &[FeedEntry]) -> Vec<u32> {
        entries.iter().map(|e| e.index).collect()
    }

    #[test]
    #[allow(clippy::cast_precision_loss)] // test-only comparison against f64
    fn decay_table_is_two_to_the_minus_k_over_16() {
        // Pin the table against the value it claims to be, so an edit that
        // breaks it fails here rather than as a subtle reordering.
        for (k, v) in DECAY_TABLE.iter().enumerate() {
            let exact = 65536.0 * 2f64.powf(-(k as f64) / 16.0);
            assert!((exact - *v as f64).abs() <= 1.0, "k={k}");
        }
        assert_eq!(decay(0, HOUR), SCALE);
        assert_eq!(decay(HOUR, HOUR), SCALE / 2);
        assert_eq!(decay(2 * HOUR, HOUR), SCALE / 4);
        assert_eq!(decay(64 * HOUR, HOUR), 0);
        assert_eq!(decay(-5, HOUR), SCALE);
    }

    #[test]
    fn decay_never_increases_with_age() {
        let mut last = u64::MAX;
        for age in (0..(3 * HOUR)).step_by(97_000) {
            let d = decay(age, HOUR);
            assert!(d <= last, "age {age}");
            last = d;
        }
    }

    #[test]
    fn isqrt_is_the_floor_of_the_root() {
        for n in (0u64..10_000).chain([u64::MAX, u64::MAX - 1, 1 << 62]) {
            let r = isqrt(n);
            assert!(u128::from(r) * u128::from(r) <= u128::from(n), "{n}");
            assert!(u128::from(r + 1) * u128::from(r + 1) > u128::from(n), "{n}");
        }
    }

    #[test]
    fn the_same_count_weighs_more_in_a_smaller_group() {
        assert!(engagement(3, 5) > engagement(3, 200));
        assert_eq!(engagement(0, 5), 0);
        assert!(engagement(u64::MAX, u64::MAX) <= SCALE);
    }

    #[test]
    fn latest_is_newest_first() {
        let convs = [group(1, 5), channel(2)];
        let cands = [msg(0, 1, 3 * HOUR), msg(1, 7, HOUR), msg(0, 2, 2 * HOUR)];
        let out = rank(&preset_rule(Preset::Latest), &convs, &cands, NOW);
        assert_eq!(order(&out), [1, 2, 0]);
    }

    #[test]
    fn popular_lifts_engagement_but_not_on_channels() {
        let convs = [group(1, 5), channel(2)];
        let mut hot = msg(0, 1, 20 * HOUR);
        hot.reactions_distinct = 4;
        let mut fresh_post = msg(1, 1, HOUR);
        fresh_post.reactions_distinct = 1000; // ignored for a channel
        let quiet = msg(0, 2, 2 * HOUR);
        let out = rank(
            &preset_rule(Preset::Popular),
            &convs,
            &[quiet, hot, fresh_post],
            NOW,
        );
        assert_eq!(out[0].index, 1, "the reacted-to group message ranks first");
    }

    #[test]
    fn replies_fold_under_their_parent_oldest_first() {
        let convs = [group(1, 5)];
        let parent = msg(0, 10, 5 * HOUR);
        let mut r1 = msg(0, 12, HOUR);
        r1.reply_to = Some(10);
        let mut r2 = msg(0, 11, 2 * HOUR);
        r2.reply_to = Some(10);
        let mut orphan = msg(0, 13, HOUR);
        orphan.reply_to = Some(99);
        let out = rank(
            &preset_rule(Preset::Latest),
            &convs,
            &[parent, r1, r2, orphan],
            NOW,
        );
        let top = out.iter().find(|e| e.index == 0).unwrap();
        assert_eq!(top.replies, [2, 1]);
        assert!(
            out.iter().any(|e| e.index == 3),
            "an orphan reply stands alone"
        );
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn a_reply_cycle_does_not_hang_or_lose_messages() {
        let convs = [group(1, 5)];
        let mut a = msg(0, 1, HOUR);
        a.reply_to = Some(2);
        let mut b = msg(0, 2, HOUR);
        b.reply_to = Some(1);
        let out = rank(&preset_rule(Preset::Latest), &convs, &[a, b], NOW);
        let mut all: Vec<u32> = out
            .iter()
            .flat_map(|e| std::iter::once(e.index).chain(e.replies.clone()))
            .collect();
        all.sort_unstable();
        assert_eq!(all, [0, 1]);
    }

    #[test]
    fn filters_apply() {
        let mut convs = vec![group(1, 5), channel(2), group(3, 5)];
        convs[2].muted = true;
        let mut own = msg(0, 1, HOUR);
        own.from_self = true;
        let mut spoiler = msg(0, 2, HOUR);
        spoiler.text = "Big SPOILER ahead".into();
        let mut spoilers = msg(0, 3, HOUR);
        spoilers.text = "no spoilers here".into();
        let old = msg(0, 4, 40 * 24 * HOUR);
        let post = msg(1, 1, HOUR);
        let muted = msg(2, 1, HOUR);
        let cands = [own, spoiler, spoilers, old, post, muted];

        let mut rule = preset_rule(Preset::Latest);
        rule.channels = false;
        rule.hide_own = true;
        rule.hide_words = vec!["spoiler".into()];
        let out = rank(&rule, &convs, &cands, NOW);
        // Whole words only: "spoilers" is not "spoiler".
        assert_eq!(order(&out), [2]);

        rule.include_muted = true;
        let out = rank(&rule, &convs, &cands, NOW);
        assert!(out.iter().any(|e| e.index == 5));
    }

    #[test]
    fn include_and_exclude() {
        let convs = [group(1, 5), group(2, 5)];
        let cands = [msg(0, 1, HOUR), msg(1, 1, HOUR)];
        let mut rule = preset_rule(Preset::Latest);
        rule.include = vec![vec![2]];
        assert_eq!(order(&rank(&rule, &convs, &cands, NOW)), [1]);
        rule.include.clear();
        rule.exclude = vec![vec![2]];
        assert_eq!(order(&rank(&rule, &convs, &cands, NOW)), [0]);
    }

    #[test]
    fn for_me_takes_mentions_replies_and_own_reacted_messages() {
        let convs = [group(1, 5)];
        let mut mention = msg(0, 1, HOUR);
        mention.mentions_me = true;
        let mut reply = msg(0, 2, HOUR);
        reply.replies_to_me = true;
        let mut mine = msg(0, 3, HOUR);
        mine.from_self = true;
        mine.reactions_distinct = 2;
        let mut mine_quiet = msg(0, 4, HOUR);
        mine_quiet.from_self = true;
        let other = msg(0, 5, HOUR);
        let out = rank(
            &preset_rule(Preset::ForMe),
            &convs,
            &[mention, reply, mine, mine_quiet, other],
            NOW,
        );
        let mut got = order(&out);
        got.sort_unstable();
        assert_eq!(got, [0, 1, 2]);
    }

    #[test]
    fn out_of_range_and_duplicates_are_dropped() {
        let convs = [group(1, 5)];
        let cands = [msg(0, 1, HOUR), msg(0, 1, 2 * HOUR), msg(7, 1, HOUR)];
        assert_eq!(
            order(&rank(&preset_rule(Preset::Latest), &convs, &cands, NOW)),
            [0]
        );
    }

    #[test]
    fn contains_word_respects_boundaries_and_unicode() {
        assert!(contains_word("big spoiler ahead", "spoiler"));
        assert!(!contains_word("spoilers", "spoiler"));
        assert!(contains_word("это спойлер!", "спойлер"));
        assert!(!contains_word("спойлеры", "спойлер"));
        assert!(contains_word("два слова тут", "два слова"));
        assert!(!contains_word("anything", ""));
    }

    mod properties {
        use super::*;
        use proptest::prelude::*;

        fn arb_candidate() -> impl Strategy<Value = FeedCandidate> {
            (
                0u32..4,
                0i64..40,
                proptest::option::of(0i64..40),
                -(40 * 24 * HOUR)..(2 * HOUR),
                any::<[bool; 5]>(),
                0u32..20,
                "[a-z ]{0,12}",
            )
                .prop_map(
                    |(conversation, message_id, reply_to, age, b, reactions, text)| FeedCandidate {
                        conversation,
                        message_id,
                        reply_to,
                        time_ms: NOW - age,
                        from_self: b[0],
                        from_contact: b[1],
                        mentions_me: b[2],
                        replies_to_me: b[3],
                        reactions_distinct: reactions,
                        has_media: b[4],
                        text,
                    },
                )
        }

        type Key = (u32, i64);

        fn keys(cands: &[FeedCandidate], out: &[FeedEntry]) -> Vec<(u32, i64, Vec<Key>)> {
            out.iter()
                .map(|e| {
                    let c = &cands[e.index as usize];
                    let replies = e
                        .replies
                        .iter()
                        .map(|&r| (cands[r as usize].conversation, cands[r as usize].message_id))
                        .collect();
                    (c.conversation, c.message_id, replies)
                })
                .collect()
        }

        proptest! {
            #[test]
            fn order_does_not_depend_on_input_order(
                mut cands in proptest::collection::vec(arb_candidate(), 0..40),
                shuffle in any::<u64>(),
                preset in 0usize..3,
            ) {
                // Unique keys: which of two duplicates is "first" does depend
                // on input order, by definition.
                let mut seen = std::collections::BTreeSet::new();
                cands.retain(|c| seen.insert((c.conversation, c.message_id)));
                let convs = [group(1, 5), group(2, 300), channel(3), group(4, 2)];
                let rule = preset_rule(crate::preset::ALL_PRESETS[preset]);
                let a = rank(&rule, &convs, &cands, NOW);
                let mut permuted = cands.clone();
                let n = permuted.len();
                if n > 1 {
                    let mut x = shuffle;
                    for i in (1..n).rev() {
                        x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                        let j = usize::try_from(x >> 33).unwrap() % (i + 1);
                        permuted.swap(i, j);
                    }
                }
                let b = rank(&rule, &convs, &permuted, NOW);
                prop_assert_eq!(keys(&cands, &a), keys(&permuted, &b));
            }

            #[test]
            fn every_index_appears_at_most_once(
                cands in proptest::collection::vec(arb_candidate(), 0..60),
                now in any::<i64>(),
            ) {
                let convs = [group(1, 5), group(2, 300), channel(3)];
                let out = rank(&preset_rule(Preset::Popular), &convs, &cands, now);
                let mut all: Vec<u32> = out
                    .iter()
                    .flat_map(|e| std::iter::once(e.index).chain(e.replies.iter().copied()))
                    .collect();
                let before = all.len();
                all.sort_unstable();
                all.dedup();
                prop_assert_eq!(all.len(), before);
                prop_assert!(all.iter().all(|&i| (i as usize) < cands.len()));
            }
        }
    }
}

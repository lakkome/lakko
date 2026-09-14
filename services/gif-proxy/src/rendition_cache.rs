// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Short-lived in-memory map from `(account, GIF id)` to the renditions GIPHY
//! returned for it.
//!
//! ## Why it exists
//!
//! A results page names 24 GIFs, and the picker then fetches 24 previews by id.
//! Without this map every preview would cost one more GIPHY API call
//! (`GET /v1/gifs/{id}`) on top of the search, spending the API key's quota 25
//! times per page. With it, a preview right after a search goes straight to the
//! media host; the id lookup is only the fallback.
//!
//! ## Why it is keyed per account
//!
//! A cache shared across accounts would answer faster for an id someone else
//! recently searched, so a signed-in account could time media requests to learn
//! what other people searched for. Keying on the account closes that: a hit is
//! only possible for an id this account itself was just shown.
//!
//! Nothing here is persisted or logged. Entries expire after [`DEFAULT_TTL`]
//! and the map is capped at [`DEFAULT_MAX_ENTRIES`] (expired entries go first,
//! then the oldest).

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::giphy::Renditions;
use crate::rate_limit::AccountKey;

/// How long a looked-up id stays usable. Rendition URLs are stable for far
/// longer; this bounds how long the map remembers what an account browsed.
pub(crate) const DEFAULT_TTL: Duration = Duration::from_secs(10 * 60);
/// Entry ceiling across all accounts.
pub(crate) const DEFAULT_MAX_ENTRIES: usize = 8192;

struct Entry {
    renditions: Renditions,
    inserted: Instant,
}

pub(crate) struct RenditionCache {
    entries: Mutex<HashMap<(AccountKey, String), Entry>>,
    ttl: Duration,
    max_entries: usize,
}

impl RenditionCache {
    pub(crate) fn new(ttl: Duration, max_entries: usize) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            ttl,
            max_entries: max_entries.max(1),
        }
    }

    pub(crate) fn get_at(&self, account: AccountKey, id: &str, now: Instant) -> Option<Renditions> {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let key = (account, id.to_owned());
        let expired = now.saturating_duration_since(entries.get(&key)?.inserted) >= self.ttl;
        if expired {
            entries.remove(&key);
            return None;
        }
        entries.get(&key).map(|e| e.renditions.clone())
    }

    pub(crate) fn insert_at(
        &self,
        account: AccountKey,
        id: String,
        renditions: Renditions,
        now: Instant,
    ) {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let key = (account, id);
        if !entries.contains_key(&key) && entries.len() >= self.max_entries {
            let ttl = self.ttl;
            entries.retain(|_, e| now.saturating_duration_since(e.inserted) < ttl);
            if entries.len() >= self.max_entries {
                if let Some(oldest) = entries
                    .iter()
                    .min_by_key(|(_, e)| e.inserted)
                    .map(|(k, _)| k.clone())
                {
                    entries.remove(&oldest);
                }
            }
        }
        entries.insert(
            key,
            Entry {
                renditions,
                inserted: now,
            },
        );
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::giphy::RenditionRef;

    fn renditions(tag: &str) -> Renditions {
        Renditions {
            preview: RenditionRef {
                url: format!("https://media.giphy.com/{tag}/p.gif"),
                declared_size: None,
            },
            full: RenditionRef {
                url: format!("https://media.giphy.com/{tag}/f.gif"),
                declared_size: None,
            },
            width: 10,
            height: 10,
        }
    }

    #[test]
    fn a_hit_is_per_account() {
        let cache = RenditionCache::new(Duration::from_secs(60), 16);
        let now = Instant::now();
        cache.insert_at([1; 16], "abc".into(), renditions("a"), now);
        assert_eq!(cache.get_at([1; 16], "abc", now), Some(renditions("a")));
        assert_eq!(cache.get_at([2; 16], "abc", now), None);
    }

    #[test]
    fn entries_expire() {
        let cache = RenditionCache::new(Duration::from_secs(60), 16);
        let now = Instant::now();
        cache.insert_at([1; 16], "abc".into(), renditions("a"), now);
        assert!(cache
            .get_at([1; 16], "abc", now + Duration::from_secs(59))
            .is_some());
        assert!(cache
            .get_at([1; 16], "abc", now + Duration::from_secs(60))
            .is_none());
        assert_eq!(cache.len(), 0, "an expired hit is removed");
    }

    #[test]
    fn the_map_is_bounded_expired_first_then_oldest() {
        let cache = RenditionCache::new(Duration::from_secs(60), 2);
        let t0 = Instant::now();
        cache.insert_at([1; 16], "old".into(), renditions("o"), t0);
        cache.insert_at(
            [1; 16],
            "mid".into(),
            renditions("m"),
            t0 + Duration::from_secs(30),
        );
        // "old" is expired at t0+61: it goes, "mid" stays.
        let t1 = t0 + Duration::from_secs(61);
        cache.insert_at([1; 16], "new".into(), renditions("n"), t1);
        assert_eq!(cache.len(), 2);
        assert!(cache.get_at([1; 16], "mid", t1).is_some());
        assert!(cache.get_at([1; 16], "old", t1).is_none());
        // Nothing expired: the oldest ("mid") goes.
        cache.insert_at([1; 16], "newer".into(), renditions("x"), t1);
        assert_eq!(cache.len(), 2);
        assert!(cache.get_at([1; 16], "mid", t1).is_none());
        assert!(cache.get_at([1; 16], "new", t1).is_some());
        assert!(cache.get_at([1; 16], "newer", t1).is_some());
    }
}

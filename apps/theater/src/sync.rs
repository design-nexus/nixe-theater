//! Incremental library sync. A library synced before asks its server only for what
//! changed since (metadata and watch state) and lays that over its snapshot. Removals
//! don't show up as changes, so the server's counts are compared with what the last sync
//! counted plus what's new; any difference, a manual refresh, or a day since the last
//! full sync fetches the whole library again.
use nixe_media_cache::SyncMarks;
use nixe_media_model::{ItemId, MediaItem};
use std::collections::HashMap;

/// The longest a library goes without a full sync.
pub const FULL_EVERY: i64 = 24 * 3600;
/// How far back past the last sync to ask, for server clocks that disagree with ours.
pub const SLACK: i64 = 600;

/// A library brought up to date: its titles, and the server's counts.
pub type Synced = (Vec<MediaItem>, Vec<u64>);

#[derive(Debug, PartialEq)]
pub enum Plan {
    Full,
    /// Ask for changes after this time (Unix seconds).
    Since(i64),
}

/// How to sync a library with `kinds` counted kinds, given what the last sync left.
pub fn plan(marks: Option<&SyncMarks>, kinds: usize, now: i64, full: bool) -> Plan {
    match marks {
        Some(m) if !full && m.counts.len() == kinds && m.synced_at <= now && now - m.full_at < FULL_EVERY => {
            Plan::Since(m.synced_at - SLACK)
        }
        _ => Plan::Full,
    }
}

/// `cached` with `changed` laid over it by id (the last change of a title wins; new
/// titles go at the end), and how many titles of each kind are new. `kind_of` gives a
/// title's kind index; titles without one are kept but not counted.
pub fn apply(
    mut cached: Vec<MediaItem>,
    changed: Vec<MediaItem>,
    kinds: usize,
    kind_of: impl Fn(&MediaItem) -> Option<usize>,
) -> (Vec<MediaItem>, Vec<u64>) {
    let mut at: HashMap<ItemId, usize> = cached.iter().enumerate().map(|(i, item)| (item.id.clone(), i)).collect();
    let known = cached.len();
    let mut added = vec![0; kinds];
    for item in changed {
        match at.get(&item.id) {
            Some(&i) => cached[i] = item,
            None => {
                if let Some(k) = kind_of(&item).filter(|k| *k < kinds) {
                    added[k] += 1;
                }
                at.insert(item.id.clone(), cached.len());
                cached.push(item);
            }
        }
    }
    debug_assert!(cached.len() >= known);
    (cached, added)
}

/// Nothing was removed if every count grew by exactly the titles added.
pub fn nothing_removed(then: &[u64], now: &[u64], added: &[u64]) -> bool {
    then.len() == now.len() && now.len() == added.len() && then.iter().zip(added).zip(now).all(|((t, a), n)| t + a == *n)
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nixe_media_model::{MediaKind, ProviderKind, ServerId};

    fn item(id: &str, kind: MediaKind, title: &str) -> MediaItem {
        MediaItem {
            id: ItemId { server: ServerId { provider: ProviderKind::Plex, id: "s".into() }, id: id.into() },
            library_id: "1".into(),
            kind,
            title: title.into(),
            tv: Default::default(),
            metadata: Default::default(),
            summary: String::new(),
            year: None,
            external_ids: Default::default(),
            parent: None,
            series: None,
            season_number: None,
            episode_number: None,
            duration_ms: None,
            added_at: None,
            poster: None,
            backdrop: None,
            versions: vec![],
            watch: Default::default(),
        }
    }

    #[test]
    fn plans_full_syncs_when_needed() {
        let marks = SyncMarks { synced_at: 5_000, full_at: 1_000, counts: vec![3] };
        assert_eq!(plan(Some(&marks), 1, 6_000, false), Plan::Since(5_000 - SLACK));
        assert_eq!(plan(Some(&marks), 1, 6_000, true), Plan::Full);
        assert_eq!(plan(None, 1, 6_000, false), Plan::Full);
        // Another kind count, a clock that went back, or a day since the full sync.
        assert_eq!(plan(Some(&marks), 3, 6_000, false), Plan::Full);
        assert_eq!(plan(Some(&marks), 1, 4_000, false), Plan::Full);
        assert_eq!(plan(Some(&marks), 1, 1_000 + FULL_EVERY, false), Plan::Full);
    }

    #[test]
    fn lays_changes_over_the_snapshot_and_counts_new_titles() {
        let cached = vec![item("1", MediaKind::Series, "Show"), item("2", MediaKind::Episode, "Pilot")];
        let changed = vec![
            item("2", MediaKind::Episode, "Pilot (renamed)"),
            item("3", MediaKind::Episode, "Second"),
            // Seen twice: changed metadata and newly watched.
            item("3", MediaKind::Episode, "Second, watched"),
            item("9", MediaKind::Collection, "Box"),
        ];
        let kind_of = |i: &MediaItem| match i.kind {
            MediaKind::Series => Some(0),
            MediaKind::Season => Some(1),
            MediaKind::Episode => Some(2),
            _ => None,
        };
        let (items, added) = apply(cached, changed, 3, kind_of);
        let titles: Vec<_> = items.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(titles, ["Show", "Pilot (renamed)", "Second, watched", "Box"]);
        assert_eq!(added, [0, 0, 1]);
        assert!(nothing_removed(&[1, 0, 1], &[1, 0, 2], &added));
        // One title went while one came: the server counts one fewer than expected.
        assert!(!nothing_removed(&[1, 0, 2], &[1, 0, 2], &added));
        assert!(!nothing_removed(&[1], &[1, 0, 2], &added));
    }
}

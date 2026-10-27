//! Manual reconciliation policy. Different cuts never inherit each other's offsets.
use crate::{MediaItem, WatchState, index::edition_key};
pub fn progress_compatible(a: &MediaItem, b: &MediaItem) -> bool {
    a.duration_ms.zip(b.duration_ms).is_some_and(|(a, b)| a.abs_diff(b) <= 5000)
        && !a.versions.is_empty()
        && !b.versions.is_empty()
        && a.versions.iter().all(|left| {
            b.versions.iter().all(|right| edition_key(left.edition.as_deref()) == edition_key(right.edition.as_deref()))
        })
}
pub fn changed(a: &WatchState, b: &WatchState) -> bool {
    a.watched != b.watched || a.position_ms != b.position_ms || a.updated_at != b.updated_at
}
pub fn desired(source: &MediaItem, target: &MediaItem) -> WatchState {
    WatchState {
        watched: source.watch.watched,
        position_ms: if progress_compatible(source, target) {
            source.watch.position_ms.min(target.duration_ms.unwrap_or(u64::MAX))
        } else {
            target.watch.position_ms
        },
        updated_at: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cuts_and_conflicts() {
        let mut a = crate::index::tests::item("plex", "a", Some("1"), None);
        let mut b = crate::index::tests::item("jelly", "b", Some("1"), None);
        a.duration_ms = Some(600000);
        b.duration_ms = Some(600000);
        a.watch.position_ms = 42000;
        b.watch.position_ms = 10000;
        assert_eq!(desired(&a, &b).position_ms, 42000);
        b.versions[0].edition = Some("Different cut".into());
        assert_eq!(desired(&a, &b).position_ms, 10000);
        assert!(changed(&a.watch, &b.watch));
        assert_eq!(crate::Reconciliation::default(), crate::Reconciliation::Disabled);
    }
}

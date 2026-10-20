//! Conservative logical grouping. Sources and their editions remain independent.
use crate::{ExternalIds, ItemId, MediaItem, MediaKind};
use std::collections::{HashMap, HashSet};

#[derive(Clone)]
pub struct UnifiedItem {
    pub sources: Vec<MediaItem>,
}

type Key = (u8, String, String);
fn number(value: &Option<String>) -> Option<String> {
    let value = value.as_deref()?.trim();
    if value.is_empty() || !value.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let value = value.trim_start_matches('0');
    (!value.is_empty()).then(|| value.to_owned())
}
fn ids(ids: &ExternalIds) -> Vec<(String, String)> {
    let mut result = Vec::new();
    if let Some(id) = number(&ids.tmdb) {
        result.push(("tmdb".into(), id));
    }
    if let Some(id) = number(&ids.tvdb) {
        result.push(("tvdb".into(), id));
    }
    if let Some(id) = ids.imdb.as_deref().map(str::trim).map(str::to_ascii_lowercase)
        && id.starts_with("tt") && id.len() > 2 && id[2..].bytes().all(|c| c.is_ascii_digit()) {
            result.push(("imdb".into(), id));
        }
    result
}
fn keys(item: &MediaItem, series: &HashMap<ItemId, ExternalIds>) -> Vec<Key> {
    let kind = match item.kind {
        MediaKind::Movie => 0,
        MediaKind::Series => 1,
        MediaKind::Episode => 2,
        _ => return vec![],
    };
    let mut result: Vec<_> = ids(&item.external_ids).into_iter().map(|(namespace, id)| (kind, namespace, id)).collect();
    if item.kind == MediaKind::Episode
        && let (Some(series), Some(season), Some(episode)) =
            (item.series.as_ref().and_then(|id| series.get(id)), item.season_number, item.episode_number)
        {
            result.extend(
                ids(series)
                    .into_iter()
                    .map(|(namespace, id)| (kind, format!("series-{namespace}"), format!("{id}/{season}/{episode}"))),
            );
        }
    result
}
fn compatible(a: &[Key], b: &[Key]) -> bool {
    a.iter().all(|(kind, namespace, id)| {
        !b.iter()
            .any(|(other_kind, other_namespace, other_id)| kind == other_kind && namespace == other_namespace && id != other_id)
    })
}

/// No title/year guessing: absent, invalid or conflicting IDs leave titles separate.
/// Identifier bridges may join groups only when every known identifier agrees.
pub fn unify(items: impl IntoIterator<Item = MediaItem>) -> Vec<UnifiedItem> {
    let items: Vec<_> = items.into_iter().collect();
    let series: HashMap<_, _> =
        items.iter().filter(|i| i.kind == MediaKind::Series).map(|i| (i.id.clone(), i.external_ids.clone())).collect();
    let mut groups: Vec<Option<UnifiedItem>> = Vec::new();
    let mut aliases: HashMap<Key, Vec<usize>> = HashMap::new();
    let mut known = HashSet::new();
    for item in items {
        if !known.insert(item.id.clone()) {
            continue;
        }
        let identity = keys(&item, &series);
        let mut candidates: Vec<_> = identity
            .iter()
            .filter_map(|key| aliases.get(key))
            .flatten()
            .copied()
            .filter(|&index| groups[index].is_some())
            .collect();
        candidates.sort_unstable();
        candidates.dedup();
        let candidate_keys: Vec<_> = candidates
            .iter()
            .map(|&index| groups[index].as_ref().unwrap().sources.iter().flat_map(|item| keys(item, &series)).collect::<Vec<_>>())
            .collect();
        let mut all_keys = identity.clone();
        let consistent = candidate_keys.iter().all(|other| {
            let matches = compatible(&all_keys, other);
            all_keys.extend(other.iter().cloned());
            matches
        });
        let mut sources = Vec::new();
        let target = if consistent && !candidates.is_empty() {
            let target = candidates[0];
            for index in candidates {
                sources.extend(groups[index].take().unwrap().sources);
            }
            target
        } else {
            all_keys = identity;
            groups.push(None);
            groups.len() - 1
        };
        sources.push(item);
        groups[target] = Some(UnifiedItem { sources });
        for key in all_keys {
            let aliases = aliases.entry(key).or_default();
            if !aliases.contains(&target) {
                aliases.push(target);
            }
        }
    }
    groups.into_iter().flatten().collect()
}

/// Edition labels are conservative: an unspecified cut does not imply theatrical.
pub fn edition_key(edition: Option<&str>) -> String {
    edition.unwrap_or_default().split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{MediaVersion, ProviderKind, ServerId, WatchState};
    pub(crate) fn item(server: &str, id: &str, tmdb: Option<&str>, imdb: Option<&str>) -> MediaItem {
        MediaItem {
            id: ItemId {
                server: ServerId {
                    provider: if server == "plex" { ProviderKind::Plex } else { ProviderKind::Jellyfin },
                    id: server.into(),
                },
                id: id.into(),
            },
            library_id: "library".into(),
            kind: MediaKind::Movie,
            title: "Same title".into(),
            tv: Default::default(),
            metadata: Default::default(),
            summary: String::new(),
            year: Some(2000),
            external_ids: ExternalIds { tmdb: tmdb.map(String::from), imdb: imdb.map(String::from), tvdb: None },
            parent: None,
            series: None,
            season_number: None,
            episode_number: None,
            duration_ms: None,
            added_at: None,
            poster: None,
            backdrop: None,
            versions: vec![MediaVersion {
                id: "version".into(),
                edition: None,
                parts: vec!["part".into()],
                part_durations_ms: vec![],
                video_codec: None,
                container: None,
                width: None,
                height: None,
                technical: Default::default(),
            }],
            watch: WatchState::default(),
        }
    }
    #[test]
    fn bridges_ids_without_merging_conflicts_or_title_only_matches() {
        let a = item("plex", "a", Some("0012"), None);
        let b = item("jelly", "b", None, Some("tt123"));
        let bridge = item("plex", "bridge", Some("12"), Some("tt123"));
        for order in [[0, 1, 2], [2, 1, 0], [1, 0, 2], [1, 2, 0], [0, 2, 1], [2, 0, 1]] {
            let items = [a.clone(), b.clone(), bridge.clone()];
            let grouped = unify(order.map(|i| items[i].clone()));
            assert_eq!(grouped.len(), 1);
            assert_eq!(grouped[0].sources.len(), 3);
        }
        let conflict = item("jelly", "conflict", Some("12"), Some("tt999"));
        let mut remake = item("plex", "remake", Some("13"), Some("tt456"));
        remake.year = Some(2020);
        assert_eq!(unify([bridge, conflict, remake, item("plex", "unknown", None, None)]).len(), 4);
    }
    #[test]
    fn preserves_editions_and_each_sources_watch_state() {
        let a = item("plex", "a", Some("12"), None);
        let mut b = item("jelly", "b", Some("12"), None);
        b.versions[0].edition = Some("Director's cut".into());
        b.watch.position_ms = 42_000;
        let grouped = unify([a.clone(), a, b]);
        assert_eq!(grouped.len(), 1);
        assert_eq!(grouped[0].sources.len(), 2);
        assert_eq!(grouped[0].sources[0].watch.position_ms, 0);
        assert_eq!(grouped[0].sources[1].watch.position_ms, 42_000);
        assert_ne!(edition_key(None), edition_key(Some("Theatrical")));
        assert_eq!(edition_key(Some(" Director's   Cut ")), "director's cut");
    }
    #[test]
    fn episodes_require_series_identity_and_exact_numbering() {
        let mut a = item("plex", "series", Some("12"), None);
        a.kind = MediaKind::Series;
        let mut b = item("jelly", "series", Some("12"), None);
        b.kind = MediaKind::Series;
        let mut episode_a = item("plex", "episode", None, None);
        episode_a.kind = MediaKind::Episode;
        episode_a.series = Some(a.id.clone());
        episode_a.season_number = Some(0);
        episode_a.episode_number = Some(1);
        let mut episode_b = episode_a.clone();
        episode_b.id = item("jelly", "episode", None, None).id;
        episode_b.series = Some(b.id.clone());
        let mut other = episode_b.clone();
        other.id.id = "other-episode".into();
        other.episode_number = Some(2);
        let grouped = unify([a, b, episode_a, episode_b, other]);
        assert_eq!(grouped.len(), 3);
        assert_eq!(grouped.iter().filter(|group| group.sources.len() == 2).count(), 2);
    }
}

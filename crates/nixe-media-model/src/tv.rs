//! Presentation metadata uses season/show art, never episode thumbnails.
use crate::*;
use std::collections::HashMap;
pub fn enrich(items: &mut [MediaItem]) {
    let context: HashMap<_, _> = items
        .iter()
        .filter(|i| matches!(i.kind, MediaKind::Series | MediaKind::Season))
        .map(|i| (i.id.clone(), i.clone()))
        .collect();
    for item in items {
        if !matches!(item.kind, MediaKind::Episode | MediaKind::Season) {
            continue;
        }
        let series = item.series.as_ref().and_then(|id| context.get(id));
        if let Some(series) = series {
            item.tv.series_title = Some(series.title.clone());
            item.tv.unwatched_episodes = series.metadata.unwatched_episodes;
            item.tv.series_poster = series.poster.clone().or(item.tv.series_poster.take());
        }
        if item.kind == MediaKind::Episode
            && let Some(season) = item.parent.as_ref().and_then(|id| context.get(id)).filter(|i| i.kind == MediaKind::Season) {
                item.tv.season_poster = season.poster.clone().or(item.tv.season_poster.take());
                item.tv.episode_count = season.tv.episode_count;
                item.tv.unwatched_episodes = item.tv.unwatched_episodes.or(season.metadata.unwatched_episodes);
            }
    }
}
pub fn posters(item: &MediaItem) -> Vec<String> {
    let keys = match item.kind {
        MediaKind::Episode => vec![item.tv.season_poster.clone(), item.tv.series_poster.clone()],
        MediaKind::Season => vec![item.poster.clone(), item.tv.series_poster.clone()],
        _ => vec![item.poster.clone()],
    };
    keys.into_iter().flatten().fold(vec![], |mut keys, key| {
        if !keys.contains(&key) {
            keys.push(key);
        }
        keys
    })
}
pub fn labels(item: &MediaItem) -> (String, String, String) {
    match item.kind {
        MediaKind::Episode => (
            item.tv.series_title.clone().unwrap_or_else(|| "TV show".into()),
            item.title.clone(),
            match (item.season_number, item.episode_number) {
                (Some(s), Some(e)) => format!("Season {s} · Episode {e}"),
                (Some(s), None) => format!("Season {s}"),
                _ => "Episode".into(),
            },
        ),
        MediaKind::Season => (
            item.tv.series_title.clone().unwrap_or_else(|| item.title.clone()),
            item.season_number.map(|s| format!("Season {s}")).unwrap_or_else(|| item.title.clone()),
            item.tv.episode_count.map(|n| format!("{n} episodes")).unwrap_or_default(),
        ),
        _ => (item.title.clone(), item.year.map(|y| y.to_string()).unwrap_or_default(), String::new()),
    }
}
pub fn recently_added(items: Vec<MediaItem>, seasons: &[MediaItem]) -> Vec<MediaItem> {
    let mut groups: HashMap<ItemId, Vec<MediaItem>> = HashMap::new();
    let mut recent = vec![];
    for item in items {
        if item.kind != MediaKind::Episode {
            continue;
        }
        if let Some(parent) = &item.parent {
            groups.entry(parent.clone()).or_default().push(item);
        } else {
            recent.push(item);
        }
    }
    for (parent, episodes) in groups {
        let season = seasons.iter().find(|s| s.id == parent && s.kind == MediaKind::Season);
        let stamps: Vec<_> = episodes.iter().filter_map(|i| i.added_at).collect();
        let numbers: std::collections::HashSet<_> = episodes.iter().filter_map(|i| i.episode_number).collect();
        let entire = season.is_some_and(|s| s.tv.episode_count == Some(episodes.len() as u32))
            && episodes.len() >= 2
            && numbers.len() == episodes.len()
            && stamps.len() == episodes.len()
            && stamps.iter().max().zip(stamps.iter().min()).is_some_and(|(a, b)| a.abs_diff(*b) <= 86400);
        if entire {
            let mut season = season.unwrap().clone();
            season.added_at = stamps.into_iter().max();
            recent.push(season);
        } else {
            recent.extend(episodes);
        }
    }
    recent.sort_by_key(|i| std::cmp::Reverse(i.added_at));
    recent.truncate(18);
    recent
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tv_cards_keep_context_and_group_only_known_complete_seasons() {
        let mut show = crate::index::tests::item("plex", "show", None, None);
        show.kind = MediaKind::Series;
        show.title = "Show".into();
        show.poster = Some("show-art".into());
        show.metadata.unwatched_episodes = Some(7);
        let mut season = show.clone();
        season.kind = MediaKind::Season;
        season.id.id = "season".into();
        season.series = Some(show.id.clone());
        season.poster = Some("season-art".into());
        season.tv.episode_count = Some(2);
        season.season_number = Some(1);
        let mut episode = season.clone();
        episode.kind = MediaKind::Episode;
        episode.id.id = "one".into();
        episode.parent = Some(season.id.clone());
        episode.title = "Pilot".into();
        episode.poster = Some("screenshot".into());
        episode.episode_number = Some(1);
        episode.added_at = Some(10);
        let mut two = episode.clone();
        two.id.id = "two".into();
        two.episode_number = Some(2);
        two.added_at = Some(20);
        let mut all = vec![show, season, episode, two];
        enrich(&mut all);
        assert_eq!(labels(&all[2]), ("Show".into(), "Pilot".into(), "Season 1 · Episode 1".into()));
        assert_eq!(posters(&all[2]), vec!["season-art", "show-art"]);
        assert_eq!(all[2].tv.unwatched_episodes, Some(7));
        all[0].metadata.unwatched_episodes = Some(6);
        enrich(&mut all);
        assert_eq!(all[2].tv.unwatched_episodes, Some(6));
        assert_eq!(recently_added(all[2..].to_vec(), &all).len(), 1);
        all[3].added_at = Some(100000);
        assert_eq!(recently_added(all[2..].to_vec(), &all).len(), 2);
        all[1].tv.episode_count = None;
        assert_eq!(recently_added(all[2..].to_vec(), &all).len(), 2);
    }
}

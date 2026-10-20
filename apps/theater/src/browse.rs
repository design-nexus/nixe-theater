//! Typed library queries shared by the native panel, saved views and worker index.
use nixe_media_model::*;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Filter {
    pub watch: Vec<String>,
    pub genres: Vec<String>,
    pub year_min: Option<u32>,
    pub year_max: Option<u32>,
    pub runtime_min: Option<u32>,
    pub runtime_max: Option<u32>,
    pub rating_source: String,
    pub rating_min: Option<u32>,
    pub votes_min: Option<u64>,
    pub certifications: Vec<String>,
    pub quality: Vec<String>,
    pub hdr: Vec<String>,
    pub audio: Vec<String>,
    pub subtitles: Vec<String>,
    pub editions: Vec<String>,
    pub availability: Vec<String>,
    pub status: Vec<String>,
    pub unwatched: bool,
    pub people: Vec<String>,
    pub studios: Vec<String>,
    pub collections: Vec<String>,
    pub playlists: Vec<String>,
    pub playlist_members: Vec<ItemId>,
}
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub filter: Filter,
    pub source: String,
    pub sort: String,
    pub descending: bool,
    pub shuffle: u64,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct SavedView {
    pub name: String,
    pub page: String,
    pub state: State,
}
pub fn sorts(tv: bool) -> Vec<(String, String)> {
    let mut v = crate::widgets::opts(&[
        ("title", "Title"),
        ("newest", "Date added"),
        ("release", "Release date"),
        ("year", "Year"),
        ("runtime", "Runtime"),
        ("watched", "Last watched"),
        ("plays", "Play count"),
        ("personal", "Your rating"),
        ("imdb", "IMDb rating"),
        ("tmdb", "TMDB rating"),
        ("rt_critic", "Rotten Tomatoes · critics"),
        ("rt_audience", "Rotten Tomatoes · audience"),
        ("metacritic", "Metacritic"),
        ("critic", "Server critic rating"),
        ("audience", "Server audience rating"),
        ("resolution", "Resolution"),
        ("shuffle", "Shuffle"),
    ]);
    if tv {
        v.extend(crate::widgets::opts(&[
            ("episode_added", "Latest episode added"),
            ("seasons", "Season count"),
            ("unwatched", "Unwatched episode count"),
        ]));
    }
    v
}
fn any(values: &[String], candidate: &str) -> bool {
    values.is_empty() || values.iter().any(|v| v.eq_ignore_ascii_case(candidate))
}
fn intersects(selected: &[String], values: &[String]) -> bool {
    selected.is_empty() || values.iter().any(|v| any(selected, v))
}
fn bound<T: Ord>(n: Option<T>, min: Option<T>, max: Option<T>) -> bool {
    if min.is_none() && max.is_none() {
        return true;
    }
    n.is_some_and(|n| min.is_none_or(|m| n >= m) && max.is_none_or(|m| n <= m))
}
pub fn rating<'a>(i: &'a MediaItem, source: &str) -> Option<&'a Rating> {
    i.metadata.ratings.iter().find(|r| match source {
        "critic" => matches!(r.source.as_str(), "plex_critic" | "jellyfin_critic"),
        "audience" => matches!(r.source.as_str(), "plex_audience" | "jellyfin"),
        _ => r.source == source,
    })
}
pub fn matches(i: &MediaItem, f: &Filter, online: &[ServerId]) -> bool {
    let m = &i.metadata;
    let watch = if i.watch.watched {
        "Watched"
    } else if i.watch.position_ms > 0 {
        "In progress"
    } else {
        "Unwatched"
    };
    if !any(&f.watch, watch)
        || !intersects(&f.genres, &m.genres)
        || !bound(i.year, f.year_min, f.year_max)
        || !bound(i.duration_ms.map(|n| (n / 60000) as u32), f.runtime_min, f.runtime_max)
        || !any(&f.certifications, m.certification.as_deref().unwrap_or(""))
        || !intersects(&f.studios, &m.studios)
        || !any(&f.availability, if online.contains(&i.id.server) { "Online" } else { "Offline" })
        || !any(&f.status, m.status.as_deref().unwrap_or(""))
        || (f.unwatched && !m.unwatched_episodes.is_some_and(|n| n > 0))
    {
        return false;
    }
    if !f.people.is_empty() && !m.credits.iter().any(|c| any(&f.people, &c.name)) {
        return false;
    }
    let mut collections = m.collections.clone();
    collections.extend(m.collection.clone());
    if !intersects(&f.collections, &collections) {
        return false;
    }
    if !f.playlists.is_empty() && !f.playlist_members.contains(&i.id) {
        return false;
    }
    if f.rating_min.is_some() || f.votes_min.is_some() {
        let source = if f.rating_source.is_empty() { "imdb" } else { &f.rating_source };
        if source == "personal" {
            if !bound(crate::metadata::personal(i), f.rating_min.map(|n| n * 10), None) || f.votes_min.is_some() {
                return false;
            }
        } else {
            let Some(r) = rating(i, source) else { return false };
            if !bound(Some(r.normalized()), f.rating_min, None) || !bound(r.votes, f.votes_min, None) {
                return false;
            }
        }
    }
    if f.quality.is_empty() && f.hdr.is_empty() && f.audio.is_empty() && f.subtitles.is_empty() && f.editions.is_empty() {
        return true;
    }
    i.versions.iter().any(|v| {
        let quality = v.height.map(|h| {
            if h >= 2000 {
                "4K"
            } else if h >= 1000 {
                "1080p"
            } else if h >= 700 {
                "720p"
            } else {
                "SD"
            }
        });
        let hdr = v.technical.hdr.as_deref().map(|s| {
            let s = s.to_lowercase();
            if s.contains("dovi") || s.contains("dolby") {
                "Dolby Vision"
            } else if s.contains("hdr") || s.contains("hlg") {
                "HDR"
            } else {
                "SDR"
            }
        });
        any(&f.quality, quality.unwrap_or(""))
            && any(&f.hdr, hdr.unwrap_or(""))
            && intersects(&f.audio, &v.technical.audio_languages)
            && intersects(&f.subtitles, &v.technical.subtitle_languages)
            && any(&f.editions, v.edition.as_deref().unwrap_or("Original"))
    })
}
fn optional<T: Ord>(a: Option<T>, b: Option<T>, descending: bool) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => {
            if descending {
                b.cmp(&a)
            } else {
                a.cmp(&b)
            }
        }
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        _ => Ordering::Equal,
    }
}
pub fn sort(items: &mut [MediaItem], s: &State) {
    use std::hash::{Hash, Hasher};
    let key = |i: &MediaItem| -> Option<i64> {
        match s.sort.as_str() {
            "newest" => i.added_at,
            "year" => i.year.map(|n| n as i64),
            "release" => i
                .metadata
                .release_date
                .as_ref()
                .and_then(|s| s.chars().filter(|c| c.is_ascii_digit()).take(8).collect::<String>().parse().ok())
                .or(i.year.map(|n| n as i64 * 10000)),
            "runtime" => i.duration_ms.map(|n| n as i64),
            "watched" => i.watch.updated_at,
            "plays" => i.metadata.play_count.map(|n| n as i64),
            "personal" => crate::metadata::personal(i).map(|n| n as i64),
            "resolution" => i.versions.iter().filter_map(|v| v.height).max().map(|n| n as i64),
            "episode_added" => i.metadata.latest_episode_added,
            "seasons" => i.metadata.season_count.map(|n| n as i64),
            "unwatched" => i.metadata.unwatched_episodes.map(|n| n as i64),
            "shuffle" => {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                i.id.hash(&mut h);
                s.shuffle.hash(&mut h);
                Some((h.finish() >> 1) as i64)
            }
            source => rating(i, source).map(|r| r.normalized() as i64),
        }
    };
    items.sort_by_cached_key(|i| (i.title.to_lowercase(), serde_json::to_string(&i.id).unwrap_or_default()));
    if s.sort.is_empty() || s.sort == "title" {
        if s.descending {
            items.reverse();
        }
        return;
    }
    items.sort_by(|a, b| {
        if s.sort.is_empty() || s.sort == "title" {
            let cmp = a.title.to_lowercase().cmp(&b.title.to_lowercase());
            if s.descending { cmp.reverse() } else { cmp }
        } else {
            optional(key(a), key(b), s.descending)
        }
    });
}
pub fn aggregate_tv(items: &mut [MediaItem]) {
    use std::collections::{HashMap, HashSet};
    let mut counts: HashMap<ItemId, (HashSet<u32>, u32, u32, Option<i64>)> = HashMap::new();
    let mut versions: HashMap<ItemId, Vec<MediaVersion>> = HashMap::new();
    for i in items.iter().filter(|i| i.kind == MediaKind::Episode) {
        for parent in i.series.iter().chain(i.parent.iter().filter(|id| Some(*id) != i.series.as_ref())) {
            let list = versions.entry(parent.clone()).or_default();
            for v in &i.versions {
                if !list.iter().any(|a| a.edition == v.edition && a.height == v.height && a.technical == v.technical) {
                    list.push(v.clone());
                }
            }
            let c = counts.entry(parent.clone()).or_default();
            if let Some(n) = i.season_number {
                c.0.insert(n);
            }
            c.1 += 1;
            c.2 += u32::from(!i.watch.watched);
            c.3 = std::cmp::max(c.3, i.added_at);
        }
    }
    for i in items.iter_mut().filter(|i| matches!(i.kind, MediaKind::Series | MediaKind::Season)) {
        if let Some((seasons, total, unwatched, latest)) = counts.get(&i.id) {
            if i.kind == MediaKind::Series {
                i.metadata.season_count = i.metadata.season_count.or(Some(seasons.len() as u32));
            }
            i.metadata.episode_count = i.metadata.episode_count.or(Some(*total));
            if i.metadata.episode_count.is_none_or(|known| *total >= known) || i.metadata.unwatched_episodes.is_none() {
                i.metadata.unwatched_episodes = Some(*unwatched);
            }
            if i.versions.is_empty() {
                i.versions = versions.remove(&i.id).unwrap_or_default();
            }
            i.metadata.latest_episode_added = std::cmp::max(i.metadata.latest_episode_added, *latest);
        }
    }
}
pub fn chips(f: &Filter) -> Vec<(&'static str, String)> {
    let mut v = vec![];
    macro_rules! list{($($field:ident=>$label:literal),*)=>{$(if !f.$field.is_empty(){v.push((stringify!($field),format!("{}: {}",$label,f.$field.join(", "))));})*}}
    list!(watch=>"Watch",genres=>"Genre",certifications=>"Certification",quality=>"Quality",hdr=>"HDR",audio=>"Audio",subtitles=>"Subtitles",editions=>"Edition",availability=>"Availability",status=>"Status",people=>"People",studios=>"Studio",collections=>"Collection",playlists=>"Playlist");
    for (key, label, min, max) in [("year", "Year", f.year_min, f.year_max), ("runtime", "Minutes", f.runtime_min, f.runtime_max)]
    {
        if min.is_some() || max.is_some() {
            v.push((
                key,
                format!(
                    "{label}: {}–{}",
                    min.map(|v| v.to_string()).unwrap_or_default(),
                    max.map(|v| v.to_string()).unwrap_or_default()
                ),
            ));
        }
    }
    if let Some(n) = f.rating_min {
        v.push((
            "rating",
            format!("{} ≥ {:.1}", if f.rating_source.is_empty() { "imdb" } else { &f.rating_source }, n as f64 / 10.0),
        ));
    }
    if let Some(n) = f.votes_min {
        v.push(("votes", format!("Votes ≥ {n}")));
    }
    if f.unwatched {
        v.push(("unwatched", "Has unwatched episodes".into()));
    }
    v
}
pub fn clear(f: &mut Filter, key: &str) {
    macro_rules! list{($($field:ident),*)=>{match key{$(stringify!($field)=>f.$field.clear(),)*_=>{}}}}
    list!(
        watch,
        genres,
        certifications,
        quality,
        hdr,
        audio,
        subtitles,
        editions,
        availability,
        status,
        people,
        studios,
        collections,
        playlists
    );
    match key {
        "year" => {
            f.year_min = None;
            f.year_max = None
        }
        "runtime" => {
            f.runtime_min = None;
            f.runtime_max = None
        }
        "rating" => f.rating_min = None,
        "votes" => f.votes_min = None,
        "unwatched" => f.unwatched = false,
        _ => {}
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absent_sort_values_stay_last_both_directions() {
        assert_eq!(optional(None, Some(5), false), Ordering::Greater);
        assert_eq!(optional(None, Some(5), true), Ordering::Greater);
    }
    #[test]
    fn clearing_ranges_preserves_other_filters() {
        let mut f = Filter { year_min: Some(2000), genres: vec!["Drama".into()], ..Default::default() };
        clear(&mut f, "year");
        assert!(f.year_min.is_none());
        assert_eq!(f.genres, ["Drama"]);
    }
}

#[cfg(test)]
mod regression {
    use super::*;
    fn movie() -> MediaItem {
        nixe_plex::parse_item(&serde_json::json!({"type":"movie","ratingKey":"1","title":"Film","year":2020,"Genre":[{"tag":"Drama"}],"Media":[
            {"id":"one","height":2160,"Part":[{"key":"/library/parts/1/file","Stream":[{"streamType":1,"colorTrc":"bt709"},{"streamType":2,"languageCode":"eng"}]}]},
            {"id":"two","height":1080,"Part":[{"key":"/library/parts/2/file","Stream":[{"streamType":1,"colorTrc":"smpte2084"},{"streamType":2,"languageCode":"fra"}]}]}
        ]}),&ServerId{provider:ProviderKind::Plex,id:"fixture".into()},"1").unwrap()
    }
    #[test]
    fn technical_facets_must_match_one_real_version() {
        let item = movie();
        let mut f = Filter { quality: vec!["4K".into()], hdr: vec!["HDR".into()], ..Default::default() };
        assert!(!matches(&item, &f, &[]));
        f.hdr = vec!["SDR".into()];
        assert!(matches(&item, &f, &[]));
        f.audio = vec!["fra".into()];
        assert!(!matches(&item, &f, &[]));
    }
    #[test]
    fn rating_thresholds_require_the_selected_source_and_votes() {
        let mut item = movie();
        item.metadata.ratings =
            vec![Rating { source: "imdb".into(), value: 780, scale: 1000, votes: Some(12000), ..Default::default() }];
        let mut f = Filter { rating_source: "imdb".into(), rating_min: Some(70), votes_min: Some(10000), ..Default::default() };
        assert!(matches(&item, &f, &[]));
        f.rating_source = "rt_critic".into();
        assert!(!matches(&item, &f, &[]));
        f.rating_source = "imdb".into();
        item.metadata.ratings[0].votes = None;
        assert!(!matches(&item, &f, &[]));
    }
    #[test]
    fn old_snapshot_records_deserialize_and_saved_queries_round_trip() {
        let mut old = serde_json::to_value(movie()).unwrap();
        old.as_object_mut().unwrap().remove("metadata");
        for v in old["versions"].as_array_mut().unwrap() {
            v.as_object_mut().unwrap().remove("technical");
        }
        let restored: MediaItem = serde_json::from_value(old).unwrap();
        assert!(restored.metadata.ratings.is_empty());
        assert!(restored.versions[0].technical.audio_languages.is_empty());
        let state = State {
            filter: Filter { genres: vec!["Drama".into()], quality: vec!["4K".into()], ..Default::default() },
            sort: "imdb".into(),
            descending: true,
            ..Default::default()
        };
        assert!(serde_json::from_str::<State>(&serde_json::to_string(&state).unwrap()).unwrap() == state);
    }
}

#[cfg(test)]
mod episode_count_tests {
    use super::*;
    #[test]
    fn updates_show_and_season_counts_with_source_scoped_episodes() {
        let server = ServerId { provider: ProviderKind::Plex, id: "fixture".into() };
        let mut show = nixe_plex::parse_item(
            &serde_json::json!({"type":"show","ratingKey":"1","title":"Show","leafCount":2,"viewedLeafCount":0}),
            &server,
            "1",
        )
        .unwrap();
        let mut season = show.clone();
        season.id.id = "2".into();
        season.kind = MediaKind::Season;
        let mut episode = show.clone();
        episode.kind = MediaKind::Episode;
        episode.id.id = "3".into();
        episode.series = Some(show.id.clone());
        episode.parent = Some(season.id.clone());
        let mut watched = episode.clone();
        watched.id.id = "4".into();
        watched.watch.watched = true;
        let mut items = vec![show.clone(), season, episode, watched];
        aggregate_tv(&mut items);
        assert_eq!(items[0].metadata.unwatched_episodes, Some(1));
        assert_eq!(items[1].metadata.unwatched_episodes, Some(1));
        items[2].watch.watched = true;
        aggregate_tv(&mut items);
        assert_eq!(items[0].metadata.unwatched_episodes, Some(0));
        assert_eq!(items[1].metadata.unwatched_episodes, Some(0));
        show.metadata.episode_count = Some(20);
        show.metadata.unwatched_episodes = Some(17);
        items[0] = show;
        aggregate_tv(&mut items);
        assert_eq!(items[0].metadata.unwatched_episodes, Some(17));
    }
}

#[cfg(test)]
mod tv_rating_sort_tests {
    use super::*;
    #[test]
    fn imdb_series_sort_is_source_specific_and_keeps_missing_last() {
        let server = ServerId { provider: ProviderKind::Plex, id: "fixture".into() };
        let make = |id: &str, title: &str, score: Option<f64>| {
            let mut v = serde_json::json!({"type":"show","ratingKey":id,"title":title,"rating":9.9,"ratingImage":"themoviedb://image.rating"});
            if let Some(score) = score {
                v["Rating"] = serde_json::json!([{"image":"imdb://image.rating","value":score}]);
            }
            nixe_plex::parse_item(&v, &server, "1").unwrap()
        };
        let mut shows = vec![make("1", "A", Some(7.2)), make("2", "Z", Some(9.1)), make("3", "Missing", None)];
        let mut state = State { sort: "imdb".into(), descending: true, ..Default::default() };
        sort(&mut shows, &state);
        assert_eq!(shows.iter().map(|i| i.id.id.as_str()).collect::<Vec<_>>(), ["2", "1", "3"]);
        state.descending = false;
        sort(&mut shows, &state);
        assert_eq!(shows.iter().map(|i| i.id.id.as_str()).collect::<Vec<_>>(), ["1", "2", "3"]);
    }
}

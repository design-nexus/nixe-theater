//! Worker-owned view index. GTK submits only the newest request and renders one page.
use nixe_media_model::{
    index::{UnifiedItem, unify},
    *,
};
use std::{
    collections::HashMap,
    sync::{Arc, Condvar, Mutex},
};

#[derive(Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Home {
    pub featured: Option<MediaItem>,
    pub continuing: Vec<MediaItem>,
    pub movies: Vec<MediaItem>,
    pub tv: Vec<MediaItem>,
    #[serde(default)]
    pub discovery: Vec<Vec<MediaItem>>,
}
pub const DISCOVERY_CATEGORIES: &[&str] = &[
    "Highly rated movies",
    "Highly rated shows",
    "Under 90 minutes",
    "Unwatched in 4K",
    "Recently released",
    "Completed shows",
    "Comedy night",
    "Action and adventure",
    "Documentaries",
    "Family night",
];
fn discovery(items: &[MediaItem], year: i64, seed: u64) -> Vec<Vec<MediaItem>> {
    use std::hash::{Hash, Hasher};
    let score = |i: &MediaItem| {
        i.metadata.ratings.iter().filter(|r| r.votes.is_none_or(|v| v >= 100)).map(|r| r.normalized()).max().unwrap_or(0)
    };
    let genre = |i: &MediaItem, names: &[&str]| i.metadata.genres.iter().any(|g| names.iter().any(|n| g.eq_ignore_ascii_case(n)));
    (0..DISCOVERY_CATEGORIES.len())
        .map(|category| {
            let mut titles: Vec<_> = items
                .iter()
                .filter(|i| matches!(i.kind, MediaKind::Movie | MediaKind::Series))
                .filter(|i| match category {
                    0 => i.kind == MediaKind::Movie && score(i) >= 75,
                    1 => i.kind == MediaKind::Series && score(i) >= 75,
                    2 => i.kind == MediaKind::Movie && i.duration_ms.is_some_and(|d| d > 0 && d < 90 * 60000),
                    3 => {
                        i.kind == MediaKind::Movie
                            && !i.watch.watched
                            && i.watch.position_ms == 0
                            && i.versions.iter().any(|v| v.height.is_some_and(|h| h >= 2000))
                    }
                    4 => i.year.is_some_and(|y| y as i64 >= year - 2 && y as i64 <= year),
                    5 => {
                        i.kind == MediaKind::Series
                            && i.metadata
                                .status
                                .as_deref()
                                .is_some_and(|s| s.eq_ignore_ascii_case("ended") || s.eq_ignore_ascii_case("canceled"))
                    }
                    6 => genre(i, &["Comedy"]),
                    7 => genre(i, &["Action", "Adventure", "Action & Adventure"]),
                    8 => genre(i, &["Documentary", "Documentaries"]),
                    _ => genre(i, &["Family", "Kids"]),
                })
                .cloned()
                .collect();
            titles.sort_by_cached_key(|i| {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                seed.hash(&mut h);
                i.id.hash(&mut h);
                h.finish()
            });
            if category < 2 {
                titles.sort_by_key(|i| std::cmp::Reverse(score(i)));
            }
            if category == 4 {
                titles.sort_by_key(|i| std::cmp::Reverse(i.year));
            }
            titles.truncate(18);
            titles
        })
        .collect()
}
pub struct Request {
    pub generation: u64,
    pub items: Option<Vec<MediaItem>>,
    pub page: String,
    pub filter: String,
    pub query: String,
    pub online: Vec<ServerId>,
    pub cache_scope: Option<String>,
    pub loading: bool,
    pub browse: crate::browse::State,
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct View {
    pub generation: u64,
    pub page: String,
    pub items: Vec<MediaItem>,
    pub home: Option<Home>,
}
impl View {
    pub fn is_current(&self, generation: u64, page: &str) -> bool {
        self.generation == generation && self.page == page
    }
}
#[derive(Default)]
struct Mailbox {
    request: Option<Request>,
    stopped: bool,
}
#[derive(Clone)]
pub struct Worker(Arc<(Mutex<Mailbox>, Condvar)>);
impl Worker {
    pub fn start() -> (Self, async_channel::Receiver<View>) {
        let worker = Self(Arc::new((Mutex::new(Mailbox::default()), Condvar::new())));
        let (tx, rx) = async_channel::unbounded();
        let mailbox = worker.0.clone();
        std::thread::spawn(move || {
            let mut index = Index::default();
            let mut restored = std::collections::HashSet::new();
            loop {
                let request = {
                    let (lock, wake) = &*mailbox;
                    let mut state = lock.lock().unwrap();
                    while state.request.is_none() && !state.stopped {
                        state = wake.wait(state).unwrap();
                    }
                    if state.stopped {
                        break;
                    }
                    state.request.take().unwrap()
                };
                let scope = request.cache_scope.clone();
                let loading = request.loading;
                let root = crate::view_cache::root();
                if let Some(key) = scope.as_ref().filter(|key| restored.insert((*key).clone()))
                    && let Some(cached) = crate::view_cache::load(&root, key, request.generation) {
                        let _ = tx.try_send(cached);
                    }
                let view = index.prepare(request);
                let empty = view.items.is_empty()
                    && view.home.as_ref().is_none_or(|h| {
                        h.featured.is_none()
                            && h.continuing.is_empty()
                            && h.movies.is_empty()
                            && h.tv.is_empty()
                            && h.discovery.iter().all(Vec::is_empty)
                    });
                if loading && empty {
                    continue;
                }
                if let Some(key) = &scope {
                    crate::view_cache::save(&root, key, &view);
                }
                let state = mailbox.0.lock().unwrap();
                if !state.stopped && state.request.is_none() && tx.try_send(view).is_err() {
                    break;
                }
            }
        });
        (worker, rx)
    }
    pub fn submit(&self, mut request: Request) {
        let mut state = self.0.0.lock().unwrap();
        if state.stopped {
            return;
        }
        if request.items.is_none() {
            request.items = state.request.as_mut().and_then(|old| old.items.take());
        }
        state.request = Some(request);
        self.0.1.notify_one();
    }
    pub fn cancel(&self) {
        let mut state = self.0.0.lock().unwrap();
        state.stopped = true;
        state.request = None;
        self.0.1.notify_one();
    }
}
/// Immutable worker-built catalog: detail clicks clone an Arc, never the whole library.
#[derive(Default)]
pub struct Snapshot {
    groups: Vec<UnifiedItem>,
    by_id: HashMap<ItemId, usize>,
    episodes: HashMap<ItemId, Vec<(usize, usize)>>,
}
impl std::ops::Deref for Snapshot {
    type Target = [UnifiedItem];
    fn deref(&self) -> &Self::Target {
        &self.groups
    }
}
impl Snapshot {
    fn new(groups: Vec<UnifiedItem>) -> Self {
        let mut s = Self { groups, ..Default::default() };
        for (g, group) in s.groups.iter().enumerate() {
            for (n, item) in group.sources.iter().enumerate() {
                s.by_id.insert(item.id.clone(), g);
                if item.kind == MediaKind::Episode {
                    for parent in [item.series.as_ref(), item.parent.as_ref()].into_iter().flatten() {
                        s.episodes.entry(parent.clone()).or_default().push((g, n));
                    }
                }
            }
        }
        s
    }
    pub fn items(&self) -> impl Iterator<Item = &MediaItem> {
        self.groups.iter().flat_map(|g| g.sources.iter())
    }
    pub fn sources(&self, item: &MediaItem) -> Vec<MediaItem> {
        let mut sources = self.by_id.get(&item.id).map(|&g| self.groups[g].sources.clone()).unwrap_or_default();
        if let Some(existing) = sources.iter_mut().find(|i| i.id == item.id) {
            *existing = item.clone();
        } else {
            sources.push(item.clone());
        }
        sources
    }
    pub fn episodes(&self, id: &ItemId) -> Vec<MediaItem> {
        self.episodes.get(id).into_iter().flatten().map(|&(g, n)| self.groups[g].sources[n].clone()).collect()
    }
}
static SNAPSHOT: std::sync::OnceLock<std::sync::RwLock<Arc<Snapshot>>> = std::sync::OnceLock::new();
pub fn snapshot() -> Arc<Snapshot> {
    SNAPSHOT.get_or_init(Default::default).read().unwrap().clone()
}
fn publish(snapshot: Arc<Snapshot>) {
    *SNAPSHOT.get_or_init(Default::default).write().unwrap() = snapshot;
}
#[derive(Default)]
struct Index {
    groups: Arc<Snapshot>,
    series: HashMap<ItemId, String>,
}
impl Index {
    fn prepare(&mut self, request: Request) -> View {
        if let Some(mut items) = request.items {
            crate::metadata::submit(&items, &request.page);
            for item in &mut items {
                crate::metadata::apply(item);
            }
            crate::browse::aggregate_tv(&mut items);
            nixe_media_model::tv::enrich(&mut items);
            self.series =
                items.iter().filter(|i| i.kind == MediaKind::Series).map(|i| (i.id.clone(), i.title.to_lowercase())).collect();
            self.groups = Arc::new(Snapshot::new(unify(items)));
            publish(self.groups.clone());
        }
        let select = |group: &UnifiedItem, kind: Option<MediaKind>, resume: bool| {
            group
                .sources
                .iter()
                .enumerate()
                .filter(|(_, item)| {
                    kind.is_none_or(|kind| item.kind == kind)
                        && (!resume
                            || (!item.watch.watched
                                && item.watch.position_ms > 0
                                && matches!(item.kind, MediaKind::Movie | MediaKind::Episode)))
                        && crate::servers::matches_filter(item, &request.filter)
                        && crate::browse::matches(item, &request.browse.filter, &request.online)
                })
                .min_by_key(|(index, item)| {
                    (
                        !request.online.contains(&item.id.server),
                        std::cmp::Reverse(!item.watch.watched && item.watch.position_ms > 0),
                        std::cmp::Reverse(item.watch.updated_at),
                        *index,
                    )
                })
                .map(|(_, item)| item.clone())
        };
        let mut view = View { generation: request.generation, page: request.page.clone(), items: vec![], home: None };
        if matches!(request.page.as_str(), "home" | "discover") {
            let mut home = Home {
                continuing: self.groups.iter().filter_map(|group| select(group, None, true)).collect(),
                ..Default::default()
            };
            home.continuing.sort_by_key(|i| std::cmp::Reverse(i.watch.updated_at));
            home.continuing.truncate(12);
            for (kind, items) in [(MediaKind::Movie, &mut home.movies), (MediaKind::Series, &mut home.tv)] {
                *items = self.groups.iter().filter_map(|group| select(group, Some(kind), false)).collect();
                items.sort_by_key(|i| std::cmp::Reverse(i.added_at));
                items.truncate(18);
            }
            let selected: Vec<_> = self.groups.iter().filter_map(|group| select(group, None, false)).collect();
            let episodes: Vec<_> = selected.iter().filter(|i| i.kind == MediaKind::Episode).cloned().collect();
            if !episodes.is_empty() {
                home.tv = nixe_media_model::tv::recently_added(episodes, &selected);
            }
            if request.page == "discover" {
                let day =
                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() / 86400;
                let daily = |i: &MediaItem| {
                    use std::hash::{Hash, Hasher};
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    i.id.hash(&mut h);
                    day.hash(&mut h);
                    h.finish()
                };
                home.continuing = selected
                    .iter()
                    .filter(|i| i.kind == MediaKind::Movie && !i.watch.watched && i.watch.position_ms == 0)
                    .cloned()
                    .collect();
                home.continuing.sort_by_key(daily);
                home.continuing.truncate(18);
                let started: std::collections::HashSet<_> = selected
                    .iter()
                    .filter(|i| i.kind == MediaKind::Episode && (i.watch.watched || i.watch.position_ms > 0))
                    .filter_map(|i| i.series.clone())
                    .collect();
                home.movies = selected
                    .iter()
                    .filter(|i| i.kind == MediaKind::Series && !i.watch.watched && !started.contains(&i.id))
                    .cloned()
                    .collect();
                home.movies.sort_by_key(daily);
                home.movies.truncate(18);
                // Gregorian average year avoids a new date dependency for an approximate decade filter.
                let year = 1970 + (day as i64 / 365);
                home.discovery = discovery(&selected, year, day);
                home.tv = selected
                    .into_iter()
                    .filter(|i| i.kind == MediaKind::Movie && i.year.is_some_and(|y| y as i64 <= year - 10))
                    .collect();
                home.tv.sort_by_key(daily);
                home.tv.truncate(18);
            }
            home.featured = if request.page == "discover" {
                None
            } else {
                home.continuing.first().or(home.movies.first()).or(home.tv.first()).cloned()
            };
            view.home = Some(home);
        } else {
            let kind = match request.page.as_str() {
                "movies" => Some(MediaKind::Movie),
                "tv" => Some(MediaKind::Series),
                "collections" => Some(MediaKind::Collection),
                _ => None,
            };
            view.items = self
                .groups
                .iter()
                .filter_map(|group| {
                    if request.page == "search" {
                        if request.query.is_empty() {
                            return None;
                        }
                        let sources = group
                            .sources
                            .iter()
                            .filter(|i| {
                                i.title.to_lowercase().contains(&request.query)
                                    || i.series
                                        .as_ref()
                                        .and_then(|id| self.series.get(id))
                                        .is_some_and(|s| s.contains(&request.query))
                            })
                            .cloned()
                            .collect();
                        select(&UnifiedItem { sources }, None, false)
                    } else {
                        select(group, kind, false)
                    }
                })
                .collect();
            crate::browse::sort(&mut view.items, &request.browse);
        }
        view
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(generation: u64) -> Request {
        Request {
            generation,
            items: None,
            page: "movies".into(),
            filter: String::new(),
            query: String::new(),
            online: vec![],
            cache_scope: None,
            loading: false,
            browse: Default::default(),
        }
    }
    #[test]
    fn latest_request_preserves_pending_catalog_and_cancellation() {
        let mailbox = Worker(Arc::new((Mutex::new(Mailbox::default()), Condvar::new())));
        let mut first = request(1);
        first.items = Some(vec![]);
        mailbox.submit(first);
        mailbox.submit(request(2));
        let state = mailbox.0.0.lock().unwrap();
        assert_eq!(state.request.as_ref().unwrap().generation, 2);
        assert!(state.request.as_ref().unwrap().items.is_some());
        drop(state);
        mailbox.cancel();
        mailbox.submit(request(3));
        assert!(mailbox.0.0.lock().unwrap().request.is_none());
        let view = View { generation: 2, page: "movies".into(), items: vec![], home: None };
        assert!(view.is_current(2, "movies"));
        assert!(!view.is_current(3, "movies"));
        assert!(!view.is_current(2, "home"));
    }
    #[test]
    fn prepares_large_catalog_without_building_hidden_pages() {
        let item = MediaItem {
            id: ItemId { server: ServerId { provider: ProviderKind::Plex, id: "fixture".into() }, id: "0".into() },
            library_id: "1".into(),
            kind: MediaKind::Movie,
            title: "Title".into(),
            tv: Default::default(),
            metadata: Default::default(),
            summary: String::new(),
            year: Some(2026),
            external_ids: ExternalIds::default(),
            parent: None,
            series: None,
            season_number: None,
            episode_number: None,
            duration_ms: Some(600000),
            added_at: Some(1),
            poster: None,
            backdrop: None,
            versions: vec![],
            watch: WatchState::default(),
        };
        for size in [3600, 20000] {
            let mut input = request(1);
            input.items = Some(
                (0..size)
                    .map(|n| {
                        let mut i = item.clone();
                        i.id.id = n.to_string();
                        i
                    })
                    .collect(),
            );
            input.page = "home".into();
            let now = std::time::Instant::now();
            let mut index = Index::default();
            let home = index.prepare(input);
            assert!(home.items.is_empty());
            assert_eq!(home.home.unwrap().movies.len(), 18);
            let mut query = request(2);
            query.page = "search".into();
            query.query = "absent".into();
            assert!(index.prepare(query).items.is_empty());
            assert_eq!(index.prepare(request(3)).items.len(), size);
            eprintln!("Prepared {size} titles: {:?}", now.elapsed());
        }
    }
}

#[cfg(test)]
mod cached_check {
    use super::*;
    #[test]
    fn prepares_optional_read_only_cache_snapshot() {
        let Some(path) = std::env::var_os("THEATER_CATALOG_FIXTURE") else { return };
        let items: Vec<MediaItem> = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let size = items.len();
        let now = std::time::Instant::now();
        let mut index = Index::default();
        let view = index.prepare(Request {
            generation: 1,
            items: Some(items),
            page: "home".into(),
            filter: String::new(),
            query: String::new(),
            online: vec![],
            cache_scope: None,
            loading: false,
            browse: Default::default(),
        });
        assert!(view.items.is_empty());
        assert!(view.home.unwrap().featured.is_some());
        eprintln!("Prepared cached Home from {size} records in {:?}", now.elapsed());
    }
}

#[cfg(test)]
mod detail_index_tests {
    use super::*;
    #[test]
    fn detail_lookup_keeps_full_selected_version_and_avoids_catalog_rebuilds() {
        let server = ServerId { provider: ProviderKind::Plex, id: "fixture".into() };
        let base =
            nixe_plex::parse_item(&serde_json::json!({"type":"movie","ratingKey":"1","title":"Title"}), &server, "1").unwrap();
        let items = (0..20000)
            .map(|n| {
                let mut i = base.clone();
                i.id.id = n.to_string();
                i
            })
            .collect::<Vec<_>>();
        let snapshot = Snapshot::new(unify(items));
        let mut selected = base;
        selected.summary = "Fresh details".into();
        let started = std::time::Instant::now();
        for _ in 0..1000 {
            let sources = snapshot.sources(&selected);
            assert_eq!(sources.len(), 1);
            assert_eq!(sources[0].summary, "Fresh details");
        }
        eprintln!("1000 indexed detail-source lookups in a 20,000-item library: {:?}", started.elapsed());
    }
}

#[cfg(test)]
mod discovery_tests {
    use super::*;
    #[test]
    fn discovery_uses_root_titles_and_honors_watch_quality_and_genres() {
        let server = ServerId { provider: ProviderKind::Plex, id: "test".into() };
        let mut movie=nixe_plex::parse_item(&serde_json::json!({"type":"movie","ratingKey":"1","title":"Short comedy","duration":80*60000,"Genre":[{"tag":"Comedy"}],"Media":[{"height":2160,"Part":[]}]}),&server,"1").unwrap();
        let mut episode = movie.clone();
        episode.kind = MediaKind::Episode;
        episode.id.id = "2".into();
        let rows = discovery(&[movie.clone(), episode], 2026, 1);
        assert_eq!(rows[2].len(), 1);
        assert_eq!(rows[3].len(), 1);
        assert_eq!(rows[6].len(), 1);
        assert!(rows.iter().flatten().all(|i| i.kind != MediaKind::Episode));
        movie.watch.watched = true;
        assert!(discovery(&[movie], 2026, 1)[3].is_empty());
    }
}

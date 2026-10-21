//! Shared provider routing; source identity prevents cross-server credential use.
use crate::{jellyfin, plex, prefs};
use nixe_media_model::*;
use std::collections::HashMap;
#[derive(Clone)]
pub enum Client {
    Plex(nixe_plex::Client),
    Jellyfin(nixe_jellyfin::Client),
}
impl Client {
    pub fn playlists(&self, cursor: Option<&str>) -> Result<Page<MediaPlaylist>, ProviderError> {
        match self {
            Self::Plex(c) => c.playlists(cursor),
            Self::Jellyfin(c) => c.playlists(cursor),
        }
    }
    pub fn playlist_items(&self, playlist: &ItemId, cursor: Option<&str>) -> Result<Page<MediaItem>, ProviderError> {
        match self {
            Self::Plex(c) => c.playlist_items(playlist, cursor),
            Self::Jellyfin(c) => c.playlist_items(playlist, cursor),
        }
    }

    pub fn discard_session(self, session: String, mode: PlaybackMode) {
        std::thread::spawn(move || {
            if mode == PlaybackMode::Transcode {
                let _ = self.stop_transcode(&session);
            }
            if let Self::Jellyfin(client) = self {
                client.forget_session(&session);
            }
        });
    }
    pub fn label(&self) -> &'static str {
        match self {
            Self::Plex(_) => "Plex",
            Self::Jellyfin(_) => "Jellyfin",
        }
    }
    pub fn server_id(&self) -> &ServerId {
        match self {
            Self::Plex(c) => c.server_id(),
            Self::Jellyfin(c) => c.server_id(),
        }
    }
    pub fn detail(&self, item: &ItemId) -> Result<MediaItem, ProviderError> {
        match self {
            Self::Plex(c) => c.detail(item),
            Self::Jellyfin(c) => c.detail(item),
        }
    }
    pub fn child_page(&self, item: &ItemId, library: &str, cursor: Option<&str>) -> Result<Page<MediaItem>, ProviderError> {
        match self {
            Self::Plex(c) => c.child_page(item, library, cursor),
            Self::Jellyfin(c) => c.child_page(item, library, cursor),
        }
    }
    pub fn resolve(&self, request: PlaybackRequest) -> Result<PlaybackSource, ProviderError> {
        match self {
            Self::Plex(c) => c.resolve(request),
            Self::Jellyfin(c) => c.resolve(request),
        }
    }
    pub fn timeline(&self, progress: PlaybackProgress) -> Result<(), ProviderError> {
        match self {
            Self::Plex(c) => c.timeline(progress),
            Self::Jellyfin(c) => c.timeline(progress),
        }
    }
    pub fn stop_transcode(&self, session: &str) -> Result<(), ProviderError> {
        match self {
            Self::Plex(c) => c.stop_transcode(session),
            Self::Jellyfin(c) => c.stop_transcode(session),
        }
    }
    pub fn apply_watch(&self, item: &ItemId, state: &WatchState) -> Result<MediaItem, ProviderError> {
        match self {
            Self::Plex(client) => client.apply_watch(item, state),
            Self::Jellyfin(client) => client.apply_watch(item, state),
        }
    }
    pub fn subtitle(&self, url: &str) -> Result<String, ProviderError> {
        match self {
            Self::Plex(_) => Err(ProviderError::Unsupported),
            Self::Jellyfin(client) => client.subtitle(url),
        }
    }
    pub fn artwork(&self, key: &str, width: u32, height: u32) -> Result<Vec<u8>, ProviderError> {
        match self {
            Self::Plex(c) => c.artwork(key, width, height),
            Self::Jellyfin(c) => c.artwork(key, width, height),
        }
    }
}
pub fn client(server: &ServerId) -> Option<Client> {
    match server.provider {
        ProviderKind::Plex => plex::client().filter(|c| c.server_id() == server).map(Client::Plex),
        ProviderKind::Jellyfin => jellyfin::client().filter(|c| c.server_id() == server).map(Client::Jellyfin),
    }
}
pub fn configured(server: &ServerId) -> bool {
    let p = prefs::get();
    p.plex.is_some_and(|p| p.server == *server) || p.jellyfin.is_some_and(|p| p.server == *server)
}
pub fn any_configured() -> bool {
    let p = prefs::get();
    p.plex.is_some() || p.jellyfin.is_some()
}
pub fn items() -> Vec<MediaItem> {
    plex::items().into_iter().chain(jellyfin::items()).collect()
}
pub fn latest_watch(item: MediaItem) -> MediaItem {
    match item.id.server.provider {
        ProviderKind::Plex => plex::latest_watch(item),
        ProviderKind::Jellyfin => jellyfin::latest_watch(item),
    }
}
pub fn details_watch(item: &MediaItem, children: &[MediaItem], started: i64) {
    match item.id.server.provider {
        ProviderKind::Plex => plex::details_watch(item, children, started),
        ProviderKind::Jellyfin => jellyfin::details_watch(item, children, started),
    }
}
pub fn playback_watch(item: MediaItem, revision: i64) {
    match item.id.server.provider {
        ProviderKind::Plex => plex::playback_watch(item, revision),
        ProviderKind::Jellyfin => jellyfin::playback_watch(item, revision),
    }
}
pub fn refresh() {
    plex::refresh();
    jellyfin::refresh()
}
pub fn start() {
    plex::start();
    jellyfin::start()
}
pub fn cancel() {
    crate::reconcile::cancel();
    plex::cancel();
    jellyfin::cancel()
}
pub use plex::{database, revision};
/// The most titles of one kind a library page loop takes before giving up on that
/// library (a safety stop against a server that pages forever).
pub const MAX_TITLES: usize = 500_000;
pub fn library_key(server: &ServerId, library: &str) -> String {
    serde_json::to_string(&(server, library)).unwrap_or_default()
}
pub fn provider_key(provider: ProviderKind) -> &'static str {
    match provider {
        ProviderKind::Plex => "provider:plex",
        ProviderKind::Jellyfin => "provider:jellyfin",
    }
}
pub fn filters() -> Vec<(String, String)> {
    let mut filters = vec![(String::new(), "All libraries".into())];
    let p = prefs::get();
    for (server, libraries, label) in
        p.plex.map(|p| (p.server, p.libraries, "Plex")).into_iter().chain(p.jellyfin.map(|p| (p.server, p.libraries, "Jellyfin")))
    {
        filters.push((provider_key(server.provider).into(), label.into()));
        filters.extend(libraries.into_iter().map(|l| (library_key(&server, &l.id), format!("{label} · {}", l.title))));
    }
    filters
}
pub fn matches_filter(item: &MediaItem, filter: &str) -> bool {
    filter.is_empty()
        || filter == provider_key(item.id.server.provider)
        || filter == library_key(&item.id.server, &item.library_id)
}

/// Watch overlays are shared policy, never synchronization between providers.
#[derive(Default)]
pub struct Catalog {
    pub items: Vec<MediaItem>,
    pub watch: HashMap<ItemId, (i64, WatchState)>,
    pub watch_floor: i64,
    /// Where each id is in `items`, as of the last `merge`/`record`/`finish`; checked
    /// before use, since `items` is also set directly.
    index: HashMap<ItemId, usize>,
}
impl Catalog {
    fn position(&self, id: &ItemId) -> Option<usize> {
        match self.index.get(id) {
            Some(&i) if self.items.get(i).is_some_and(|x| x.id == *id) => Some(i),
            _ => self.items.iter().position(|i| i.id == *id),
        }
    }
    fn reindex(&mut self) {
        self.index = self.items.iter().enumerate().map(|(i, item)| (item.id.clone(), i)).collect();
    }
    pub fn latest(&self, mut item: MediaItem) -> MediaItem {
        if let Some((_, watch)) = self.watch.get(&item.id) {
            item.watch = watch.clone()
        } else if let Some(current) = self.position(&item.id).map(|i| &self.items[i])
            && current.watch.updated_at > item.watch.updated_at {
                item.watch = current.watch.clone()
            }
        item
    }
    pub fn confirmed(&mut self, item: &MediaItem, children: &[MediaItem], started: i64) {
        let positions: HashMap<_, _> = self.items.iter().enumerate().map(|(i, item)| (item.id.clone(), i)).collect();
        for media in std::iter::once(item).chain(children.iter()) {
            if self.watch.get(&media.id).is_some_and(|(revision, _)| *revision > started) {
                continue;
            }
            if let Some(entry) = self.watch.get_mut(&media.id) {
                *entry = (started, media.watch.clone())
            }
            if let Some(&index) = positions.get(&media.id) {
                self.items[index].watch = media.watch.clone()
            }
        }
    }
    pub fn record(&mut self, item: MediaItem, revision: i64) -> bool {
        if revision <= self.watch_floor || self.watch.get(&item.id).is_some_and(|(old, _)| *old > revision) {
            return false;
        }
        self.watch.insert(item.id.clone(), (revision, item.watch.clone()));
        if let Some(i) = self.position(&item.id) {
            self.items[i].watch = item.watch
        } else {
            self.index.insert(item.id.clone(), self.items.len());
            self.items.push(item)
        }
        true
    }
    pub fn merge(&mut self, items: Vec<MediaItem>) {
        let mut positions: HashMap<_, _> = self.items.iter().enumerate().map(|(i, item)| (item.id.clone(), i)).collect();
        for mut item in items {
            if let Some((_, watch)) = self.watch.get(&item.id) {
                item.watch = watch.clone()
            }
            if let Some(&index) = positions.get(&item.id) {
                self.items[index] = item
            } else {
                positions.insert(item.id.clone(), self.items.len());
                self.items.push(item)
            }
        }
        self.index = positions;
    }
    pub fn finish(&mut self, server: &ServerId, items: Vec<MediaItem>, started: i64) {
        self.watch_floor = started;
        self.watch.retain(|id, (revision, _)| id.server == *server && *revision > started);
        let recent = self.items.iter().filter(|i| self.watch.contains_key(&i.id)).cloned().collect();
        self.items.clear();
        self.reindex();
        self.merge(items);
        self.merge(recent);
    }
}

pub struct ConnectionState {
    pub provider: ProviderKind,
    pub connected: bool,
    pub busy: bool,
    pub message: String,
}
pub fn connection_states() -> Vec<ConnectionState> {
    let p = prefs::get();
    p.plex.map(|_| plex::connection_state()).into_iter().chain(p.jellyfin.map(|_| jellyfin::connection_state())).collect()
}

pub fn clients() -> Vec<Client> {
    [plex::client().map(Client::Plex), jellyfin::client().map(Client::Jellyfin)].into_iter().flatten().collect()
}

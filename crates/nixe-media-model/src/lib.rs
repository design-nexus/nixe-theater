//! Provider-independent metadata. Server keys and versions retain their provenance.
pub mod index;
pub mod metadata;
pub use metadata::*;
pub mod parts;
pub mod reconcile;
pub mod tv;

use serde::{Deserialize, Serialize};
use std::{future::Future, pin::Pin};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Plex,
    Jellyfin,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ServerId {
    pub provider: ProviderKind,
    /// Stable provider server/account identifier, not a URL or display name.
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ItemId {
    pub server: ServerId,
    pub id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Movie,
    Series,
    Season,
    Episode,
    Collection,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalIds {
    pub tmdb: Option<String>,
    pub imdb: Option<String>,
    pub tvdb: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchState {
    pub position_ms: u64,
    pub watched: bool,
    /// Server-provided timestamp when available; absence is not "newest".
    pub updated_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaVersion {
    pub id: String,
    pub edition: Option<String>,
    /// Opaque part keys; resolve authenticated URLs only when starting playback.
    pub parts: Vec<String>,
    #[serde(default)]
    pub part_durations_ms: Vec<u64>,
    pub video_codec: Option<String>,
    pub container: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    #[serde(default)]
    pub technical: Technical,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TvContext {
    pub series_title: Option<String>,
    /// Parent-show count for episode cards presented as show entries on Home.
    pub unwatched_episodes: Option<u32>,
    pub season_poster: Option<String>,
    pub series_poster: Option<String>,
    pub episode_count: Option<u32>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaPlaylist {
    pub id: ItemId,
    #[serde(default)]
    pub poster: Option<String>,
    pub title: String,
    pub count: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaItem {
    pub id: ItemId,
    pub library_id: String,
    pub kind: MediaKind,
    pub title: String,
    #[serde(default)]
    pub tv: TvContext,
    #[serde(default)]
    pub metadata: Metadata,
    pub summary: String,
    pub year: Option<u32>,
    pub external_ids: ExternalIds,
    pub parent: Option<ItemId>,
    pub series: Option<ItemId>,
    pub season_number: Option<u32>,
    pub episode_number: Option<u32>,
    pub duration_ms: Option<u64>,
    pub added_at: Option<i64>,
    /// Provider-relative keys only, without authentication parameters.
    pub poster: Option<String>,
    pub backdrop: Option<String>,
    pub versions: Vec<MediaVersion>,
    pub watch: WatchState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Library {
    pub id: String,
    pub title: String,
    pub kind: MediaKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerState {
    Disconnected,
    Connecting,
    Online,
    Offline,
    AuthenticationRequired,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reconciliation {
    #[default]
    Disabled,
    /// Explicit per-conflict review; no automatic cross-server writes.
    Manual,
}

/// Error messages must be safe for display: no tokens, authenticated URLs or bodies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderError {
    AuthenticationRequired,
    Offline,
    NotFound,
    Unsupported,
    InvalidResponse,
    Failed(String),
}
impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AuthenticationRequired => write!(f, "Sign in to this server"),
            Self::Offline => write!(f, "Server is offline"),
            Self::NotFound => write!(f, "Media is no longer available"),
            Self::Unsupported => write!(f, "This operation is not supported"),
            Self::InvalidResponse => write!(f, "Server returned an invalid response"),
            Self::Failed(message) => f.write_str(message),
        }
    }
}
impl std::error::Error for ProviderError {}

pub type ProviderFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ProviderError>> + Send + 'a>>;

pub struct Page<T> {
    pub items: Vec<T>,
    pub next: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackMode {
    Direct,
    Transcode,
}

pub struct PlaybackRequest {
    pub item: ItemId,
    pub version_id: String,
    pub position_ms: u64,
    pub mode: PlaybackMode,
    pub burn_subtitle: Option<i64>,
}

/// A provider subtitle URL is ephemeral and must never enter metadata caches or logs.
#[derive(Clone)]
pub struct ExternalSubtitle {
    pub id: i64,
    pub url: String,
    pub title: String,
    pub language: String,
    pub selected: bool,
    pub burn_in: bool,
}

/// Ephemeral credentials may be present: deliberately neither Debug nor Serialize.
/// HTTP headers must be applied only to the stream's trusted origin.
pub struct PlaybackSource {
    pub url: String,
    /// Ordered continuations of the same version, with ephemeral URL-scoped auth.
    /// Consumers concatenate these into one timeline, never separate watch sessions.
    pub additional_urls: Vec<String>,
    pub subtitles: Vec<ExternalSubtitle>,
    pub headers: Vec<(String, String)>,
    pub session_id: String,
    pub mode: PlaybackMode,
    /// Absolute item position corresponding to the start of a converted stream.
    pub stream_offset_ms: u64,
    pub duration_ms: Option<u64>,
    pub part_end_ms: Option<u64>,
    pub burned_subtitle: Option<i64>,
}

#[derive(Debug, Clone, Copy)]
pub enum PlaybackState {
    Playing,
    Paused,
    Stopped,
}

pub struct PlaybackProgress {
    pub item: ItemId,
    pub version_id: String,
    pub session_id: String,
    pub mode: PlaybackMode,
    pub state: PlaybackState,
    pub position_ms: u64,
    pub duration_ms: Option<u64>,
}

/// Adapters own authentication and transport; all I/O futures execute off GTK's
/// thread. Pagination is explicit so a large server never forces one huge request.
pub trait MediaProvider: Send + Sync {
    fn server(&self) -> &ServerId;
    fn libraries(&self) -> ProviderFuture<'_, Vec<Library>>;
    fn items<'a>(&'a self, library: &'a str, cursor: Option<&'a str>) -> ProviderFuture<'a, Page<MediaItem>>;
    fn details<'a>(&'a self, item: &'a ItemId) -> ProviderFuture<'a, MediaItem>;
    fn children<'a>(&'a self, item: &'a ItemId, cursor: Option<&'a str>) -> ProviderFuture<'a, Page<MediaItem>>;
    fn playback(&self, request: PlaybackRequest) -> ProviderFuture<'_, PlaybackSource>;
    fn report(&self, progress: PlaybackProgress) -> ProviderFuture<'_, ()>;
    fn playlists<'a>(&'a self, cursor: Option<&'a str>) -> ProviderFuture<'a, Page<MediaPlaylist>>;
    fn playlist_items<'a>(&'a self, playlist: &'a ItemId, cursor: Option<&'a str>) -> ProviderFuture<'a, Page<MediaItem>>;
    fn stop_session<'a>(&'a self, session: &'a str) -> ProviderFuture<'a, ()>;
}

#[derive(Clone)]
pub struct Secret(std::sync::Arc<String>);
impl Secret {
    pub fn new(value: String) -> std::result::Result<Self, ProviderError> {
        if value.is_empty() || value.len() > 4096 || value.chars().any(char::is_control) {
            return Err(ProviderError::Failed("Enter a valid access token".into()));
        }
        Ok(Self(std::sync::Arc::new(value)))
    }
    /// Only for Secret Service and request construction; never display or log.
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret([redacted])")
    }
}

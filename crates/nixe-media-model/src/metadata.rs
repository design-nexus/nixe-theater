//! Optional descriptive fields retain their source and never turn absence into zero.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rating {
    /// Stable source/category: imdb, tmdb, metacritic, rt_critic, rt_audience.
    pub source: String,
    /// Hundredths of the original source scale, avoiding lossy persisted floats.
    pub value: u32,
    pub scale: u32,
    pub votes: Option<u64>,
    pub url: Option<String>,
    pub fetched_at: i64,
}
impl Rating {
    pub fn normalized(&self) -> u32 {
        self.value.saturating_mul(100) / self.scale.max(1)
    }
    pub fn label(&self) -> String {
        let name = match self.source.as_str() {
            "imdb" => "IMDb",
            "tmdb" => "TMDB",
            "metacritic" => "Metacritic",
            "rt_critic" => "Rotten Tomatoes · critics",
            "rt_audience" => "Rotten Tomatoes · audience",
            "plex_critic" => "Plex · critics",
            "plex_audience" => "Plex · audience",
            "jellyfin" => "Jellyfin · community",
            "jellyfin_critic" => "Jellyfin · critics",
            _ => &self.source,
        };
        format!(
            "{} {:.1}/{}{}",
            name,
            self.value as f64 / 100.0,
            self.scale / 100,
            self.votes.map(|v| format!(" · {v} votes")).unwrap_or_default()
        )
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Credit {
    pub name: String,
    pub role: String,
    pub department: String,
    pub tmdb_id: Option<u64>,
    /// Either an approved TMDB image path or a provider-relative artwork key.
    pub photo: Option<String>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Technical {
    pub hdr: Option<String>,
    pub audio_codecs: Vec<String>,
    pub audio_languages: Vec<String>,
    pub subtitle_languages: Vec<String>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RelatedTitle {
    pub tmdb_id: u64,
    pub title: String,
    pub year: Option<u32>,
    pub series: bool,
    pub character: String,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Metadata {
    pub tagline: String,
    pub genres: Vec<String>,
    pub release_date: Option<String>,
    pub certification: Option<String>,
    pub original_language: Option<String>,
    pub countries: Vec<String>,
    pub studios: Vec<String>,
    pub ratings: Vec<Rating>,
    pub credits: Vec<Credit>,
    pub status: Option<String>,
    pub networks: Vec<String>,
    pub season_count: Option<u32>,
    pub episode_count: Option<u32>,
    pub unwatched_episodes: Option<u32>,
    pub latest_episode_added: Option<i64>,
    pub next_air_date: Option<String>,
    pub play_count: Option<u64>,
    pub awards: Option<String>,
    pub collection: Option<String>,
    pub collections: Vec<String>,
    pub recommendations: Vec<RelatedTitle>,
    pub trailers: Vec<String>,
    pub provider_trailers: Vec<crate::MediaItem>,
}
impl Metadata {
    /// Preserve server descriptions; supplement sparse fields and refresh sourced ratings.
    pub fn supplement(&mut self, other: &Self) {
        macro_rules! empty { ($($f:ident),*) => { $(if self.$f.is_empty() { self.$f = other.$f.clone(); })* }; }
        macro_rules! absent { ($($f:ident),*) => { $(if self.$f.is_none() { self.$f = other.$f.clone(); })* }; }
        empty!(tagline, genres, countries, studios, credits, networks, collections, recommendations, trailers, provider_trailers);
        absent!(
            release_date,
            certification,
            original_language,
            status,
            season_count,
            episode_count,
            unwatched_episodes,
            latest_episode_added,
            next_air_date,
            play_count,
            awards,
            collection
        );
        // Merge richer person identities/photos without replacing server cast ordering.
        for credit in &mut self.credits {
            if let Some(extra) = other.credits.iter().find(|c| c.name.eq_ignore_ascii_case(&credit.name)) {
                if credit.tmdb_id.is_none() {
                    credit.tmdb_id = extra.tmdb_id;
                }
                if credit.photo.is_none() {
                    credit.photo = extra.photo.clone();
                }
            }
        }
        for rating in &other.ratings {
            if let Some(current) = self.ratings.iter_mut().find(|r| r.source == rating.source) {
                if current.fetched_at <= rating.fetched_at {
                    *current = rating.clone();
                }
            } else {
                self.ratings.push(rating.clone());
            }
        }
    }
}

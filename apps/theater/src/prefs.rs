pub use nixe_theme::ThemeMode;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub plex: Option<PlexSettings>,
    pub jellyfin: Option<JellyfinSettings>,
    pub client_id: String,
    pub mode: ThemeMode,
    pub theme: String,
    pub glow: bool,
    pub reduce_motion: bool,
    pub sidebar_collapsed: bool,
    pub last_section: String,
    pub volume: f64,
    pub muted: bool,
    pub time_left: bool,
    pub poster_size: String,
    pub restore_view: bool,
    pub last_library_page: String,
    pub library_filter: String,
    pub library_sort: String,
    pub browse: std::collections::BTreeMap<String, crate::browse::State>,
    pub tmdb_enabled: bool,
    pub mdblist_enabled: bool,
    pub metadata_language: String,
    pub metadata_region: String,
    pub reconciliation: nixe_media_model::Reconciliation,
    /// Play the next episode after a countdown when one ends.
    pub autoplay_next: bool,
}
impl Default for Prefs {
    fn default() -> Self {
        Self {
            plex: None,
            jellyfin: None,
            client_id: String::new(),
            mode: ThemeMode::Omarchy,
            theme: "tokyo-night".into(),
            glow: true,
            reduce_motion: false,
            sidebar_collapsed: false,
            last_section: "home".into(),
            volume: 1.0,
            muted: false,
            time_left: false,
            poster_size: "medium".into(),
            restore_view: true,
            last_library_page: String::new(),
            library_filter: String::new(),
            library_sort: "title".into(),
            browse: Default::default(),
            tmdb_enabled: false,
            mdblist_enabled: false,
            metadata_language: "en-US".into(),
            metadata_region: "US".into(),
            reconciliation: Default::default(),
            autoplay_next: true,
        }
    }
}
nixe_core::prefs_store!(Prefs);

#[derive(Clone, Serialize, Deserialize)]
pub struct PlexSettings {
    pub address: String,
    pub name: String,
    pub server: nixe_media_model::ServerId,
    pub libraries: Vec<nixe_media_model::Library>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct JellyfinSettings {
    pub address: String,
    pub name: String,
    pub server: nixe_media_model::ServerId,
    pub libraries: Vec<nixe_media_model::Library>,
    pub user_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn older_preferences_get_medium_posters_and_choices_round_trip() {
        let old: Prefs = serde_json::from_str("{}").unwrap();
        assert_eq!(old.poster_size, "medium");
        for size in ["small", "medium", "large"] {
            let p = Prefs { poster_size: size.into(), ..Default::default() };
            let saved = serde_json::to_string(&p).unwrap();
            assert_eq!(serde_json::from_str::<Prefs>(&saved).unwrap().poster_size, size);
        }
    }
}

pub fn metadata_config() -> crate::metadata::Config {
    let p = get();
    crate::metadata::Config {
        tmdb: p.tmdb_enabled,
        mdblist: p.mdblist_enabled,
        language: p.metadata_language,
        region: p.metadata_region,
        epoch: 0,
    }
}
pub fn browse_state(page: &str) -> crate::browse::State {
    let p = get();
    p.browse.get(page).cloned().unwrap_or_else(|| crate::browse::State {
        source: p.library_filter,
        sort: p.library_sort.clone(),
        descending: p.library_sort == "newest" || p.library_sort == "year",
        ..Default::default()
    })
}

pub fn startup_page(p: &Prefs) -> &str {
    if !p.restore_view {
        return "home";
    }
    let remembered = if p.last_library_page.is_empty() { &p.last_section } else { &p.last_library_page };
    if matches!(remembered.as_str(), "home" | "movies" | "tv" | "collections" | "discover" | "playlists") {
        remembered
    } else {
        "home"
    }
}

#[cfg(test)]
mod startup_tests {
    use super::*;
    #[test]
    fn home_choice_is_absolute_and_last_restores_a_safe_library_page() {
        let mut p =
            Prefs { restore_view: false, last_library_page: "tv".into(), last_section: "playing".into(), ..Default::default() };
        assert_eq!(startup_page(&p), "home");
        p.restore_view = true;
        assert_eq!(startup_page(&p), "tv");
        p.last_library_page.clear();
        assert_eq!(startup_page(&p), "home");
        p.last_section = "discover".into();
        assert_eq!(startup_page(&p), "discover");
    }
}

//! Source marks use the same embedded symbolic-icon installation as shared playback.
pub fn install() {
    let dir = nixe_core::paths::cache_dir().join("rating-icons");
    for (name, svg) in [
        ("theater-filter-symbolic.svg", include_str!("../data/theater-filter-symbolic.svg")),
        ("theater-imdb-symbolic.svg", include_str!("../data/ratings/imdb-symbolic.svg")),
        ("theater-tmdb-symbolic.svg", include_str!("../data/ratings/themoviedatabase-symbolic.svg")),
        ("theater-tomato-symbolic.svg", include_str!("../data/ratings/rottentomatoes-symbolic.svg")),
        ("theater-popcorn-symbolic.svg", include_str!("../data/ratings/popcorn-symbolic.svg")),
        ("theater-metacritic-symbolic.svg", include_str!("../data/ratings/metacritic-symbolic.svg")),
        ("theater-plex-symbolic.svg", include_str!("../data/ratings/plex-symbolic.svg")),
        ("theater-jellyfin-symbolic.svg", include_str!("../data/ratings/jellyfin-symbolic.svg")),
    ] {
        let file = dir.join(name);
        if std::fs::read_to_string(&file).ok().as_deref() != Some(svg) {
            let _ = nixe_core::cmd::atomic_write(&file, svg);
        }
    }
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::IconTheme::for_display(&display).add_search_path(dir);
    }
}
pub fn icon(source: &str) -> gtk::Image {
    let name = match source {
        "imdb" => "theater-imdb-symbolic",
        "tmdb" => "theater-tmdb-symbolic",
        "rt_critic" => "theater-tomato-symbolic",
        "rt_audience" => "theater-popcorn-symbolic",
        "metacritic" => "theater-metacritic-symbolic",
        "plex_critic" | "plex_audience" => "theater-plex-symbolic",
        "jellyfin" | "jellyfin_critic" => "theater-jellyfin-symbolic",
        _ => "starred-symbolic",
    };
    let image = gtk::Image::from_icon_name(name);
    image.set_pixel_size(if matches!(source, "imdb" | "tmdb") { 32 } else { 24 });
    image
}

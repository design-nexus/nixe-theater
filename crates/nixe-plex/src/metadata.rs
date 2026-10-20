use nixe_media_model::*;
use serde_json::Value;
fn text(v: &Value) -> Option<String> {
    v.as_str().filter(|s| !s.is_empty()).map(str::to_owned)
}
fn tags(v: &Value, name: &str) -> Vec<String> {
    v[name].as_array().into_iter().flatten().filter_map(|v| text(&v["tag"])).collect()
}
pub fn parse(v: &Value, server: &ServerId, library: &str) -> Metadata {
    let mut m = Metadata {
        tagline: text(&v["tagline"]).unwrap_or_default(),
        genres: tags(v, "Genre"),
        release_date: text(&v["originallyAvailableAt"]),
        certification: text(&v["contentRating"]),
        countries: tags(v, "Country"),
        studios: text(&v["studio"]).into_iter().collect(),
        play_count: v["viewCount"].as_u64(),
        collections: tags(v, "Collection"),
        season_count: v["childCount"].as_u64().and_then(|n| n.try_into().ok()),
        episode_count: v["leafCount"].as_u64().and_then(|n| n.try_into().ok()),
        unwatched_episodes: v["leafCount"].as_u64().map(|n| n.saturating_sub(v["viewedLeafCount"].as_u64().unwrap_or(0)) as u32),
        ..Default::default()
    };
    for (field, department) in [("Role", "Cast"), ("Director", "Directing"), ("Writer", "Writing"), ("Producer", "Production")] {
        for p in v[field].as_array().into_iter().flatten() {
            if let Some(name) = text(&p["tag"]) {
                m.credits.push(Credit {
                    name,
                    role: text(&p["role"]).unwrap_or_else(|| department.into()),
                    department: department.into(),
                    photo: text(&p["thumb"]).filter(|s| (s.starts_with("/library/") && !s.contains('?')) || public_image(s)),
                    ..Default::default()
                });
            }
        }
    }
    for (field, image, fallback) in
        [("rating", "ratingImage", "plex_critic"), ("audienceRating", "audienceRatingImage", "plex_audience")]
    {
        if let Some(n) = v[field].as_f64().filter(|n| n.is_finite() && *n >= 0.0 && *n <= 10.0) {
            let image = v[image].as_str().unwrap_or_default();
            let source = if image.starts_with("imdb://") {
                "imdb"
            } else if image.starts_with("themoviedb://") {
                "tmdb"
            } else if image.starts_with("rottentomatoes://") {
                if field == "rating" { "rt_critic" } else { "rt_audience" }
            } else {
                fallback
            };
            let percent = source.starts_with("rt_");
            let url = if source == "imdb" {
                v["Guid"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|g| g["id"].as_str()?.strip_prefix("imdb://"))
                    .find(|id| id.starts_with("tt") && id[2..].chars().all(|c| c.is_ascii_digit()))
                    .map(|id| format!("https://www.imdb.com/title/{id}/"))
            } else {
                None
            };
            m.ratings.push(Rating {
                source: source.into(),
                value: (n * if percent { 1000.0 } else { 100.0 }).round() as u32,
                scale: if percent { 10000 } else { 1000 },
                url,
                ..Default::default()
            });
        }
    }
    for r in v["Rating"].as_array().into_iter().flatten() {
        let image = r["image"].as_str().unwrap_or_default();
        let source = if image.starts_with("imdb://") {
            Some("imdb")
        } else if image.starts_with("themoviedb://") {
            Some("tmdb")
        } else if image.starts_with("rottentomatoes://") {
            Some(if r["type"] == "audience" { "rt_audience" } else { "rt_critic" })
        } else {
            None
        };
        if let (Some(source), Some(n)) = (source, r["value"].as_f64().filter(|n| n.is_finite() && *n >= 0.0 && *n <= 10.0)) {
            let percent = source.starts_with("rt_");
            let rating = Rating {
                source: source.into(),
                value: (n * if percent { 1000.0 } else { 100.0 }).round() as u32,
                scale: if percent { 10000 } else { 1000 },
                votes: r["votes"].as_u64(),
                ..Default::default()
            };
            if let Some(existing) = m.ratings.iter_mut().find(|r| r.source == source) {
                *existing = rating;
            } else {
                m.ratings.push(rating);
            }
        }
    }
    m.provider_trailers = v["Extras"]["Metadata"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|v| v["extraType"] == 1)
        .take(8)
        .filter_map(|extra| super::parse_item(extra, server, library))
        .collect();

    m
}
pub fn technical(v: &Value) -> Technical {
    let streams: Vec<_> =
        v["Part"].as_array().into_iter().flatten().flat_map(|p| p["Stream"].as_array().into_iter().flatten()).collect();
    let video = streams.iter().find(|s| s["streamType"] == 1);
    let mut t = Technical::default();
    if let Some(video) = video {
        t.hdr = Some(
            if video["DOVIPresent"].as_bool() == Some(true) || video["DOVIProfile"].as_u64().is_some_and(|n| n > 0) {
                "Dolby Vision"
            } else if video["colorTrc"].as_str().is_some_and(|s| matches!(s, "smpte2084" | "arib-std-b67"))
                || video["displayTitle"].as_str().is_some_and(|s| s.contains("HDR"))
            {
                "HDR"
            } else {
                "SDR"
            }
            .into(),
        );
    }
    for s in streams {
        if s["streamType"] == 2 {
            if let Some(c) = text(&s["codec"]) {
                t.audio_codecs.push(c);
            }
            if let Some(l) = text(&s["languageCode"]).or_else(|| text(&s["language"])) {
                t.audio_languages.push(l);
            }
        }
        if s["streamType"] == 3
            && let Some(l) = text(&s["languageCode"]).or_else(|| text(&s["language"])) {
                t.subtitle_languages.push(l);
            }
    }
    t.audio_codecs.sort();
    t.audio_codecs.dedup();
    t.audio_languages.sort();
    t.audio_languages.dedup();
    t.subtitle_languages.sort();
    t.subtitle_languages.dedup();
    t
}

fn public_image(s: &str) -> bool {
    url::Url::parse(s).is_ok_and(|u| {
        u.scheme() == "https"
            && u.username().is_empty()
            && u.password().is_none()
            && u.query().is_none()
            && u.fragment().is_none()
            && matches!(u.host_str(), Some("metadata-static.plex.tv" | "images.plex.tv" | "image.tmdb.org"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn distinct_rating_sources_and_cast_survive_parsing() {
        let m = parse(
            &serde_json::json!({"Genre":[{"tag":"Drama"}],"Role":[{"tag":"Actor","role":"Character"}],"Rating":[{"image":"imdb://image.rating","value":8.1},{"image":"rottentomatoes://image.rating.ripe","type":"critic","value":9.0},{"image":"rottentomatoes://image.rating.upright","type":"audience","value":7.5}]}),
            &ServerId { provider: ProviderKind::Plex, id: "test".into() },
            "1",
        );
        assert_eq!(m.ratings.len(), 3);
        assert_eq!(m.ratings[1].source, "rt_critic");
        assert_eq!(m.ratings[2].source, "rt_audience");
        assert_eq!(m.ratings[1].normalized(), 90);
        assert_eq!(m.credits[0].role, "Character");
    }
}

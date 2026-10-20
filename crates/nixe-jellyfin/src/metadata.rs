use nixe_media_model::*;
use serde_json::Value;
fn text(v: &Value) -> Option<String> {
    v.as_str().filter(|s| !s.is_empty()).map(str::to_owned)
}
fn strings(v: &Value) -> Vec<String> {
    v.as_array().into_iter().flatten().filter_map(text).collect()
}
pub fn parse(v: &Value) -> Metadata {
    let mut m = Metadata {
        tagline: strings(&v["Taglines"]).join(" · "),
        genres: strings(&v["Genres"]),
        release_date: text(&v["PremiereDate"]).map(|s| s.chars().take(10).collect()),
        certification: text(&v["OfficialRating"]),
        original_language: text(&v["OriginalLanguage"]),
        countries: strings(&v["ProductionLocations"]),
        studios: v["Studios"].as_array().into_iter().flatten().filter_map(|s| text(&s["Name"])).collect(),
        status: text(&v["Status"]),
        play_count: v["UserData"]["PlayCount"].as_u64(),
        episode_count: v["RecursiveItemCount"].as_u64().map(|n| n as u32),
        unwatched_episodes: v["UserData"]["UnplayedItemCount"].as_u64().map(|n| n as u32),
        latest_episode_added: v["DateLastMediaAdded"]
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.timestamp()),
        trailers: v["RemoteTrailers"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| text(&v["Url"]))
            .filter(|s| s.starts_with("https://"))
            .collect(),
        ..Default::default()
    };
    for (field, source, scale) in [("CommunityRating", "jellyfin", 1000), ("CriticRating", "rt_critic", 10000)] {
        if let Some(n) = v[field].as_f64().filter(|n| n.is_finite() && *n >= 0.0 && *n <= scale as f64 / 100.0) {
            // Jellyfin's generic critic score does not identify its source.
            let source = if field == "CriticRating" { "jellyfin_critic" } else { source };
            m.ratings.push(Rating { source: source.into(), value: (n * 100.0).round() as u32, scale, ..Default::default() });
        }
    }
    for p in v["People"].as_array().into_iter().flatten() {
        if let Some(name) = text(&p["Name"]) {
            let department = match p["Type"].as_str().unwrap_or("") {
                "Actor" => "Cast",
                "Director" => "Directing",
                "Writer" => "Writing",
                "Producer" => "Production",
                _ => "Crew",
            };
            let photo = if p["PrimaryImageTag"].as_str().is_some() {
                text(&p["Id"])
                    .filter(|id| id.chars().all(|c| c.is_ascii_hexdigit() || c == '-'))
                    .map(|id| format!("Items/{id}/Images/Primary"))
            } else {
                None
            };
            m.credits.push(Credit {
                name,
                role: text(&p["Role"]).unwrap_or_else(|| department.into()),
                department: department.into(),
                photo,
                ..Default::default()
            });
        }
    }
    m
}
pub fn technical(source: &Value) -> Technical {
    let mut t = Technical::default();
    for s in source["MediaStreams"].as_array().into_iter().flatten() {
        match s["Type"].as_str() {
            Some("Video") => t.hdr = text(&s["VideoRangeType"]).or_else(|| text(&s["VideoRange"])),
            Some("Audio") => {
                if let Some(c) = text(&s["Codec"]) {
                    t.audio_codecs.push(c);
                }
                if let Some(l) = text(&s["Language"]) {
                    t.audio_languages.push(l);
                }
            }
            Some("Subtitle") => {
                if let Some(l) = text(&s["Language"]) {
                    t.subtitle_languages.push(l);
                }
            }
            _ => {}
        }
    }
    t.audio_languages.sort();
    t.audio_languages.dedup();
    t.subtitle_languages.sort();
    t.subtitle_languages.dedup();
    t
}

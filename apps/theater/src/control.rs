//! Theater's tools on the control interface (`nixe_core::control`), for Nixe Assistant:
//! find titles, see what's in progress and what's playing, play, control playback, and
//! mark something watched. Ids are opaque strings from `theater.search`.

use crate::{library, servers, window};
use nixe_core::control::text;
use nixe_media_model::{ItemId, MediaItem, MediaKind};
use serde_json::{Value, json};

pub fn tools() -> Value {
    let id = json!({"type": "string", "description": "An id from theater.search or theater.continue_watching"});
    json!([
        {"name": "theater.search", "description": "Find movies, shows and episodes in the Plex and Jellyfin libraries by title.", "inputSchema": {"type": "object", "properties": {"query": {"type": "string"}}, "required": ["query"]}},
        {"name": "theater.continue_watching", "description": "Movies and episodes started but not finished, most recent first.", "inputSchema": {"type": "object", "properties": {}}},
        {"name": "theater.now_playing", "description": "What Theater is playing: title, position, length, paused or not.", "inputSchema": {"type": "object", "properties": {}}},
        {"name": "theater.play", "description": "Play a movie or episode (a show plays its next unwatched episode), resuming unless from_start.", "inputSchema": {"type": "object", "properties": {"id": id, "from_start": {"type": "boolean"}}, "required": ["id"]}},
        {"name": "theater.control", "description": "Pause, resume, toggle, stop, or skip forward or back (seconds, 30 if not given).", "inputSchema": {"type": "object", "properties": {"action": {"enum": ["pause", "resume", "toggle", "stop", "forward", "back"]}, "seconds": {"type": "number"}}, "required": ["action"]}},
        {"name": "theater.mark_watched", "description": "Mark a title watched (or unwatched with watched=false); a show or season marks all its episodes.", "inputSchema": {"type": "object", "properties": {"id": id, "watched": {"type": "boolean"}}, "required": ["id"]}},
    ])
}

fn encode(id: &ItemId) -> String {
    serde_json::to_string(id).unwrap_or_default()
}

fn find(args: &Value) -> Result<MediaItem, String> {
    let raw = text(args, "id")?;
    let id: ItemId = serde_json::from_str(&raw).map_err(|_| "that isn't an id from theater.search")?;
    servers::items()
        .into_iter()
        .find(|i| i.id == id)
        .map(servers::latest_watch)
        .ok_or_else(|| "no title with that id is loaded; is its server connected?".into())
}

fn card(i: &MediaItem) -> Value {
    let kind = match i.kind {
        MediaKind::Movie => "movie",
        MediaKind::Series => "show",
        MediaKind::Season => "season",
        MediaKind::Episode => "episode",
        _ => "other",
    };
    json!({
        "id": encode(&i.id),
        "title": i.title,
        "kind": kind,
        "show": i.tv.series_title,
        "season": i.season_number,
        "episode": i.episode_number,
        "year": i.year,
        "server": servers::client(&i.id.server).map(|c| c.label()).unwrap_or("offline"),
        "watched": i.watch.watched,
        "minutes_in": i.watch.position_ms / 60_000,
        "minutes_long": i.duration_ms.map(|d| d / 60_000),
    })
}

/// The next episode of `show` to play: the first unwatched one, in order.
fn next_up(show: &MediaItem) -> Option<MediaItem> {
    let mut episodes: Vec<MediaItem> = servers::items()
        .into_iter()
        .filter(|i| i.kind == MediaKind::Episode && i.series.as_ref() == Some(&show.id))
        .map(servers::latest_watch)
        .collect();
    episodes.sort_by_key(|e| (e.season_number.unwrap_or(0) == 0, e.season_number, e.episode_number));
    episodes.into_iter().find(|e| !e.watch.watched)
}

pub fn handle(name: &str, args: Value) -> Result<Value, String> {
    match name {
        "theater.search" => {
            let q = text(&args, "query")?.to_lowercase();
            let words: Vec<&str> = q.split_whitespace().collect();
            let mut found: Vec<MediaItem> = servers::items()
                .into_iter()
                .filter(|i| matches!(i.kind, MediaKind::Movie | MediaKind::Series | MediaKind::Episode))
                .filter(|i| {
                    let hay = format!("{} {}", i.title, i.tv.series_title.as_deref().unwrap_or("")).to_lowercase();
                    words.iter().all(|w| hay.contains(w))
                })
                .collect();
            // Movies and shows before episodes, then by title.
            found.sort_by_key(|i| (i.kind == MediaKind::Episode, i.title.to_lowercase()));
            found.truncate(15);
            Ok(json!({"titles": found.iter().map(card).collect::<Vec<_>>(), "untrusted_content": true}))
        }
        "theater.continue_watching" => {
            let mut items: Vec<MediaItem> = servers::items()
                .into_iter()
                .map(servers::latest_watch)
                .filter(|i| matches!(i.kind, MediaKind::Movie | MediaKind::Episode) && i.watch.position_ms > 0 && !i.watch.watched)
                .collect();
            items.sort_by_key(|i| std::cmp::Reverse(i.watch.updated_at));
            items.truncate(10);
            Ok(json!({"titles": items.iter().map(card).collect::<Vec<_>>(), "untrusted_content": true}))
        }
        "theater.now_playing" => Ok(window::now_playing().map_or(json!({"playing": false}), |(title, position, duration, paused)| {
            json!({"playing": true, "title": title, "position_s": position.round(), "length_s": duration.round(), "paused": paused, "untrusted_content": true})
        })),
        "theater.play" => {
            let item = find(&args)?;
            let target = match item.kind {
                MediaKind::Series => next_up(&item).ok_or("every episode of that show is watched")?,
                MediaKind::Movie | MediaKind::Episode => item,
                _ => return Err("only movies, shows and episodes play".into()),
            };
            let from_start = args["from_start"].as_bool().unwrap_or(false);
            if !library::play_item(&target, from_start) {
                return Err("no playable source for that title is online".into());
            }
            Ok(json!({"playing": card(&target)}))
        }
        "theater.control" => {
            let action = text(&args, "action")?;
            let seconds = args["seconds"].as_f64().unwrap_or(30.0).clamp(1.0, 3600.0);
            window::control(&action, seconds)?;
            Ok(json!({"done": action}))
        }
        "theater.mark_watched" => {
            let item = find(&args)?;
            let watched = args["watched"].as_bool().unwrap_or(true);
            if servers::client(&item.id.server).is_none() {
                return Err("that title's server is offline".into());
            }
            library::set_watched(&item, watched);
            Ok(json!({"marking": card(&item), "watched": watched}))
        }
        _ => Err(format!("unknown tool {name}")),
    }
}

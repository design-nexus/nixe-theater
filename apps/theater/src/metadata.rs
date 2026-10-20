//! Cached optional enrichment. No HTTP, keyring or SQLite work runs on GTK.
use nixe_media_model::*;
use reqwest::blocking::Client;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    io::Read,
    sync::{Arc, Condvar, Mutex, OnceLock, RwLock},
    time::Duration,
};
#[derive(Clone, Default)]
pub struct Config {
    pub tmdb: bool,
    pub mdblist: bool,
    pub language: String,
    pub region: String,
    pub epoch: u64,
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Cached {
    metadata: Metadata,
    tmdb_at: i64,
    mdb_at: i64,
    plex_at: i64,
    locale: String,
}
#[derive(Default)]
struct Pending {
    items: Option<Vec<MediaItem>>,
    priority: Option<MediaItem>,
    config: Config,
    stop: bool,
}
struct Hub {
    data: RwLock<HashMap<String, Cached>>,
    ratings: RwLock<HashMap<String, u32>>,
    status: RwLock<String>,
    pending: Mutex<Pending>,
    wake: Condvar,
    notify: async_channel::Sender<()>,
}
static HUB: OnceLock<Arc<Hub>> = OnceLock::new();
pub fn status() -> String {
    HUB.get().map(|h| h.status.read().unwrap().clone()).unwrap_or_default()
}
fn activity(h: &Hub, message: &str) {
    let mut status = h.status.write().unwrap();
    if status.as_str() != message {
        *status = message.into();
        let _ = h.notify.try_send(());
    }
}
pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64
}
pub fn key(i: &MediaItem) -> String {
    let kind = format!("{:?}", i.kind);
    if let Some(id) = i.external_ids.tmdb.as_deref().filter(|s| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit())) {
        return format!("{kind}/tmdb/{id}");
    }
    if let Some(id) = i
        .external_ids
        .imdb
        .as_deref()
        .filter(|s| s.starts_with("tt") && s.len() > 2 && s[2..].bytes().all(|c| c.is_ascii_digit()))
    {
        return format!("{kind}/imdb/{id}");
    }
    serde_json::to_string(&i.id).unwrap_or_default()
}
pub fn apply(i: &mut MediaItem) {
    if let Some(h) = HUB.get()
        && let Some(c) = h.data.read().unwrap().get(&key(i)) {
            i.metadata.supplement(&c.metadata);
        }
}
pub fn provider_ratings_fresh(item: &MediaItem) -> bool {
    HUB.get().and_then(|h| h.data.read().ok()?.get(&key(item)).map(|c| now() - c.plex_at < 86400)).unwrap_or(false)
}
pub fn store_provider_ratings(items: &[MediaItem]) {
    store_provider_ratings_at(items, now());
}
fn store_provider_ratings_at(items: &[MediaItem], fetched_at: i64) {
    let Some(h) = HUB.get() else { return };
    let db = cache_db();
    for item in items {
        let id = key(item);
        let cached = {
            let mut data = h.data.write().unwrap();
            let c = data.entry(id.clone()).or_default();
            let extra = Metadata {
                ratings: item
                    .metadata
                    .ratings
                    .iter()
                    .cloned()
                    .map(|mut r| {
                        r.fetched_at = fetched_at;
                        r
                    })
                    .collect(),
                ..Default::default()
            };
            c.metadata.supplement(&extra);
            c.plex_at = c.plex_at.max(fetched_at);
            c.clone()
        };
        if let Some(db) = &db
            && let Ok(body) = serde_json::to_string(&cached) {
                let _ = db.execute(
                    "INSERT INTO enrichment VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET body=excluded.body",
                    params![id, body],
                );
            }
    }
}
pub fn personal(i: &MediaItem) -> Option<u32> {
    HUB.get()?.ratings.read().ok()?.get(&key(i)).copied()
}
pub fn configure(mut c: Config) {
    if let Some(h) = HUB.get() {
        let mut p = h.pending.lock().unwrap();
        c.epoch = p.config.epoch + 1;
        p.config = c;
        h.wake.notify_one();
    }
}
pub fn submit(items: &[MediaItem], page: &str) {
    if let Some(h) = HUB.get() {
        let mut items = items.to_vec();
        items.sort_by_key(|i| !(if page == "tv" { i.kind == MediaKind::Series } else { i.kind == MediaKind::Movie }));
        h.pending.lock().unwrap().items = Some(items);
        h.wake.notify_one();
    }
}
pub fn prioritize(item: MediaItem) {
    if let Some(h) = HUB.get() {
        h.pending.lock().unwrap().priority = Some(item);
        h.wake.notify_one();
    }
}
pub fn cancel() {
    if let Some(h) = HUB.get() {
        h.pending.lock().unwrap().stop = true;
        h.wake.notify_one();
    }
}
pub fn start(config: Config) -> async_channel::Receiver<()> {
    let (tx, rx) = async_channel::bounded(1);
    let h = Arc::new(Hub {
        data: Default::default(),
        ratings: Default::default(),
        status: Default::default(),
        pending: Mutex::new(Pending { config, ..Default::default() }),
        wake: Condvar::new(),
        notify: tx,
    });
    if HUB.set(h.clone()).is_ok() {
        std::thread::spawn(move || run(h));
    }
    rx
}
fn cache_db() -> Option<Connection> {
    let path = nixe_core::paths::cache_dir();
    std::fs::create_dir_all(&path).ok()?;
    let db = Connection::open(path.join("enrichment.sqlite3")).ok()?;
    db.busy_timeout(Duration::from_secs(2)).ok()?;
    db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS enrichment(id TEXT PRIMARY KEY,body TEXT NOT NULL); CREATE TABLE IF NOT EXISTS people(id TEXT PRIMARY KEY, body TEXT NOT NULL, updated INTEGER NOT NULL);").ok()?;
    Some(db)
}
fn user_db() -> Result<Connection, ProviderError> {
    let path = nixe_core::paths::data_dir();
    std::fs::create_dir_all(&path).map_err(|_| failure("Could not save your rating"))?;
    let db = Connection::open(path.join("library.sqlite3")).map_err(|_| failure("Could not save your rating"))?;
    db.busy_timeout(Duration::from_secs(2)).map_err(|_| failure("Library data is busy"))?;
    db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS ratings(id TEXT PRIMARY KEY,value INTEGER NOT NULL CHECK(value BETWEEN 50 AND 1000)); CREATE TABLE IF NOT EXISTS saved_views(id TEXT PRIMARY KEY,body TEXT NOT NULL);").map_err(|_|failure("Could not open library data"))?;
    Ok(db)
}
pub fn set_personal(i: MediaItem, rating: Option<u32>) {
    let id = key(&i);
    nixe_core::cmd::background(
        move || {
            let db = user_db()?;
            if let Some(v) = rating.filter(|v| (50..=1000).contains(v) && v % 50 == 0) {
                db.execute(
                    "INSERT INTO ratings VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET value=excluded.value",
                    params![id, v],
                )
                .map_err(|_| failure("Could not save your rating"))?;
            } else {
                db.execute("DELETE FROM ratings WHERE id=?1", [&id]).map_err(|_| failure("Could not clear your rating"))?;
            }
            if let Some(h) = HUB.get() {
                let mut r = h.ratings.write().unwrap();
                if let Some(v) = rating {
                    r.insert(id, v);
                } else {
                    r.remove(&id);
                }
            }
            Ok::<_, ProviderError>(())
        },
        |result| match result {
            Ok(()) => crate::library::refresh(),
            Err(e) => crate::window::status(&e.to_string()),
        },
    );
}
pub fn saved_views() -> Result<Vec<crate::browse::SavedView>, ProviderError> {
    let db = user_db()?;
    let mut s = db.prepare("SELECT body FROM saved_views ORDER BY id").map_err(|_| failure("Could not load saved views"))?;
    let rows = s.query_map([], |r| r.get::<_, String>(0)).map_err(|_| failure("Could not load saved views"))?;
    Ok(rows.filter_map(|r| serde_json::from_str(&r.ok()?).ok()).collect())
}
pub fn save_view(v: &crate::browse::SavedView) -> Result<(), ProviderError> {
    let db = user_db()?;
    let body = serde_json::to_string(v).map_err(|_| failure("Could not save view"))?;
    db.execute("INSERT INTO saved_views VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET body=excluded.body", params![v.name, body])
        .map_err(|_| failure("Could not save view"))?;
    Ok(())
}
pub fn delete_view(name: &str) -> Result<(), ProviderError> {
    user_db()?.execute("DELETE FROM saved_views WHERE id=?1", [name]).map_err(|_| failure("Could not delete view"))?;
    Ok(())
}
fn run(h: Arc<Hub>) {
    let db = cache_db();
    if let Some(db) = &db
        && let Ok(mut s) = db.prepare("SELECT id,body FROM enrichment")
            && let Ok(rows) = s.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))) {
                let mut data = HashMap::new();
                for row in rows.flatten() {
                    if let Ok(c) = serde_json::from_str(&row.1) {
                        data.insert(row.0, c);
                    }
                }
                *h.data.write().unwrap() = data;
            }
    if let Ok(cache) = crate::servers::database()
        && let Ok(items) = cache.detail_ratings() {
            store_provider_ratings_at(&items, 0);
        }
    if let Ok(db) = user_db()
        && let Ok(mut s) = db.prepare("SELECT id,value FROM ratings")
            && let Ok(rows) = s.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u32>(1)?))) {
                h.ratings.write().unwrap().extend(rows.flatten());
            }
    let _ = h.notify.try_send(());
    let Ok(http) = http() else { return };
    let mut epoch = u64::MAX;
    let (mut tmdb, mut mdb) = (None, None);
    let mut attempts: HashMap<(String, &str), i64> = HashMap::new();
    let mut blocked: HashMap<&str, i64> = HashMap::new();
    let mut catalog = vec![];
    loop {
        let cfg = {
            let mut p = h.pending.lock().unwrap();
            while p.items.is_none() && p.priority.is_none() && !p.stop && p.config.epoch == epoch {
                let (next, timeout) = h.wake.wait_timeout(p, Duration::from_secs(60)).unwrap();
                p = next;
                if timeout.timed_out() {
                    break;
                }
            }
            if p.stop {
                return;
            }
            if let Some(items) = p.items.take() {
                catalog = items;
            }
            if let Some(item) = p.priority.take() {
                catalog.retain(|i| i.id != item.id);
                catalog.insert(0, item);
            }
            p.config.clone()
        };
        if cfg.epoch != epoch {
            epoch = cfg.epoch;
            attempts.clear();
            blocked.clear();
            tmdb = if cfg.tmdb { crate::credentials::load_for("tmdb", "metadata").ok() } else { None };
            mdb = if cfg.mdblist { crate::credentials::load_for("mdblist", "metadata").ok() } else { None };
            if (cfg.tmdb && tmdb.is_none()) || (cfg.mdblist && mdb.is_none()) {
                activity(&h, "Metadata token unavailable · check Settings → Metadata");
            } else {
                activity(&h, "");
            }
        }
        if tmdb.is_none() && mdb.is_none() {
            continue;
        }
        let mut seen = HashSet::new();
        let mut changed = false;
        let mut last_notice = std::time::Instant::now();
        for item in &catalog {
            {
                let p = h.pending.lock().unwrap();
                if p.stop {
                    return;
                }
                if p.items.is_some() || p.priority.is_some() || p.config.epoch != epoch {
                    break;
                }
            }
            let id = key(item);
            if !seen.insert(id.clone()) {
                continue;
            }
            let mut cached = h.data.read().unwrap().get(&id).cloned().unwrap_or_default();
            for (name, secret, at) in [("tmdb", tmdb.as_ref(), cached.tmdb_at), ("mdblist", mdb.as_ref(), cached.mdb_at)] {
                let Some(secret) = secret else { continue };
                if blocked.get(name).is_some_and(|until| *until > now()) {
                    continue;
                }
                let locale = format!("{}/{}", cfg.language, cfg.region);
                if (now() - at < 86400 && (name != "tmdb" || cached.locale == locale))
                    || attempts.get(&(id.clone(), name)).is_some_and(|until| *until > now())
                {
                    continue;
                }
                let result =
                    if name == "tmdb" { fetch_tmdb(&http, item, &catalog, secret, &cfg) } else { fetch_mdb(&http, item, secret) };
                match result {
                    Ok(Some(extra)) => {
                        let previous = std::mem::replace(&mut cached.metadata, extra);
                        cached.metadata.supplement(&previous);
                        if name == "tmdb" {
                            cached.locale = locale;
                            cached.tmdb_at = now()
                        } else {
                            cached.mdb_at = now()
                        };
                        *h.status.write().unwrap() = String::new();
                        changed = true;
                        {
                            let mut data = h.data.write().unwrap();
                            if let Some(latest) = data.get(&id) {
                                cached.metadata.supplement(&latest.metadata);
                                cached.plex_at = cached.plex_at.max(latest.plex_at);
                            }
                            data.insert(id.clone(), cached.clone());
                        }
                        if let Some(db) = &db
                            && let Ok(body) = serde_json::to_string(&cached) {
                                let _ = db.execute(
                                    "INSERT INTO enrichment VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET body=excluded.body",
                                    params![id, body],
                                );
                            }
                    }
                    Ok(None) => {
                        attempts.insert((id.clone(), name), now() + 86400);
                    }
                    Err(error) => {
                        activity(
                            &h,
                            if name == "tmdb" {
                                "TMDB unavailable · using cached metadata"
                            } else {
                                "MDBList unavailable · using cached ratings"
                            },
                        );
                        let wait = error.retry.max(300);
                        attempts.insert((id.clone(), name), now() + wait);
                        if error.global {
                            blocked.insert(name, now() + wait);
                        }
                    }
                }
            }
            if changed && last_notice.elapsed() > Duration::from_secs(2) {
                let _ = h.notify.try_send(());
                changed = false;
                last_notice = std::time::Instant::now();
            }
        }
        if changed {
            let _ = h.notify.try_send(());
        }
    }
}
fn failure(s: &str) -> ProviderError {
    ProviderError::Failed(s.into())
}
fn http() -> Result<Client, ProviderError> {
    Client::builder()
        .timeout(Duration::from_secs(12))
        .connect_timeout(Duration::from_secs(4))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("Theater/0.1")
        .build()
        .map_err(|_| failure("Metadata service unavailable"))
}
struct FetchError {
    retry: i64,
    global: bool,
}
fn json(request: reqwest::blocking::RequestBuilder) -> Result<Value, FetchError> {
    let response = request.send().map_err(|_| FetchError { retry: 300, global: true })?;
    let status = response.status();
    if !status.is_success() {
        let retry =
            response.headers().get("retry-after").and_then(|h| h.to_str().ok()).and_then(|s| s.parse().ok()).unwrap_or(300);
        return Err(FetchError { retry, global: matches!(status.as_u16(), 401 | 403 | 429 | 500..=599) });
    }
    let mut bytes = vec![];
    response.take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes).map_err(|_| FetchError { retry: 300, global: false })?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(FetchError { retry: 300, global: false });
    }
    serde_json::from_slice(&bytes).map_err(|_| FetchError { retry: 3600, global: false })
}
fn tmdb_json(http: &Client, path: &str, secret: &Secret, cfg: &Config, append: &str) -> Result<Value, FetchError> {
    json(
        http.get(format!("https://api.themoviedb.org/3/{path}"))
            .bearer_auth(secret.expose())
            .query(&[("language", cfg.language.as_str()), ("append_to_response", append)]),
    )
}
fn digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit())
}
fn tmdb_route(i: &MediaItem, all: &[MediaItem]) -> Option<String> {
    let own = i.external_ids.tmdb.as_deref().filter(|s| digits(s));
    match i.kind {
        MediaKind::Movie => Some(format!("movie/{}", own?)),
        MediaKind::Series => Some(format!("tv/{}", own?)),
        MediaKind::Season | MediaKind::Episode => {
            let parent = all.iter().find(|p| Some(&p.id) == i.series.as_ref())?;
            let id = parent.external_ids.tmdb.as_deref().filter(|s| digits(s))?;
            let mut route = format!("tv/{id}/season/{}", i.season_number?);
            if i.kind == MediaKind::Episode {
                route.push_str(&format!("/episode/{}", i.episode_number?));
            }
            Some(route)
        }
        _ => None,
    }
}
fn fetch_tmdb(
    http: &Client,
    item: &MediaItem,
    all: &[MediaItem],
    secret: &Secret,
    cfg: &Config,
) -> Result<Option<Metadata>, FetchError> {
    let mut route = tmdb_route(item, all);
    if route.is_none() && matches!(item.kind, MediaKind::Movie | MediaKind::Series)
        && let Some(imdb) = item.external_ids.imdb.as_deref().filter(|s| s.starts_with("tt") && s.len() > 2 && digits(&s[2..])) {
            let v = json(
                http.get(format!("https://api.themoviedb.org/3/find/{imdb}"))
                    .bearer_auth(secret.expose())
                    .query(&[("external_source", "imdb_id")]),
            )?;
            let field = if item.kind == MediaKind::Movie { "movie_results" } else { "tv_results" };
            if let Some(id) = v[field].as_array().filter(|a| a.len() == 1).and_then(|a| a[0]["id"].as_u64()) {
                route = Some(format!("{}/{id}", if item.kind == MediaKind::Movie { "movie" } else { "tv" }));
            }
        }
    let Some(route) = route else { return Ok(None) };
    let v = tmdb_json(http, &route, secret, cfg, "credits,recommendations,videos,release_dates,content_ratings,external_ids")?;
    let m = parse_tmdb(&v, &route, &cfg.region);
    Ok(Some(m))
}
fn text(v: &Value) -> Option<String> {
    v.as_str().filter(|s| !s.is_empty()).map(str::to_owned)
}
fn names(v: &Value) -> Vec<String> {
    v.as_array().into_iter().flatten().filter_map(|v| text(&v["name"])).collect()
}
fn related(v: &Value, series: bool) -> Option<RelatedTitle> {
    Some(RelatedTitle {
        tmdb_id: v["id"].as_u64()?,
        title: text(&v["title"]).or_else(|| text(&v["name"]))?,
        year: v["release_date"].as_str().or_else(|| v["first_air_date"].as_str()).and_then(|s| s.get(..4)?.parse().ok()),
        series,
        character: text(&v["character"]).or_else(|| text(&v["job"])).unwrap_or_default(),
    })
}
fn credit(p: &Value, cast: bool) -> Option<Credit> {
    Some(Credit {
        name: text(&p["name"])?,
        role: text(&p["character"]).or_else(|| text(&p["job"])).unwrap_or_default(),
        department: if cast { "Cast".into() } else { text(&p["department"]).unwrap_or_else(|| "Crew".into()) },
        tmdb_id: p["id"].as_u64(),
        photo: text(&p["profile_path"]).filter(|s| image_path(s)),
    })
}
fn parse_tmdb(v: &Value, route: &str, region: &str) -> Metadata {
    let series = route.starts_with("tv/");
    let mut m = Metadata {
        tagline: text(&v["tagline"]).unwrap_or_default(),
        genres: names(&v["genres"]),
        release_date: text(&v["release_date"]).or_else(|| text(&v["first_air_date"])).or_else(|| text(&v["air_date"])),
        original_language: text(&v["original_language"]),
        countries: names(&v["production_countries"]),
        studios: names(&v["production_companies"]),
        status: text(&v["status"]),
        networks: names(&v["networks"]),
        season_count: v["number_of_seasons"].as_u64().map(|n| n as u32),
        episode_count: v["number_of_episodes"].as_u64().map(|n| n as u32),
        next_air_date: text(&v["next_episode_to_air"]["air_date"]),
        collection: text(&v["belongs_to_collection"]["name"]),
        ..Default::default()
    };
    if let Some(score) = v["vote_average"].as_f64().filter(|s| s.is_finite() && *s >= 0.0 && *s <= 10.0)
        && v["vote_count"].as_u64().unwrap_or(0) > 0 {
            m.ratings.push(Rating {
                source: "tmdb".into(),
                value: (score * 100.0).round() as u32,
                scale: 1000,
                votes: v["vote_count"].as_u64(),
                url: Some(format!("https://www.themoviedb.org/{route}")),
                fetched_at: now(),
            });
        }
    m.certification = v["content_ratings"]["results"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|v| v["iso_3166_1"] == region)
        .and_then(|v| text(&v["rating"]))
        .or_else(|| {
            v["release_dates"]["results"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|v| v["iso_3166_1"] == region)
                .and_then(|v| v["release_dates"].as_array()?.iter().find_map(|v| text(&v["certification"])))
        });
    m.credits.extend(v["credits"]["cast"].as_array().into_iter().flatten().filter_map(|p| credit(p, true)));
    m.credits.extend(
        v["credits"]["crew"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|p| {
                matches!(p["job"].as_str(), Some("Director" | "Writer" | "Screenplay" | "Producer" | "Executive Producer"))
            })
            .filter_map(|p| credit(p, false)),
    );
    m.credits.extend(v["created_by"].as_array().into_iter().flatten().filter_map(|p| {
        let mut c = credit(p, false)?;
        c.role = "Creator".into();
        c.department = "Creator".into();
        Some(c)
    }));
    m.recommendations =
        v["recommendations"]["results"].as_array().into_iter().flatten().filter_map(|p| related(p, series)).collect();
    for video in
        v["videos"]["results"].as_array().into_iter().flatten().filter(|v| v["type"] == "Trailer" && v["site"] == "YouTube")
    {
        if let Some(key) = video["key"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() < 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        {
            m.trailers.push(format!("https://www.youtube.com/watch?v={key}"));
        }
    }
    m
}
fn fetch_mdb(http: &Client, item: &MediaItem, secret: &Secret) -> Result<Option<Metadata>, FetchError> {
    if !matches!(item.kind, MediaKind::Movie | MediaKind::Series) {
        return Ok(None);
    }
    let (provider, id) =
        if let Some(id) = item.external_ids.imdb.as_deref().filter(|s| s.starts_with("tt") && s.len() > 2 && digits(&s[2..])) {
            ("imdb", id)
        } else if let Some(id) = item.external_ids.tmdb.as_deref().filter(|s| digits(s)) {
            ("tmdb", id)
        } else {
            return Ok(None);
        };
    let kind = if item.kind == MediaKind::Movie { "movie" } else { "show" };
    let v = json(http.get(format!("https://api.mdblist.com/{provider}/{kind}/{id}")).query(&[("apikey", secret.expose())]))?;
    Ok(Some(parse_mdb(&v, item)))
}
fn parse_mdb(v: &Value, item: &MediaItem) -> Metadata {
    let mut m =
        Metadata { awards: text(&v["awards"]), next_air_date: text(&v["next_episode"]["air_date"]), ..Default::default() };
    for r in v["ratings"].as_array().into_iter().flatten() {
        let (source, scale) = match r["source"].as_str() {
            Some("imdb") => ("imdb", 1000),
            Some("tmdb") => ("tmdb", 1000),
            Some("metacritic") => ("metacritic", 10000),
            Some("tomatoes" | "rtomatoes") => ("rt_critic", 10000),
            Some("audience" | "popcorn" | "tomatoesaudience" | "tomatoes_audience") => ("rt_audience", 10000),
            _ => continue,
        };
        let n = r["value"].as_f64().or_else(|| r["value"].as_str()?.parse().ok());
        if let Some(n) = n.filter(|n| n.is_finite() && *n >= 0.0 && *n <= scale as f64 / 100.0) {
            let url = if source == "imdb" {
                item.external_ids.imdb.as_ref().map(|id| format!("https://www.imdb.com/title/{id}/"))
            } else {
                r["url"].as_str().filter(|s| safe_link(s)).map(str::to_owned)
            };
            m.ratings.push(Rating {
                source: source.into(),
                value: (n * 100.0).round() as u32,
                scale,
                votes: r["votes"].as_u64(),
                url,
                fetched_at: now(),
            });
        }
    }
    m
}
pub fn safe_link(s: &str) -> bool {
    url::Url::parse(s)
        .is_ok_and(|u| u.scheme() == "https" && u.host_str().is_some() && u.username().is_empty() && u.password().is_none())
}
pub fn public_image(s: &str) -> bool {
    url::Url::parse(s).is_ok_and(|u| {
        u.scheme() == "https"
            && u.username().is_empty()
            && u.password().is_none()
            && u.query().is_none()
            && u.fragment().is_none()
            && matches!(u.host_str(), Some("metadata-static.plex.tv" | "images.plex.tv" | "image.tmdb.org"))
    })
}
fn image_path(s: &str) -> bool {
    s.starts_with('/') && s.len() < 200 && s[1..].bytes().all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Person {
    pub name: String,
    pub biography: String,
    pub birthday: Option<String>,
    pub birthplace: Option<String>,
    pub photo: Option<String>,
    pub titles: Vec<RelatedTitle>,
}
pub fn person(id: u64, cfg: Config) -> Result<Person, ProviderError> {
    let db = cache_db();
    let cached = db
        .as_ref()
        .and_then(|d| {
            d.query_row("SELECT body,updated FROM people WHERE id=?1", [id.to_string()], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })
            .ok()
        })
        .and_then(|(s, t)| Some((serde_json::from_str::<Person>(&s).ok()?, t)));
    if let Some((p, t)) = &cached
        && now() - t < 604800 {
            return Ok(p.clone());
        }
    let fetch = || {
        let secret = crate::credentials::load_for("tmdb", "metadata")?;
        let v = tmdb_json(&http()?, &format!("person/{id}"), &secret, &cfg, "combined_credits")
            .map_err(|_| failure("Person information is unavailable"))?;
        let p = Person {
            name: text(&v["name"]).unwrap_or_default(),
            biography: text(&v["biography"]).unwrap_or_default(),
            birthday: text(&v["birthday"]),
            birthplace: text(&v["place_of_birth"]),
            photo: text(&v["profile_path"]).filter(|s| image_path(s)),
            titles: v["combined_credits"]["cast"]
                .as_array()
                .into_iter()
                .flatten()
                .chain(v["combined_credits"]["crew"].as_array().into_iter().flatten())
                .filter_map(|v| related(v, v["media_type"] == "tv"))
                .collect(),
        };
        if let Some(db) = &db {
            let _ = db.execute(
                "INSERT INTO people VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET body=excluded.body,updated=excluded.updated",
                params![id.to_string(), serde_json::to_string(&p).unwrap_or_default(), now()],
            );
        }
        Ok(p)
    };
    fetch().or_else(|error| cached.map(|c| c.0).ok_or(error))
}
/// Approved image host only, bounded response and persistent disk cache.
pub fn image(path: &str) -> Option<std::path::PathBuf> {
    if !image_path(path) && !public_image(path) {
        return None;
    }
    use sha2::{Digest, Sha256};
    let dir = nixe_core::paths::cache_dir().join("people-artwork");
    std::fs::create_dir_all(&dir).ok()?;
    let file = dir.join(format!("{:x}.jpg", Sha256::digest(path)));
    if file.exists() {
        return Some(file);
    }
    let image_url = if public_image(path) { path.to_owned() } else { format!("https://image.tmdb.org/t/p/w185{path}") };
    let response = http().ok()?.get(image_url).send().ok()?.error_for_status().ok()?;
    let mut bytes = vec![];
    response.take(2 * 1024 * 1024 + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() > 2 * 1024 * 1024 {
        return None;
    }
    nixe_core::cmd::atomic_write(&file, bytes).ok()?;
    Some(file)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_scores_stay_missing_and_categories_are_distinct() {
        let m = parse_tmdb(&serde_json::json!({"vote_average":0,"vote_count":0}), "movie/1", "US");
        assert!(m.ratings.is_empty());
        assert!(!safe_link("file:///tmp/test"));
        assert!(!image_path("/../../secret"));
    }
}

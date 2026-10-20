//! Plex transport. Errors never expose URLs, credentials, response bodies or TLS diagnostics.
//! Synchronous methods run on app workers; MediaProvider futures use a blocking pool.
mod metadata;
use nixe_media_model::*;
use reqwest::{
    blocking::{Client as Http, Response},
    header::{HeaderMap, HeaderValue},
    redirect,
};
use serde_json::Value;
use std::{io::Read, time::Duration};
use url::Url;

type Result<T> = std::result::Result<T, ProviderError>;
const PAGE_SIZE: usize = 100;
const MAX_BODY: u64 = 16 * 1024 * 1024;
pub use nixe_media_model::Secret;
fn http() -> Result<Http> {
    Http::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(20))
        .redirect(redirect::Policy::none())
        .build()
        .map_err(|_| ProviderError::Offline)
}
fn headers(client_id: &str, secret: Option<&Secret>) -> Result<HeaderMap> {
    let mut h = HeaderMap::new();
    h.insert("Accept", HeaderValue::from_static("application/json"));
    h.insert("X-Plex-Product", HeaderValue::from_static("Theater"));
    h.insert("X-Plex-Version", HeaderValue::from_static("0.1.0"));
    h.insert("X-Plex-Platform", HeaderValue::from_static("Linux"));
    h.insert("X-Plex-Device", HeaderValue::from_static("PC"));
    h.insert("X-Plex-Client-Identifier", HeaderValue::from_str(client_id).map_err(|_| ProviderError::InvalidResponse)?);
    if let Some(secret) = secret {
        let mut value = HeaderValue::from_str(secret.expose()).map_err(|_| ProviderError::AuthenticationRequired)?;
        value.set_sensitive(true);
        h.insert("X-Plex-Token", value);
    }
    Ok(h)
}
fn response(response: std::result::Result<Response, reqwest::Error>) -> Result<Response> {
    let response = response.map_err(|_| ProviderError::Offline)?;
    match response.status().as_u16() {
        200..=299 => Ok(response),
        401 | 403 => Err(ProviderError::AuthenticationRequired),
        404 => Err(ProviderError::NotFound),
        300..=399 => Err(ProviderError::Failed("Plex redirected the request. Enter the server's final address.".into())),
        _ => Err(ProviderError::Failed("Plex could not complete the request".into())),
    }
}
fn json(response: Response) -> Result<Value> {
    let mut bytes = Vec::new();
    response.take(MAX_BODY + 1).read_to_end(&mut bytes).map_err(|_| ProviderError::Offline)?;
    if bytes.len() as u64 > MAX_BODY {
        return Err(ProviderError::InvalidResponse);
    }
    serde_json::from_slice(&bytes).map_err(|_| ProviderError::InvalidResponse)
}

pub fn server_url(address: &str) -> Result<Url> {
    let mut u = Url::parse(address.trim()).map_err(|_| ProviderError::Failed("Enter an HTTP or HTTPS server address".into()))?;
    if !matches!(u.scheme(), "http" | "https")
        || u.host_str().is_none()
        || !u.username().is_empty()
        || u.password().is_some()
        || u.query().is_some()
        || u.fragment().is_some()
        || !matches!(u.path(), "" | "/")
    {
        return Err(ProviderError::Failed("Use a server address without credentials, path, query or fragment".into()));
    }
    u.set_path("/");
    Ok(u)
}
fn relative_key(path: &str) -> bool {
    path.starts_with('/')
        && !path.starts_with("//")
        && !path.contains(['?', '#', '\\'])
        && !path.chars().any(char::is_control)
        && !path.contains("://")
}
fn id(value: &str) -> Result<&str> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) { Err(ProviderError::InvalidResponse) } else { Ok(value) }
}

pub struct Pin {
    pub id: u64,
    pub code: String,
    pub login_url: String,
}
pub fn create_pin(client_id: &str) -> Result<Pin> {
    let v = json(response(
        http()?.post("https://plex.tv/api/v2/pins").headers(headers(client_id, None)?).form(&[("strong", "true")]).send(),
    )?)?;
    let pin_id = v["id"].as_u64().ok_or(ProviderError::InvalidResponse)?;
    let code = v["code"].as_str().filter(|s| !s.is_empty()).ok_or(ProviderError::InvalidResponse)?.to_owned();
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("clientID", client_id)
        .append_pair("code", &code)
        .append_pair("context[device][product]", "Theater")
        .finish();
    Ok(Pin { id: pin_id, code, login_url: format!("https://app.plex.tv/auth#?{query}") })
}
pub fn check_pin(client_id: &str, pin: &Pin) -> Result<Option<Secret>> {
    let v = json(response(
        http()?
            .get(format!("https://plex.tv/api/v2/pins/{}", pin.id))
            .headers(headers(client_id, None)?)
            .query(&[("code", &pin.code)])
            .send(),
    )?)?;
    v["authToken"].as_str().filter(|t| !t.is_empty()).map(|t| Secret::new(t.into())).transpose()
}

/// Account resources carry server-specific credentials. Never log or serialize them.
#[derive(Clone)]
pub struct DiscoveredServer {
    pub name: String,
    pub id: String,
    token: Secret,
    connections: Vec<(u8, String)>,
}
pub fn discover(client_id: &str, account_token: &Secret) -> Result<Vec<DiscoveredServer>> {
    let value = json(response(
        http()?
            .get("https://clients.plex.tv/api/v2/resources")
            .headers(headers(client_id, Some(account_token))?)
            .query(&[("includeHttps", "1"), ("includeRelay", "1"), ("includeIPv6", "1")])
            .send(),
    )?)?;
    parse_resources(&value)
}
fn flag(value: &Value) -> bool {
    value.as_bool().unwrap_or_else(|| value.as_u64() == Some(1) || value.as_str() == Some("1"))
}
fn parse_resources(value: &Value) -> Result<Vec<DiscoveredServer>> {
    let entries = value.as_array().ok_or(ProviderError::InvalidResponse)?;
    let mut result = Vec::new();
    for entry in entries {
        if !entry["provides"].as_str().is_some_and(|s| s.split(',').any(|s| s.trim() == "server")) {
            continue;
        }
        let Some(server_id) = entry["clientIdentifier"].as_str().filter(|s| !s.is_empty()) else { continue };
        let Some(token) = entry["accessToken"].as_str().and_then(|s| Secret::new(s.into()).ok()) else { continue };
        let mut connections = Vec::new();
        for c in entry["connections"].as_array().into_iter().flatten() {
            let Some(url) = c["uri"].as_str().and_then(|s| server_url(s).ok()) else { continue };
            if flag(&entry["httpsRequired"]) && url.scheme() != "https" {
                continue;
            }
            let priority = u8::from(flag(&c["relay"])) * 4 + u8::from(!flag(&c["local"])) * 2 + u8::from(url.scheme() != "https");
            connections.push((priority, url.to_string()));
        }
        connections.sort();
        connections.dedup();
        connections.truncate(12);
        result.push(DiscoveredServer {
            name: entry["name"].as_str().unwrap_or("Plex server").into(),
            id: server_id.into(),
            token,
            connections,
        });
    }
    result.sort_by_key(|s| s.name.to_lowercase());
    Ok(result)
}
impl DiscoveredServer {
    /// Probe equivalent connection choices concurrently; local connections precede
    /// remote ones, with relay last. Confirm machine identity before accepting one.
    pub fn connect(&self, client_id: &str) -> Result<(Client, Secret)> {
        let mut priorities: Vec<_> = self.connections.iter().map(|(p, _)| *p).collect();
        priorities.dedup();
        for priority in priorities {
            let (tx, rx) = std::sync::mpsc::channel();
            for (_, address) in self.connections.iter().filter(|(p, _)| *p == priority).take(4) {
                let (address, token, identifier, expected, tx) =
                    (address.clone(), self.token.clone(), client_id.to_owned(), self.id.clone(), tx.clone());
                std::thread::spawn(move || {
                    let result = (|| {
                        let transport = Http::builder()
                            .connect_timeout(Duration::from_secs(3))
                            .timeout(Duration::from_secs(6))
                            .redirect(redirect::Policy::none())
                            .build()
                            .map_err(|_| ProviderError::Offline)?;
                        let mut client = Client::connect_with_http(&address, token, &identifier, transport)?;
                        if client.server.id != expected {
                            return Err(ProviderError::InvalidResponse);
                        }
                        // Normal operations retain the standard request timeouts.
                        client.http = http()?;
                        Ok(client)
                    })();
                    let _ = tx.send(result);
                });
            }
            drop(tx);
            if let Some(client) = rx.into_iter().flatten().next() {
                return Ok((client, self.token.clone()));
            }
        }
        Err(ProviderError::Failed("This Plex server is unavailable. Retry or use a manual connection.".into()))
    }
}

#[derive(Clone)]
pub struct Client {
    http: Http,
    base: Url,
    token: Secret,
    client_id: String,
    server: ServerId,
    pub name: String,
}
impl Client {
    /// Startup probing is bounded independently of metadata/stream requests.
    pub fn reconnect(address: &str, token: Secret, client_id: &str) -> Result<Self> {
        let transport = Http::builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(6))
            .redirect(redirect::Policy::none())
            .build()
            .map_err(|_| ProviderError::Offline)?;
        let mut client = Self::connect_with_http(address, token, client_id, transport)?;
        client.http = http()?;
        Ok(client)
    }
    pub fn connect(address: &str, token: Secret, client_id: &str) -> Result<Self> {
        Self::connect_with_http(address, token, client_id, http()?)
    }
    fn connect_with_http(address: &str, token: Secret, client_id: &str, transport: Http) -> Result<Self> {
        let mut client = Self {
            http: transport,
            base: server_url(address)?,
            token,
            client_id: client_id.into(),
            server: ServerId { provider: ProviderKind::Plex, id: String::new() },
            name: "Plex".into(),
        };
        let v = client.get("/", &[])?;
        let container = &v["MediaContainer"];
        client.server.id =
            container["machineIdentifier"].as_str().filter(|s| !s.is_empty()).ok_or(ProviderError::InvalidResponse)?.into();
        client.name = container["friendlyName"].as_str().unwrap_or("Plex").into();
        // Root identity can be public; verify access before accepting/saving credentials.
        client.list_libraries()?;
        Ok(client)
    }
    pub fn address(&self) -> String {
        self.base.to_string()
    }
    pub fn server_id(&self) -> &ServerId {
        &self.server
    }
    fn url(&self, path: &str) -> Result<Url> {
        if !relative_key(path) {
            return Err(ProviderError::InvalidResponse);
        }
        let u = self.base.join(path).map_err(|_| ProviderError::InvalidResponse)?;
        if u.origin() != self.base.origin() {
            return Err(ProviderError::InvalidResponse);
        }
        Ok(u)
    }
    fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Value> {
        json(response(self.http.get(self.url(path)?).headers(headers(&self.client_id, Some(&self.token))?).query(query).send())?)
    }
    pub fn list_libraries(&self) -> Result<Vec<Library>> {
        let v = self.get("/library/sections", &[])?;
        let list = v["MediaContainer"]["Directory"].as_array().ok_or(ProviderError::InvalidResponse)?;
        list.iter()
            .filter_map(|entry| {
                let kind = match entry["type"].as_str()? {
                    "movie" | "clip" => MediaKind::Movie,
                    "show" => MediaKind::Series,
                    _ => return None,
                };
                Some(id(entry["key"].as_str().unwrap_or_default()).map(|key| Library {
                    id: key.into(),
                    title: entry["title"].as_str().unwrap_or("Library").into(),
                    kind,
                }))
            })
            .collect()
    }
    fn page(&self, path: &str, library: &str, cursor: Option<&str>) -> Result<Page<MediaItem>> {
        self.page_kind(path, library, cursor, None)
    }
    pub fn playlists(&self, cursor: Option<&str>) -> Result<Page<MediaPlaylist>> {
        let start: usize = cursor.unwrap_or("0").parse().map_err(|_| ProviderError::InvalidResponse)?;
        if start >= 50000 {
            return Err(ProviderError::InvalidResponse);
        }
        let value = self.get(
            "/playlists",
            &[
                ("playlistType", "video".into()),
                ("X-Plex-Container-Start", start.to_string()),
                ("X-Plex-Container-Size", PAGE_SIZE.to_string()),
            ],
        )?;
        let c = &value["MediaContainer"];
        if c["offset"].as_u64().is_some_and(|n| n != start as u64) {
            return Err(ProviderError::InvalidResponse);
        }
        let raw = c["Metadata"].as_array().cloned().unwrap_or_default();
        let end = start + raw.len();
        let items = raw
            .iter()
            .filter(|v| v["playlistType"].as_str().is_none_or(|t| t == "video"))
            .filter_map(|v| {
                Some(MediaPlaylist {
                    id: ItemId { server: self.server.clone(), id: id(v["ratingKey"].as_str()?).ok()?.into() },
                    title: v["title"].as_str()?.into(),
                    count: v["leafCount"].as_u64(),
                    poster: v["thumb"]
                        .as_str()
                        .or(v["composite"].as_str())
                        .filter(|s| relative_key(s) && (s.starts_with("/library/") || s.starts_with("/playlists/")))
                        .map(String::from),
                })
            })
            .collect();
        let more = !raw.is_empty() && c["totalSize"].as_u64().map_or(raw.len() >= PAGE_SIZE, |n| (end as u64) < n);
        Ok(Page { items, next: more.then(|| end.to_string()) })
    }
    pub fn playlist_items(&self, playlist: &ItemId, cursor: Option<&str>) -> Result<Page<MediaItem>> {
        if playlist.server != self.server {
            return Err(ProviderError::InvalidResponse);
        }
        let mut page = self.page(&format!("/playlists/{}/items", id(&playlist.id)?), "", cursor)?;
        page.items.retain(|i| matches!(i.kind, MediaKind::Movie | MediaKind::Episode));
        Ok(page)
    }
    pub fn season_page(&self, library: &str, cursor: Option<&str>) -> Result<Page<MediaItem>> {
        self.page_kind(&format!("/library/sections/{}/all", id(library)?), library, cursor, Some("3"))
    }
    pub fn episode_page(&self, library: &str, cursor: Option<&str>) -> Result<Page<MediaItem>> {
        self.page_kind(&format!("/library/sections/{}/all", id(library)?), library, cursor, Some("4"))
    }
    fn page_kind(&self, path: &str, library: &str, cursor: Option<&str>, kind: Option<&str>) -> Result<Page<MediaItem>> {
        let start: usize = cursor.unwrap_or("0").parse().map_err(|_| ProviderError::InvalidResponse)?;
        let mut query = vec![
            ("X-Plex-Container-Start", start.to_string()),
            ("X-Plex-Container-Size", PAGE_SIZE.to_string()),
            ("includeGuids", "1".into()),
            ("includeOptionalElements", "Rating".into()),
        ];
        if let Some(kind) = kind {
            query.push(("type", kind.into()));
        }
        let v = self.get(path, &query)?;
        let c = &v["MediaContainer"];
        if c["offset"].as_u64().is_some_and(|offset| offset != start as u64) {
            return Err(ProviderError::InvalidResponse);
        }
        if start >= 100_000 {
            return Err(ProviderError::Failed("Plex pagination limit reached".into()));
        }
        let raw = match c.get("Metadata") {
            Some(Value::Array(items)) => items.as_slice(),
            None if c["size"] == 0 => &[],
            _ => return Err(ProviderError::InvalidResponse),
        };
        let items: Vec<_> = raw.iter().filter_map(|entry| parse_item(entry, &self.server, library)).collect();
        let end = start.checked_add(raw.len()).ok_or(ProviderError::InvalidResponse)?;
        let more = !raw.is_empty() && c["totalSize"].as_u64().map(|n| (end as u64) < n).unwrap_or(raw.len() >= PAGE_SIZE);
        Ok(Page { items, next: more.then(|| end.to_string()) })
    }
    pub fn library_page(&self, library: &str, cursor: Option<&str>) -> Result<Page<MediaItem>> {
        self.page(&format!("/library/sections/{}/all", id(library)?), library, cursor)
    }
    pub fn collection_page(&self, library: &str, cursor: Option<&str>) -> Result<Page<MediaItem>> {
        self.page(&format!("/library/sections/{}/collections", id(library)?), library, cursor)
    }
    /// Full rating elements are commonly omitted from section listings.
    pub fn rating_details(&self, items: &[ItemId]) -> Result<Vec<MediaItem>> {
        if items.is_empty() || items.len() > 24 {
            return Err(ProviderError::InvalidResponse);
        }
        for item in items {
            self.validate_item(item)?;
        }
        let keys = items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>().join(",");
        let result = self.get(
            &format!("/library/metadata/{keys}"),
            &[("includeGuids", "1".into()), ("includeOptionalElements", "Rating".into()), ("includeRatings", "1".into())],
        );
        match result {
            Ok(v) => {
                let raw = v["MediaContainer"]["Metadata"].as_array().ok_or(ProviderError::InvalidResponse)?;
                Ok(raw.iter().filter_map(|v| parse_item(v, &self.server, "")).filter(|i| items.contains(&i.id)).collect())
            }
            Err(ProviderError::InvalidResponse | ProviderError::NotFound | ProviderError::Unsupported) if items.len() > 1 => {
                items.iter().map(|i| self.detail(i)).collect()
            }
            Err(e) => Err(e),
        }
    }
    pub fn detail(&self, key: &ItemId) -> Result<MediaItem> {
        self.validate_item(key)?;
        let v = self.get(
            &format!("/library/metadata/{}", id(&key.id)?),
            &[("includeGuids", "1".into()), ("includeExtras", "1".into()), ("includeRatings", "1".into())],
        )?;
        v["MediaContainer"]["Metadata"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|v| parse_item(v, &self.server, ""))
            .ok_or(ProviderError::InvalidResponse)
    }
    pub fn child_page(&self, key: &ItemId, library: &str, cursor: Option<&str>) -> Result<Page<MediaItem>> {
        self.validate_item(key)?;
        self.page(&format!("/library/metadata/{}/children", id(&key.id)?), library, cursor)
    }
    pub fn continue_page(&self) -> Result<Page<MediaItem>> {
        self.page("/library/onDeck", "", None)
    }
    fn validate_item(&self, key: &ItemId) -> Result<()> {
        if key.server == self.server { id(&key.id).map(|_| ()) } else { Err(ProviderError::InvalidResponse) }
    }
    pub fn resolve(&self, request: PlaybackRequest) -> Result<PlaybackSource> {
        self.validate_item(&request.item)?;
        let item = self.detail(&request.item)?;
        let (media_index, version) =
            item.versions.iter().enumerate().find(|(_, v)| v.id == request.version_id).ok_or(ProviderError::NotFound)?;
        if version.parts.is_empty() || version.parts.len() > 64 {
            return Err(ProviderError::InvalidResponse);
        }
        let durations: Vec<_> =
            if version.parts.len() == 1 { vec![item.duration_ms.unwrap_or(1).max(1)] } else { version.part_durations_ms.clone() };
        let (index, start, total) = nixe_media_model::parts::locate(&durations, request.position_ms)?;
        if durations.len() != version.parts.len() {
            return Err(ProviderError::InvalidResponse);
        }
        let mut source = if request.mode == PlaybackMode::Transcode {
            self.transcode_source(&item.id, media_index, index, request.position_ms.saturating_sub(start))?
        } else {
            let mut url = self.url(&version.parts[index])?;
            url.query_pairs_mut().append_pair("X-Plex-Token", self.token.expose());
            PlaybackSource {
                url: url.into(),
                additional_urls: vec![],
                subtitles: vec![],
                headers: vec![],
                session_id: uuid::Uuid::new_v4().to_string(),
                mode: PlaybackMode::Direct,
                stream_offset_ms: 0,
                duration_ms: None,
                part_end_ms: None,
                burned_subtitle: None,
            }
        };
        source.stream_offset_ms = if request.mode == PlaybackMode::Transcode { request.position_ms } else { start };
        source.duration_ms = Some(total);
        source.part_end_ms = (version.parts.len() > 1).then_some(start + durations[index]);
        Ok(source)
    }
    fn transcode_source(&self, item: &ItemId, media_index: usize, part_index: usize, position_ms: u64) -> Result<PlaybackSource> {
        let session = uuid::Uuid::new_v4().to_string();
        // A progressive Matroska stream avoids HLS subrequests needing credentials
        // in global FFmpeg headers. Seeking restarts conversion at the new offset.
        let query = vec![
            ("path", format!("/library/metadata/{}", item.id)),
            ("mediaIndex", media_index.to_string()), ("partIndex", part_index.to_string()),
            ("session", session.clone()), ("transcodeSessionId", session.clone()),
            ("protocol", "http".into()), ("directPlay", "0".into()),
            ("directStream", "0".into()), ("directStreamAudio", "0".into()),
            ("offset", format!("{:.3}", position_ms as f64 / 1000.0)),
            ("videoResolution", "1920x1080".into()), ("videoBitrate", "8000".into()),
            ("audioChannelCount", "2".into()), ("subtitles", "embedded".into()),
            ("advancedSubtitles", "text".into()), ("X-Plex-Client-Profile-Name", "Generic".into()),
            ("X-Plex-Client-Profile-Extra", "add-transcode-target(type=videoProfile&context=streaming&protocol=http&container=mkv&videoCodec=h264&audioCodec=aac&subtitleCodec=srt&replace=true)".into()),
            ("X-Plex-Client-Identifier", self.client_id.clone()),
            ("X-Plex-Session-Identifier", session.clone()), ("X-Plex-Product", "Theater".into()),
        ];
        let mut h = headers(&self.client_id, Some(&self.token))?;
        h.insert("X-Plex-Session-Identifier", HeaderValue::from_str(&session).map_err(|_| ProviderError::InvalidResponse)?);
        let decision =
            json(response(self.http.get(self.url("/video/:/transcode/universal/decision")?).headers(h).query(&query).send())?)?;
        let c = &decision["MediaContainer"];
        let code = c["generalDecisionCode"]
            .as_i64()
            .or_else(|| c["transcodeDecisionCode"].as_i64())
            .ok_or(ProviderError::InvalidResponse)?;
        if !(1000..2000).contains(&code) {
            return Err(ProviderError::Failed(
                "Plex cannot convert this video. Check the server's transcoder settings and permissions.".into(),
            ));
        }
        let mut url = self.url("/video/:/transcode/universal/start.mkv")?;
        url.query_pairs_mut().extend_pairs(query).append_pair("X-Plex-Token", self.token.expose());
        Ok(PlaybackSource {
            url: url.into(),
            additional_urls: vec![],
            subtitles: vec![],
            headers: vec![],
            session_id: session,
            mode: PlaybackMode::Transcode,
            stream_offset_ms: position_ms,
            duration_ms: None,
            part_end_ms: None,
            burned_subtitle: None,
        })
    }
    /// Idempotent cleanup of this playback's UUID, never a server-wide stop.
    pub fn stop_transcode(&self, session: &str) -> Result<()> {
        uuid::Uuid::parse_str(session).map_err(|_| ProviderError::InvalidResponse)?;
        let result = response(
            self.http
                .get(self.url("/video/:/transcode/universal/stop")?)
                .headers(headers(&self.client_id, Some(&self.token))?)
                .query(&[("session", session)])
                .send(),
        );
        match result {
            Ok(_) | Err(ProviderError::NotFound) => Ok(()),
            Err(error) => Err(error),
        }
    }
    pub fn timeline(&self, progress: PlaybackProgress) -> Result<()> {
        self.validate_item(&progress.item)?;
        let mut h = headers(&self.client_id, Some(&self.token))?;
        h.insert(
            "X-Plex-Session-Identifier",
            HeaderValue::from_str(&progress.session_id).map_err(|_| ProviderError::InvalidResponse)?,
        );
        response(
            self.http
                .post(self.url("/:/timeline")?)
                .headers(h)
                .query(&[
                    ("key", format!("/library/metadata/{}", progress.item.id)),
                    ("ratingKey", progress.item.id),
                    (
                        "state",
                        match progress.state {
                            PlaybackState::Playing => "playing",
                            PlaybackState::Paused => "paused",
                            PlaybackState::Stopped => "stopped",
                        }
                        .into(),
                    ),
                    ("time", progress.position_ms.to_string()),
                    ("duration", progress.duration_ms.unwrap_or_default().to_string()),
                ])
                .send(),
        )?;
        Ok(())
    }
    pub fn apply_watch(&self, item: &ItemId, state: &WatchState) -> Result<MediaItem> {
        self.validate_item(item)?;
        let current = self.detail(item)?;
        if current.watch.watched != state.watched || (!state.watched && state.position_ms == 0) {
            response(
                self.http
                    .put(self.url(if state.watched { "/:/scrobble" } else { "/:/unscrobble" })?)
                    .headers(headers(&self.client_id, Some(&self.token))?)
                    .query(&[("identifier", "com.plexapp.plugins.library"), ("key", item.id.as_str())])
                    .send(),
            )?;
        }
        if !state.watched && state.position_ms > 0 {
            self.timeline(PlaybackProgress {
                item: item.clone(),
                version_id: current.versions.first().map(|v| v.id.clone()).unwrap_or_default(),
                session_id: uuid::Uuid::new_v4().to_string(),
                mode: PlaybackMode::Direct,
                state: PlaybackState::Stopped,
                position_ms: state.position_ms,
                duration_ms: current.duration_ms,
            })?;
        }
        self.detail(item)
    }
    pub fn artwork(&self, key: &str, width: u32, height: u32) -> Result<Vec<u8>> {
        if !(key.starts_with("/library/") || key.starts_with("/playlists/")) {
            return Err(ProviderError::InvalidResponse);
        }
        self.url(key)?;
        let r = response(
            self.http
                .get(self.url("/photo/:/transcode")?)
                .headers(headers(&self.client_id, Some(&self.token))?)
                .query(&[
                    ("url", key.to_owned()),
                    ("width", width.to_string()),
                    ("height", height.to_string()),
                    ("upscale", "0".into()),
                    ("minSize", "1".into()),
                ])
                .send(),
        )?;
        if !r.headers().get("Content-Type").and_then(|v| v.to_str().ok()).is_some_and(|v| v.starts_with("image/")) {
            return Err(ProviderError::InvalidResponse);
        }
        let mut bytes = Vec::new();
        r.take(8 * 1024 * 1024 + 1).read_to_end(&mut bytes).map_err(|_| ProviderError::Offline)?;
        if bytes.len() > 8 * 1024 * 1024 {
            return Err(ProviderError::InvalidResponse);
        }
        Ok(bytes)
    }
}

pub fn parse_item(v: &Value, server: &ServerId, library: &str) -> Option<MediaItem> {
    let kind = match v["type"].as_str()? {
        "movie" => MediaKind::Movie,
        "show" => MediaKind::Series,
        "season" => MediaKind::Season,
        "episode" => MediaKind::Episode,
        "collection" => MediaKind::Collection,
        _ => return None,
    };
    let key = id(v["ratingKey"].as_str()?).ok()?.to_owned();
    let mut external = ExternalIds::default();
    for guid in v["Guid"].as_array().into_iter().flatten().filter_map(|g| g["id"].as_str()) {
        if let Some((scheme, value)) = guid.split_once("://") {
            if value.is_empty() || !value.chars().all(|c| c.is_ascii_alphanumeric()) {
                continue;
            }
            match scheme {
                "tmdb" => external.tmdb = Some(value.into()),
                "imdb" => external.imdb = Some(value.into()),
                "tvdb" => external.tvdb = Some(value.into()),
                _ => {}
            }
        }
    }
    let item_id =
        |value: &Value| value.as_str().and_then(|s| id(s).ok()).map(|s| ItemId { server: server.clone(), id: s.into() });
    let art = |field: &str| v[field].as_str().filter(|s| relative_key(s) && s.starts_with("/library/")).map(String::from);
    let versions = v["Media"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(index, media)| MediaVersion {
            id: media["id"]
                .as_str()
                .map(String::from)
                .or_else(|| media["id"].as_u64().map(|n| n.to_string()))
                .unwrap_or_else(|| index.to_string()),
            edition: v["editionTitle"].as_str().map(String::from),
            parts: media["Part"]
                .as_array()
                .and_then(|parts| {
                    parts
                        .iter()
                        .map(|part| {
                            part["key"].as_str().filter(|s| relative_key(s) && s.starts_with("/library/parts/")).map(String::from)
                        })
                        .collect::<Option<Vec<_>>>()
                })
                .unwrap_or_default(),
            part_durations_ms: media["Part"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|part| part["duration"].as_u64().unwrap_or(0))
                .collect(),
            video_codec: media["videoCodec"].as_str().map(String::from),
            container: media["container"].as_str().map(String::from),
            width: media["width"].as_u64().and_then(|n| n.try_into().ok()),
            height: media["height"].as_u64().and_then(|n| n.try_into().ok()),
            technical: metadata::technical(media),
        })
        .collect();
    Some(MediaItem {
        id: ItemId { server: server.clone(), id: key },
        library_id: v["librarySectionID"]
            .as_u64()
            .map(|n| n.to_string())
            .or_else(|| v["librarySectionID"].as_str().map(String::from))
            .unwrap_or_else(|| library.into()),
        kind,
        title: v["title"].as_str().unwrap_or("Untitled").into(),
        tv: TvContext {
            unwatched_episodes: None,
            series_title: (if kind == MediaKind::Episode { v["grandparentTitle"].as_str() } else { v["parentTitle"].as_str() })
                .map(String::from),
            season_poster: if kind == MediaKind::Episode { art("parentThumb") } else { None },
            series_poster: art("grandparentThumb"),
            episode_count: (kind == MediaKind::Season).then(|| v["leafCount"].as_u64().and_then(|n| n.try_into().ok())).flatten(),
        },
        metadata: metadata::parse(v, server, library),
        summary: v["summary"].as_str().unwrap_or_default().into(),
        year: v["year"].as_u64().and_then(|n| n.try_into().ok()),
        external_ids: external,
        parent: item_id(&v["parentRatingKey"]),
        series: item_id(&v["grandparentRatingKey"])
            .or_else(|| (kind == MediaKind::Season).then(|| item_id(&v["parentRatingKey"])).flatten()),
        season_number: if kind == MediaKind::Season { v["index"].as_u64() } else { v["parentIndex"].as_u64() }
            .and_then(|n| n.try_into().ok()),
        episode_number: (kind == MediaKind::Episode).then(|| v["index"].as_u64().and_then(|n| n.try_into().ok())).flatten(),
        duration_ms: v["duration"].as_u64(),
        added_at: v["addedAt"].as_i64(),
        poster: art("thumb"),
        backdrop: art("art"),
        versions,
        watch: WatchState {
            position_ms: v["viewOffset"].as_u64().unwrap_or_default(),
            watched: v["viewCount"].as_u64().unwrap_or_default() > 0,
            updated_at: v["lastViewedAt"].as_i64(),
        },
    })
}
impl MediaProvider for Client {
    fn server(&self) -> &ServerId {
        &self.server
    }
    fn libraries(&self) -> ProviderFuture<'_, Vec<Library>> {
        let c = self.clone();
        Box::pin(blocking::unblock(move || c.list_libraries()))
    }
    fn items<'a>(&'a self, library: &'a str, cursor: Option<&'a str>) -> ProviderFuture<'a, Page<MediaItem>> {
        let (c, l, next) = (self.clone(), library.to_owned(), cursor.map(String::from));
        Box::pin(blocking::unblock(move || c.library_page(&l, next.as_deref())))
    }
    fn details<'a>(&'a self, item: &'a ItemId) -> ProviderFuture<'a, MediaItem> {
        let (c, key) = (self.clone(), item.clone());
        Box::pin(blocking::unblock(move || c.detail(&key)))
    }
    fn children<'a>(&'a self, item: &'a ItemId, cursor: Option<&'a str>) -> ProviderFuture<'a, Page<MediaItem>> {
        let (c, key, next) = (self.clone(), item.clone(), cursor.map(String::from));
        Box::pin(blocking::unblock(move || c.child_page(&key, "", next.as_deref())))
    }
    fn playback(&self, request: PlaybackRequest) -> ProviderFuture<'_, PlaybackSource> {
        let c = self.clone();
        Box::pin(blocking::unblock(move || c.resolve(request)))
    }
    fn report(&self, progress: PlaybackProgress) -> ProviderFuture<'_, ()> {
        let c = self.clone();
        Box::pin(blocking::unblock(move || c.timeline(progress)))
    }
    fn playlists<'a>(&'a self, cursor: Option<&'a str>) -> ProviderFuture<'a, Page<MediaPlaylist>> {
        let (c, cursor) = (self.clone(), cursor.map(String::from));
        Box::pin(blocking::unblock(move || c.playlists(cursor.as_deref())))
    }
    fn playlist_items<'a>(&'a self, playlist: &'a ItemId, cursor: Option<&'a str>) -> ProviderFuture<'a, Page<MediaItem>> {
        let (c, key, cursor) = (self.clone(), playlist.clone(), cursor.map(String::from));
        Box::pin(blocking::unblock(move || c.playlist_items(&key, cursor.as_deref())))
    }
    fn stop_session<'a>(&'a self, session: &'a str) -> ProviderFuture<'a, ()> {
        let (client, session) = (self.clone(), session.to_owned());
        Box::pin(blocking::unblock(move || client.stop_transcode(&session)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
        thread,
    };
    fn movie() -> Value {
        serde_json::json!({"ratingKey":"42","type":"movie","title":"Fixture movie","librarySectionID":1,
        "duration":600000,"viewOffset":120000,"editionTitle":"Director's cut", "Guid":[{"id":"tmdb://123"},{"id":"imdb://tt123"}],
        "thumb":"/library/metadata/42/thumb/1", "Media":[{"id":7,"videoCodec":"h264","height":1080,"Part":[{"key":"/library/parts/8/file.mkv"}]},{"id":9,"videoCodec":"hevc","height":2160,"Part":[{"key":"/library/parts/10/file.mkv"}]},
        {"id":11,"videoCodec":"h264","Part":[{"key":"/library/parts/12/first.mkv","duration":300000},{"key":"/library/parts/13/second.mkv","duration":300000}]}]})
    }
    #[test]
    fn playlist_pages_preserve_video_order_and_repeated_entries() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            for _ in 0..6 {
                let (mut stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                }
                let uri = Url::parse(&format!("http://fixture{}", first.split_whitespace().nth(1).unwrap())).unwrap();
                let q: std::collections::HashMap<_, _> = uri.query_pairs().into_owned().collect();
                let data = match uri.path() {
                    "/" => serde_json::json!({"MediaContainer":{"machineIdentifier":"fixture-server"}}),
                    "/library/sections" => serde_json::json!({"MediaContainer":{"Directory":[]}}),
                    "/playlists" => {
                        assert_eq!(q["playlistType"], "video");
                        let offset = q["X-Plex-Container-Start"].parse::<u64>().unwrap();
                        serde_json::json!({"MediaContainer":{"offset":offset,"totalSize":2,"Metadata":[{"ratingKey":format!("{}",123+offset),"title":"Queue","playlistType":"video","leafCount":2,"composite":"/playlists/123/composite/7"}]}})
                    }
                    "/playlists/123/items" => {
                        let offset = q["X-Plex-Container-Start"].parse::<u64>().unwrap();
                        serde_json::json!({"MediaContainer":{"offset":offset,"totalSize":2,"Metadata":[movie()]}})
                    }
                    _ => panic!("Unexpected request"),
                };
                let body = data.to_string();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        let client = Client::connect(&address, Secret::new("fixture-token".into()).unwrap(), "fixture").unwrap();
        let page = client.playlists(None).unwrap();
        assert_eq!(page.next.as_deref(), Some("1"));
        assert_eq!(page.items[0].count, Some(2));
        assert_eq!(page.items[0].poster.as_deref(), Some("/playlists/123/composite/7"));
        assert!(client.playlists(page.next.as_deref()).unwrap().next.is_none());
        let first = client.playlist_items(&page.items[0].id, None).unwrap();
        let second = client.playlist_items(&page.items[0].id, first.next.as_deref()).unwrap();
        assert_eq!(first.items[0].id, second.items[0].id);
        assert!(second.next.is_none());
        let mut foreign = page.items[0].id.clone();
        foreign.server.id = "another".into();
        assert!(client.playlist_items(&foreign, None).is_err());
        let value = serde_json::json!({"type":"episode","ratingKey":"321","title":"Pilot","grandparentTitle":"Show","parentIndex":1,"index":2,"parentThumb":"/library/metadata/2/thumb/1","grandparentThumb":"/library/metadata/3/thumb/1","thumb":"/library/metadata/321/thumb/1"});
        let episode = parse_item(&value, client.server_id(), "1").unwrap();
        assert_eq!(episode.tv.series_title.as_deref(), Some("Show"));
        assert_eq!(nixe_media_model::tv::posters(&episode)[0], "/library/metadata/2/thumb/1");
        server.join().unwrap();
    }
    #[test]
    fn authenticates_pages_resolves_versions_and_posts_progress_without_leaking_errors() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let mut watched = false;
            for _ in 0..18 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                let mut raw = String::new();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    raw.push_str(&line);
                }
                assert!(raw.to_lowercase().contains("x-plex-token: fixture-token"));
                assert!(!first.contains("fixture-token"));
                let path = first.split_whitespace().nth(1).unwrap();
                let (status, body) = if path == "/" {
                    (200, serde_json::json!({"MediaContainer":{"machineIdentifier":"fixture-server","friendlyName":"Fixture"}}))
                } else if path.starts_with("/library/sections?") || path == "/library/sections" {
                    (200, serde_json::json!({"MediaContainer":{"Directory":[{"key":"1","title":"Movies","type":"movie"}]}}))
                } else if path.starts_with("/library/sections/1/all") {
                    let second = path.contains("Start=2");
                    (
                        200,
                        serde_json::json!({"MediaContainer":{"offset":if second {2} else {0},"totalSize":3,"Metadata":if second {vec![movie()]} else {vec![movie(),movie()]}}}),
                    )
                } else if path.starts_with("/library/metadata/42?") {
                    {
                        let mut value = movie();
                        value["viewCount"] = serde_json::json!(if watched { 1 } else { 0 });
                        (200, serde_json::json!({"MediaContainer":{"Metadata":[value]}}))
                    }
                } else if path.starts_with("/video/:/transcode/universal/decision?") {
                    let url = Url::parse(&format!("http://fixture{path}")).unwrap();
                    let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
                    assert!(q["mediaIndex"] == "1" || q["mediaIndex"] == "2");
                    if q["mediaIndex"] == "2" {
                        assert_eq!(q["partIndex"], "1");
                        assert_eq!(q["offset"], "100.000");
                    }
                    assert_eq!(q["protocol"], "http");
                    assert_eq!(q["directPlay"], "0");
                    assert_eq!(q["directStream"], "0");
                    assert_eq!(q["session"], q["X-Plex-Session-Identifier"]);
                    assert!(q["X-Plex-Client-Profile-Extra"].contains("container=mkv&videoCodec=h264&audioCodec=aac"));
                    let code = if q["offset"] == "1.000" {
                        2000
                    } else {
                        if q["mediaIndex"] == "1" {
                            assert_eq!(q["offset"], "123.456");
                        }
                        1001
                    };
                    (
                        200,
                        serde_json::json!({"MediaContainer":{"generalDecisionCode":code,"generalDecisionText":"fixture-token private"}}),
                    )
                } else if path.starts_with("/video/:/transcode/universal/stop?") {
                    let url = Url::parse(&format!("http://fixture{path}")).unwrap();
                    let session = url.query_pairs().find(|(key, _)| key == "session").unwrap().1.into_owned();
                    assert!(uuid::Uuid::parse_str(&session).is_ok());
                    (404, serde_json::json!({}))
                } else if path.starts_with("/:/scrobble?") || path.starts_with("/:/unscrobble?") {
                    assert!(first.starts_with("PUT "));
                    watched = path.starts_with("/:/scrobble?");
                    (200, serde_json::json!({}))
                } else if path.starts_with("/:/timeline?") {
                    assert!(first.starts_with("POST "));
                    (200, serde_json::json!({}))
                } else {
                    (401, serde_json::json!({"error":"fixture-token is private"}))
                };
                let body = body.to_string();
                write!(stream,"HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            }
        });
        let client = Client::reconnect(&address, Secret::new("fixture-token".into()).unwrap(), "test-client").unwrap();
        let first = client.library_page("1", None).unwrap();
        assert_eq!(first.next.as_deref(), Some("2"));
        let second = client.library_page("1", first.next.as_deref()).unwrap();
        assert!(second.next.is_none());
        let item = first.items[0].clone();
        assert_eq!(item.external_ids.tmdb.as_deref(), Some("123"));
        assert_eq!(item.versions[0].edition.as_deref(), Some("Director's cut"));
        let source = client
            .resolve(PlaybackRequest {
                burn_subtitle: None,
                item: item.id.clone(),
                version_id: "7".into(),
                position_ms: 120000,
                mode: PlaybackMode::Direct,
            })
            .unwrap();
        let url = Url::parse(&source.url).unwrap();
        assert_eq!(url.path(), "/library/parts/8/file.mkv");
        assert_eq!(url.query_pairs().find(|(k, _)| k == "X-Plex-Token").unwrap().1, "fixture-token");
        assert!(source.headers.is_empty());
        client
            .timeline(PlaybackProgress {
                item: item.id.clone(),
                version_id: "7".into(),
                session_id: source.session_id,
                mode: PlaybackMode::Direct,
                state: PlaybackState::Paused,
                position_ms: 120000,
                duration_ms: Some(600000),
            })
            .unwrap();
        let joined = client
            .resolve(PlaybackRequest {
                burn_subtitle: None,
                item: item.id.clone(),
                version_id: "11".into(),
                position_ms: 400000,
                mode: PlaybackMode::Direct,
            })
            .unwrap();
        assert_eq!(Url::parse(&joined.url).unwrap().path(), "/library/parts/13/second.mkv");
        assert!(joined.additional_urls.is_empty());
        assert_eq!(joined.stream_offset_ms, 300000);
        assert_eq!(joined.duration_ms, Some(600000));
        let multi = client
            .resolve(PlaybackRequest {
                item: item.id.clone(),
                version_id: "11".into(),
                position_ms: 400000,
                mode: PlaybackMode::Transcode,
                burn_subtitle: None,
            })
            .unwrap();
        assert_eq!(multi.stream_offset_ms, 400000);
        assert_eq!(multi.part_end_ms, Some(600000));
        let converted = client
            .resolve(PlaybackRequest {
                burn_subtitle: None,
                item: item.id.clone(),
                version_id: "9".into(),
                position_ms: 123456,
                mode: PlaybackMode::Transcode,
            })
            .unwrap();
        assert_eq!(converted.mode, PlaybackMode::Transcode);
        assert_eq!(converted.stream_offset_ms, 123456);
        let converted_url = Url::parse(&converted.url).unwrap();
        assert_eq!(converted_url.path(), "/video/:/transcode/universal/start.mkv");
        let q: std::collections::HashMap<_, _> = converted_url.query_pairs().into_owned().collect();
        assert_eq!(q["X-Plex-Token"], "fixture-token");
        assert_eq!(q["session"], converted.session_id);
        client.stop_transcode(&converted.session_id).unwrap(); // 404 is an already-finished session.
        assert!(client.stop_transcode("not-a-session").is_err());
        let refused = client.resolve(PlaybackRequest {
            burn_subtitle: None,
            item: item.id.clone(),
            version_id: "9".into(),
            position_ms: 1000,
            mode: PlaybackMode::Transcode,
        });
        let error = match refused {
            Err(error) => error,
            Ok(_) => panic!("Conversion should be refused"),
        };
        assert!(!error.to_string().contains("fixture-token"));
        let updated = client.apply_watch(&item.id, &WatchState { position_ms: 0, watched: true, updated_at: None }).unwrap();
        assert!(updated.watch.watched);
        let error = client.get("/unauthorized", &[]).unwrap_err();
        assert_eq!(error, ProviderError::AuthenticationRequired);
        assert!(!error.to_string().contains("fixture-token"));
        server.join().unwrap();
    }
    #[test]
    fn resources_use_server_tokens_and_prefer_local_connections() {
        let resources = serde_json::json!([
            {"name":"Player", "clientIdentifier":"player", "provides":"client", "accessToken":"player-token"},
            {"name":"Shared server", "clientIdentifier":"shared", "provides":"server", "accessToken":"server-token", "connections":[
                {"uri":"https://remote.plex.direct:32400", "local":false, "relay":false},
                {"uri":"http://192.168.1.4:32400", "local":true, "relay":false},
                {"uri":"https://local.plex.direct:32400", "local":true, "relay":false},
                {"uri":"https://relay.plex.direct", "local":false, "relay":true},
                {"uri":"https://user:password@bad.test", "local":true}
            ]},
            {"name":"Secure server", "clientIdentifier":"secure", "provides":"server", "accessToken":"secure-token", "httpsRequired":true,
                "connections":[{"uri":"http://192.168.1.5:32400", "local":true}, {"uri":"https://secure.plex.direct:32400", "local":true}]},
            {"name":"Inaccessible", "clientIdentifier":"no-token", "provides":"server", "connections":[]}
        ]);
        let servers = parse_resources(&resources).unwrap();
        assert_eq!(servers.len(), 2);
        let shared = servers.iter().find(|s| s.id == "shared").unwrap();
        assert_eq!(shared.token.expose(), "server-token");
        let urls: Vec<_> = shared.connections.iter().map(|(_, u)| u.as_str()).collect();
        assert_eq!(
            urls,
            vec![
                "https://local.plex.direct:32400/",
                "http://192.168.1.4:32400/",
                "https://remote.plex.direct:32400/",
                "https://relay.plex.direct/"
            ]
        );
        let secure = servers.iter().find(|s| s.id == "secure").unwrap();
        assert_eq!(secure.connections.len(), 1);
        assert!(secure.connections[0].1.starts_with("https://"));
    }

    #[test]
    fn rejects_credential_urls_foreign_artwork_and_mixed_servers() {
        for url in ["file:///tmp/video", "http://user:token@host/", "https://host/?X-Plex-Token=private", "https://host/#secret"]
        {
            assert!(server_url(url).is_err());
        }
        let server = ServerId { provider: ProviderKind::Plex, id: "server".into() };
        let mut v = movie();
        v["thumb"] = Value::from("https://foreign.test/secret");
        v["art"] = Value::from("/library/1?X-Plex-Token=private");
        let item = parse_item(&v, &server, "1").unwrap();
        assert!(item.poster.is_none());
        assert!(item.backdrop.is_none());
        assert_eq!(format!("{:?}", Secret::new("private".into()).unwrap()), "Secret([redacted])");
        v["Media"][0]["Part"].as_array_mut().unwrap().push(serde_json::json!({"key":"https://foreign.test/part"}));
        let invalid = parse_item(&v, &server, "1").unwrap();
        assert!(invalid.versions[0].parts.is_empty());
        assert!(!relative_key("//foreign.test/file"));
        assert!(!relative_key("/library/file\r\nsecret"));
    }
}

#[cfg(test)]
mod rating_index_tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
        thread,
    };
    #[test]
    fn hydrates_show_rating_elements_omitted_from_library_pages() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            for n in 0..4 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                }
                let body=if n==0{serde_json::json!({"MediaContainer":{"machineIdentifier":"fixture","friendlyName":"Fixture"}})}else if n==1{serde_json::json!({"MediaContainer":{"Directory":[{"key":"1","type":"show","title":"TV"}]}})}else if n==2{assert!(first.contains("includeOptionalElements=Rating"));serde_json::json!({"MediaContainer":{"size":1,"totalSize":1,"Metadata":[{"type":"show","ratingKey":"1","title":"Show","rating":8.0,"ratingImage":"themoviedb://image.rating"}]}})}else{assert!(first.contains("/library/metadata/1,2?"));serde_json::json!({"MediaContainer":{"Metadata":[{"type":"show","ratingKey":"1","title":"Show","Rating":[{"image":"imdb://image.rating","value":9.1}]},{"type":"show","ratingKey":"2","title":"Other","Rating":[{"image":"imdb://image.rating","value":7.2}]}]}})}.to_string();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        let client = Client::connect(&address, Secret::new("fixture-token".into()).unwrap(), "fixture").unwrap();
        let page = client.library_page("1", None).unwrap();
        assert!(!page.items[0].metadata.ratings.iter().any(|r| r.source == "imdb"));
        let mut other = page.items[0].id.clone();
        other.id = "2".into();
        let full = client.rating_details(&[page.items[0].id.clone(), other]).unwrap();
        assert_eq!(full.len(), 2);
        assert_eq!(full[0].metadata.ratings[0].source, "imdb");
        assert_eq!(full[0].metadata.ratings[0].value, 910);
        server.join().unwrap();
    }
}

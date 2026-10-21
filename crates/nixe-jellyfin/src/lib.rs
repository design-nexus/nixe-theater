//! Jellyfin transport. No authenticated URLs, response bodies or tokens in errors.
mod metadata;
pub use nixe_media_model::Secret;
use nixe_media_model::*;
use reqwest::{
    blocking::{Client as Http, Response},
    header::{HeaderMap, HeaderValue},
    redirect,
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    io::Read,
    sync::{Arc, Mutex},
    time::Duration,
};
use url::Url;
type Result<T> = std::result::Result<T, ProviderError>;
const FIELDS: &str = "Overview,ProviderIds,MediaSources,MediaStreams,DateCreated,ParentId,ChildCount,Genres,People,Studios,Taglines,ProductionLocations,OriginalTitle,DateLastMediaAdded";
fn invalid() -> ProviderError {
    ProviderError::InvalidResponse
}
fn id(value: &str) -> Result<&str> {
    uuid::Uuid::parse_str(value).map_err(|_| invalid())?;
    Ok(value)
}
pub fn server_url(value: &str) -> Result<Url> {
    let mut url = Url::parse(value.trim()).map_err(|_| ProviderError::Failed("Enter a valid Jellyfin server address".into()))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ProviderError::Failed("Use an HTTP or HTTPS server address without credentials or query parameters".into()));
    }
    if !url.path().ends_with('/') {
        url.set_path(&format!("{}/", url.path()));
    }
    Ok(url)
}
fn http() -> Result<Http> {
    Http::builder()
        .redirect(redirect::Policy::none())
        .connect_timeout(Duration::from_secs(8))
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| ProviderError::Offline)
}
fn headers(device: &str, token: Option<&Secret>) -> Result<HeaderMap> {
    if device.is_empty() || device.chars().any(|c| c.is_control() || matches!(c, '"' | '\\')) {
        return Err(invalid());
    }
    let mut auth = format!("MediaBrowser Client=\"Theater\", Device=\"Desktop\", DeviceId=\"{device}\", Version=\"0.1.0\"");
    if let Some(token) = token {
        if token.expose().contains(['"', '\\']) {
            return Err(invalid());
        }
        auth.push_str(&format!(", Token=\"{}\"", token.expose()));
    }
    let mut h = HeaderMap::new();
    let mut value = HeaderValue::from_str(&auth).map_err(|_| invalid())?;
    value.set_sensitive(true);
    h.insert("Authorization", value);
    Ok(h)
}
fn response(result: reqwest::Result<Response>) -> Result<Response> {
    let response = result.map_err(|_| ProviderError::Offline)?;
    match response.status().as_u16() {
        200..=299 => Ok(response),
        401 | 403 => Err(ProviderError::AuthenticationRequired),
        404 => Err(ProviderError::NotFound),
        _ => Err(ProviderError::Failed("Jellyfin could not complete the request".into())),
    }
}
fn json_body(response: Response) -> Result<Value> {
    let mut bytes = vec![];
    response.take(16 * 1024 * 1024 + 1).read_to_end(&mut bytes).map_err(|_| invalid())?;
    if bytes.len() > 16 * 1024 * 1024 {
        return Err(invalid());
    }
    serde_json::from_slice(&bytes).map_err(|_| invalid())
}
#[derive(Clone)]
struct SessionPart {
    item: ItemId,
    version: String,
    start: u64,
    duration: u64,
}
#[derive(Clone)]
pub struct Client {
    http: Http,
    base: Url,
    token: Secret,
    device: String,
    user: String,
    server: ServerId,
    pub name: String,
    started: Arc<Mutex<HashSet<String>>>,
    sessions: Arc<Mutex<HashMap<String, SessionPart>>>,
}
impl Client {
    pub fn authenticate(address: &str, username: &str, password: &str, device: &str) -> Result<(Self, Secret)> {
        let base = server_url(address)?;
        let transport = http()?;
        let value = json_body(response(
            transport
                .post(base.join("Users/AuthenticateByName").map_err(|_| invalid())?)
                .headers(headers(device, None)?)
                .json(&json!({"Username":username,"Pw":password}))
                .send(),
        )?)?;
        let token = Secret::new(value["AccessToken"].as_str().ok_or_else(invalid)?.to_owned())?;
        let user = id(value["User"]["Id"].as_str().ok_or_else(invalid)?)?;
        let client = Self::connect(address, token.clone(), user, device)?;
        if value["ServerId"].as_str().is_some_and(|server| Some(server) != client.server.id.split(':').next()) {
            return Err(invalid());
        }
        Ok((client, token))
    }
    pub fn connect(address: &str, token: Secret, user: &str, device: &str) -> Result<Self> {
        let mut client = Self {
            http: http()?,
            base: server_url(address)?,
            token,
            device: device.into(),
            user: id(user)?.into(),
            server: ServerId { provider: ProviderKind::Jellyfin, id: String::new() },
            name: String::new(),
            started: Arc::new(Mutex::new(HashSet::new())),
            sessions: Arc::new(Mutex::new(HashMap::new())),
        };
        let info = client.get("System/Info/Public", &[])?;
        // Jellyfin watch data is per user. Scope cache/client identity to that account.
        client.server.id = format!("{}:{}", id(info["Id"].as_str().ok_or_else(invalid)?)?, client.user);
        client.name = info["ServerName"].as_str().unwrap_or("Jellyfin").into();
        client.list_libraries()?;
        Ok(client)
    }
    pub fn address(&self) -> String {
        self.base.to_string()
    }
    pub fn user_id(&self) -> &str {
        &self.user
    }
    pub fn server_id(&self) -> &ServerId {
        &self.server
    }
    fn url(&self, path: &str) -> Result<Url> {
        if path.contains(['?', '#', '\\']) || path.starts_with('/') || path.contains("..") || path.contains("://") {
            return Err(invalid());
        }
        self.base.join(path).map_err(|_| invalid())
    }
    fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Value> {
        json_body(response(
            self.http.get(self.url(path)?).headers(headers(&self.device, Some(&self.token))?).query(query).send(),
        )?)
    }
    pub fn list_libraries(&self) -> Result<Vec<Library>> {
        let value = self.get("UserViews", &[("userId", self.user.clone())])?;
        let items = value["Items"].as_array().ok_or_else(invalid)?;
        let mut libraries: Vec<Library> = items
            .iter()
            .filter_map(|item| {
                let kind = match item["CollectionType"].as_str()? {
                    "movies" => MediaKind::Movie,
                    "tvshows" => MediaKind::Series,
                    "boxsets" => MediaKind::Collection,
                    _ => return None,
                };
                Some(Library {
                    id: if kind == MediaKind::Collection {
                        format!("collections:{}", id(item["Id"].as_str()?).ok()?)
                    } else {
                        id(item["Id"].as_str()?).ok()?.into()
                    },
                    title: item["Name"].as_str()?.into(),
                    kind,
                })
            })
            .collect();
        if !libraries.iter().any(|library| library.kind == MediaKind::Collection) {
            libraries.push(Library { id: "collections".into(), title: "Collections".into(), kind: MediaKind::Collection });
        }
        Ok(libraries)
    }
    #[allow(clippy::too_many_arguments)]
    fn page(
        &self,
        library: &str,
        parent: Option<&str>,
        types: &str,
        recursive: bool,
        cursor: Option<&str>,
        resume: bool,
        extra: &[(&'static str, String)],
    ) -> Result<Page<MediaItem>> {
        let start = cursor.unwrap_or("0").parse::<usize>().map_err(|_| invalid())?;
        if start >= 50_000 {
            return Err(ProviderError::Failed("Jellyfin library limit reached".into()));
        }
        let mut query = vec![
            ("userId", self.user.clone()),
            ("startIndex", start.to_string()),
            ("limit", "100".into()),
            ("fields", FIELDS.into()),
            ("enableUserData", "true".into()),
            ("includeItemTypes", types.into()),
            ("sortBy", "SortName".into()),
            ("sortOrder", "Ascending".into()),
        ];
        query.push(("recursive", recursive.to_string()));
        if let Some(parent) = parent {
            query.push(("parentId", id(parent)?.into()));
        }
        query.extend(extra.iter().cloned());
        let value = self.get(if resume { "UserItems/Resume" } else { "Items" }, &query)?;
        if value["StartIndex"].as_u64().is_some_and(|offset| offset != start as u64) {
            return Err(invalid());
        }
        let raw = value["Items"].as_array().ok_or_else(invalid)?;
        let end = start.checked_add(raw.len()).ok_or_else(invalid)?;
        let more = !raw.is_empty() && value["TotalRecordCount"].as_u64().map_or(raw.len() >= 100, |total| (end as u64) < total);
        Ok(Page {
            items: raw.iter().filter_map(|item| parse_item(item, &self.server, library)).collect(),
            next: more.then(|| end.to_string()),
        })
    }
    pub fn playlists(&self, cursor: Option<&str>) -> Result<Page<MediaPlaylist>> {
        let start: usize = cursor.unwrap_or("0").parse().map_err(|_| invalid())?;
        if start >= 50000 {
            return Err(invalid());
        }
        let value = self.get(
            "Items",
            &[
                ("userId", self.user.clone()),
                ("includeItemTypes", "Playlist".into()),
                ("recursive", "true".into()),
                ("fields", FIELDS.into()),
                ("startIndex", start.to_string()),
                ("limit", "100".into()),
            ],
        )?;
        let raw = value["Items"].as_array().ok_or_else(invalid)?;
        let end = start + raw.len();
        let items = raw
            .iter()
            .filter(|v| v["MediaType"].as_str().is_none_or(|t| t != "Audio"))
            .filter_map(|v| {
                Some(MediaPlaylist {
                    id: ItemId { server: self.server.clone(), id: id(v["Id"].as_str()?).ok()?.into() },
                    title: v["Name"].as_str()?.into(),
                    count: v["ChildCount"].as_u64(),
                    poster: v["ImageTags"]["Primary"]
                        .as_str()
                        .and_then(|_| v["Id"].as_str())
                        .and_then(|key| id(key).ok())
                        .map(|key| format!("Items/{key}/Images/Primary")),
                })
            })
            .collect();
        let more = !raw.is_empty() && value["TotalRecordCount"].as_u64().map_or(raw.len() >= 100, |n| (end as u64) < n);
        Ok(Page { items, next: more.then(|| end.to_string()) })
    }
    pub fn playlist_items(&self, playlist: &ItemId, cursor: Option<&str>) -> Result<Page<MediaItem>> {
        self.validate(playlist)?;
        let start: usize = cursor.unwrap_or("0").parse().map_err(|_| invalid())?;
        if start >= 50000 {
            return Err(invalid());
        }
        let value = self.get(
            &format!("Playlists/{}/Items", playlist.id),
            &[
                ("userId", self.user.clone()),
                ("fields", FIELDS.into()),
                ("startIndex", start.to_string()),
                ("limit", "100".into()),
                ("enableUserData", "true".into()),
            ],
        )?;
        let raw = value["Items"].as_array().ok_or_else(invalid)?;
        if value["StartIndex"].as_u64().is_some_and(|n| n != start as u64) {
            return Err(invalid());
        }
        let end = start + raw.len();
        let items = raw
            .iter()
            .filter_map(|v| parse_item(v, &self.server, ""))
            .filter(|i| matches!(i.kind, MediaKind::Movie | MediaKind::Episode))
            .collect();
        let more = !raw.is_empty() && value["TotalRecordCount"].as_u64().map_or(raw.len() >= 100, |n| (end as u64) < n);
        Ok(Page { items, next: more.then(|| end.to_string()) })
    }
    pub fn library_page(&self, library: &str, cursor: Option<&str>) -> Result<Page<MediaItem>> {
        self.library_query(library, cursor, &[])
    }
    /// Titles whose metadata (`field` Metadata) or user data (Watched: played, progress,
    /// favourites) the server saved after `since` (Unix seconds).
    pub fn changed_page(&self, library: &str, field: ChangeField, since: i64, cursor: Option<&str>) -> Result<Page<MediaItem>> {
        let key = match field {
            ChangeField::Metadata => "minDateLastSaved",
            ChangeField::Watched => "minDateLastSavedForUser",
        };
        self.library_query(library, cursor, &[(key, iso_time(since))])
    }
    /// How many titles the library has, as the server counts them.
    pub fn count(&self, library: &str) -> Result<u64> {
        let (parent, types) = library_scope(library);
        let mut query = vec![
            ("userId", self.user.clone()),
            ("limit", "0".to_string()),
            ("recursive", "true".into()),
            ("includeItemTypes", types.into()),
        ];
        if let Some(parent) = parent {
            query.push(("parentId", id(parent)?.into()));
        }
        self.get("Items", &query)?["TotalRecordCount"].as_u64().ok_or_else(invalid)
    }
    fn library_query(&self, library: &str, cursor: Option<&str>, extra: &[(&'static str, String)]) -> Result<Page<MediaItem>> {
        let (parent, types) = library_scope(library);
        let mut page = self.page(library, parent, types, true, cursor, false, extra)?;
        for item in &mut page.items {
            if item.versions.iter().any(|version| version.parts.len() > 1) {
                let mut full = self.detail(&item.id)?;
                full.library_id = library.into();
                *item = full;
            }
        }
        Ok(page)
    }
    pub fn child_page(&self, item: &ItemId, library: &str, cursor: Option<&str>) -> Result<Page<MediaItem>> {
        self.validate(item)?;
        self.page(library, Some(&item.id), "Movie,Series,Season,Episode,BoxSet", false, cursor, false, &[])
    }
    pub fn continue_page(&self) -> Result<Page<MediaItem>> {
        self.page("", None, "Movie,Episode", false, None, true, &[])
    }
    fn validate(&self, item: &ItemId) -> Result<()> {
        if item.server != self.server {
            return Err(invalid());
        }
        id(&item.id).map(|_| ())
    }
    pub fn detail(&self, item: &ItemId) -> Result<MediaItem> {
        self.validate(item)?;
        let raw = self.get(&format!("Items/{}", item.id), &[("userId", self.user.clone())])?;
        let mut full = parse_item(&raw, &self.server, "").ok_or_else(invalid)?;
        if raw["LocalTrailerCount"].as_u64().unwrap_or(0) > 0
            && let Ok(trailers) = self.get(&format!("Items/{}/LocalTrailers", item.id), &[("userId", self.user.clone())]) {
                full.metadata.provider_trailers =
                    trailers.as_array().into_iter().flatten().take(8).filter_map(|v| parse_item(v, &self.server, "")).collect();
            }

        if raw["PartCount"].as_u64().unwrap_or(1) > 1 {
            let mut aggregate = None;
            for version in &mut full.versions {
                let parent = if id(&version.id).is_ok() { version.id.as_str() } else { item.id.as_str() };
                let value = self.get(&format!("Videos/{parent}/AdditionalParts"), &[("userId", self.user.clone())])?;
                let parts = value["Items"].as_array().ok_or_else(invalid)?;
                if parts.len() + 1 != raw["PartCount"].as_u64().unwrap_or(1) as usize || parts.len() > 63 {
                    return Err(invalid());
                }
                version.parts = vec![format!("{}/{}", item.id, version.id)];
                version.part_durations_ms = vec![raw["RunTimeTicks"].as_u64().ok_or_else(invalid)? / 10000];
                let mut states = vec![(version.part_durations_ms[0], full.watch.clone())];
                for part in parts {
                    let key = id(part["Id"].as_str().ok_or_else(invalid)?)?;
                    let detail = if part["MediaSources"].as_array().is_some_and(|sources| !sources.is_empty()) {
                        part.clone()
                    } else {
                        self.get(&format!("Items/{key}"), &[("userId", self.user.clone())])?
                    };
                    let parsed = parse_item(&detail, &self.server, "").ok_or_else(invalid)?;
                    let source = parsed.versions.first().ok_or_else(invalid)?;
                    let duration = parsed.duration_ms.ok_or_else(invalid)?;
                    version.parts.push(format!("{key}/{}", source.id));
                    version.part_durations_ms.push(duration);
                    states.push((duration, parsed.watch));
                }
                if aggregate.is_none() {
                    let mut position = 0;
                    let mut watched = true;
                    let mut updated = None;
                    for (duration, watch) in states {
                        updated = updated.max(watch.updated_at);
                        if watched {
                            position += if watch.watched { duration } else { watch.position_ms.min(duration) }
                        }
                        watched &= watch.watched;
                    }
                    aggregate = Some((
                        version.part_durations_ms.iter().sum(),
                        WatchState { position_ms: position, watched, updated_at: updated },
                    ));
                }
            }
            if let Some((duration, watch)) = aggregate {
                full.duration_ms = Some(duration);
                full.watch = watch
            }
        }
        Ok(full)
    }
    pub fn resolve(&self, request: PlaybackRequest) -> Result<PlaybackSource> {
        self.validate(&request.item)?;
        let metadata = self.detail(&request.item)?;
        let version = metadata.versions.iter().find(|version| version.id == request.version_id).ok_or(ProviderError::NotFound)?;
        let durations = if version.parts.len() == 1 {
            vec![metadata.duration_ms.unwrap_or(1).max(1)]
        } else {
            version.part_durations_ms.clone()
        };
        let (index, start, total) = nixe_media_model::parts::locate(&durations, request.position_ms)?;
        let (part_id, source_id) = version.parts.get(index).and_then(|part| part.split_once('/')).ok_or_else(invalid)?;
        let part = PlaybackRequest {
            item: ItemId { server: self.server.clone(), id: part_id.into() },
            version_id: source_id.into(),
            position_ms: request.position_ms.saturating_sub(start),
            mode: request.mode,
            burn_subtitle: request.burn_subtitle,
        };
        let mut result = if request.mode == PlaybackMode::Transcode {
            self.transcode_source(&part)?
        } else {
            let info = self.get(&format!("Items/{part_id}/PlaybackInfo"), &[("userId", self.user.clone())])?;
            let source = info["MediaSources"]
                .as_array()
                .ok_or_else(invalid)?
                .iter()
                .find(|source| source["Id"].as_str() == Some(source_id))
                .ok_or(ProviderError::NotFound)?;
            if source["RequiresOpening"].as_bool() == Some(true)
                || source["IsInfiniteStream"].as_bool() == Some(true)
                || source["RequiredHttpHeaders"].as_object().is_some_and(|headers| !headers.is_empty())
            {
                return Err(ProviderError::Unsupported);
            }
            let session = id(info["PlaySessionId"].as_str().ok_or_else(invalid)?)?.to_owned();
            let mut url = self.url(&format!("Videos/{part_id}/stream"))?;
            url.query_pairs_mut().extend_pairs([
                ("static", "true"),
                ("MediaSourceId", source_id),
                ("DeviceId", self.device.as_str()),
                ("PlaySessionId", session.as_str()),
                ("api_key", self.token.expose()),
            ]);
            PlaybackSource {
                url: url.into(),
                additional_urls: vec![],
                subtitles: self.external_subtitles(source, &part, 0)?,
                headers: vec![],
                session_id: session,
                mode: PlaybackMode::Direct,
                stream_offset_ms: 0,
                duration_ms: None,
                part_end_ms: None,
                burned_subtitle: None,
            }
        };
        result.stream_offset_ms = if request.mode == PlaybackMode::Transcode { request.position_ms } else { start };
        result.duration_ms = Some(total);
        result.part_end_ms = (version.parts.len() > 1).then_some(start + durations[index]);
        self.sessions.lock().map_err(|_| invalid())?.insert(
            result.session_id.clone(),
            SessionPart { item: part.item, version: part.version_id, start, duration: durations[index] },
        );
        Ok(result)
    }
    fn transcode_source(&self, request: &PlaybackRequest) -> Result<PlaybackSource> {
        let ticks = request.position_ms.checked_mul(10_000).ok_or_else(invalid)?;
        // Progressive Matroska keeps authentication on one URL, without HLS subrequests.
        let body = json!({"UserId":self.user,"MediaSourceId":request.version_id,"StartTimeTicks":ticks,
            "EnableDirectPlay":false,"EnableDirectStream":false,"EnableTranscoding":true,
            "AllowVideoStreamCopy":false,"AllowAudioStreamCopy":false,"AutoOpenLiveStream":false,
            "MaxStreamingBitrate":8192000,"MaxAudioChannels":2,"SubtitleStreamIndex":request.burn_subtitle.unwrap_or(-1),"AlwaysBurnInSubtitleWhenTranscoding":request.burn_subtitle.is_some_and(|index|index>=0),
            "DeviceProfile":{"Name":"Theater","MaxStreamingBitrate":8192000,"DirectPlayProfiles":[],
                "TranscodingProfiles":[{"Type":"Video","Container":"mkv","Protocol":"http","VideoCodec":"h264","AudioCodec":"aac","MaxAudioChannels":"2","Context":"Streaming","CopyTimestamps":false,"TranscodeSeekInfo":"Auto"}],
                "CodecProfiles":[{"Type":"Video","Codec":"h264","Conditions":[
                    {"Condition":"LessThanEqual","Property":"Width","Value":"1920","IsRequired":true},
                    {"Condition":"LessThanEqual","Property":"Height","Value":"1080","IsRequired":true}]}],
                "SubtitleProfiles":[{"Format":"srt","Method":"External"},{"Format":"ass","Method":"External"},{"Format":"vtt","Method":"External"},{"Format":"pgssub","Method":"Encode"},{"Format":"dvdsub","Method":"Encode"},{"Format":"dvbsub","Method":"Encode"}]}});
        let info = json_body(response(
            self.http
                .post(self.url(&format!("Items/{}/PlaybackInfo", request.item.id))?)
                .headers(headers(&self.device, Some(&self.token))?)
                .json(&body)
                .send(),
        )?)?;
        let source = info["MediaSources"]
            .as_array()
            .ok_or_else(invalid)?
            .iter()
            .find(|source| source["Id"].as_str() == Some(&request.version_id))
            .ok_or(ProviderError::NotFound)?;
        if source["SupportsTranscoding"].as_bool() != Some(true)
            || source["RequiresOpening"].as_bool() == Some(true)
            || source["IsInfiniteStream"].as_bool() == Some(true)
            || source["RequiredHttpHeaders"].as_object().is_some_and(|headers| !headers.is_empty())
        {
            return Err(ProviderError::Failed(
                "Jellyfin cannot convert this video. Check server permissions and transcoder settings.".into(),
            ));
        }
        let session = id(info["PlaySessionId"].as_str().ok_or_else(invalid)?)?.to_owned();
        let url = self.conversion_url(source["TranscodingUrl"].as_str().ok_or_else(invalid)?, request, &session)?;
        Ok(PlaybackSource {
            url: url.into(),
            additional_urls: vec![],
            subtitles: self.external_subtitles(source, request, request.position_ms)?,
            headers: vec![],
            session_id: session,
            mode: PlaybackMode::Transcode,
            stream_offset_ms: request.position_ms,
            duration_ms: None,
            part_end_ms: None,
            burned_subtitle: request.burn_subtitle.filter(|index| *index >= 0),
        })
    }
    fn conversion_url(&self, value: &str, request: &PlaybackRequest, session: &str) -> Result<Url> {
        if value.chars().any(char::is_control) || value.contains('\\') {
            return Err(invalid());
        }
        let expected = self.url(&format!("Videos/{}/stream.mkv", request.item.id))?;
        let url = if value.starts_with('/') && !value.starts_with(self.base.path()) {
            self.base.join(value.trim_start_matches('/'))
        } else {
            self.base.join(value)
        }
        .map_err(|_| invalid())?;
        if url.origin() != self.base.origin()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || !url.path().eq_ignore_ascii_case(expected.path())
        {
            return Err(invalid());
        }
        // Keep negotiated codec options, replacing authentication and playback identity.
        let mut query: Vec<(String, String)> = url
            .query_pairs()
            .into_owned()
            .filter(|(key, _)| {
                !matches!(
                    key.to_ascii_lowercase().as_str(),
                    "subtitlestreamindex"
                        | "alwaysburninsubtitlewhentranscoding"
                        | "api_key"
                        | "access_token"
                        | "x-emby-token"
                        | "x-mediabrowser-token"
                        | "deviceid"
                        | "playsessionid"
                        | "mediasourceid"
                        | "starttimeticks"
                        | "copytimestamps"
                        | "static"
                        | "maxwidth"
                        | "maxheight"
                        | "videobitrate"
                        | "audiobitrate"
                        | "maxaudiochannels"
                        | "allowvideostreamcopy"
                        | "allowaudiostreamcopy"
                )
            })
            .collect();
        query.push(("SubtitleStreamIndex".into(), request.burn_subtitle.unwrap_or(-1).to_string()));
        query.push((
            "AlwaysBurnInSubtitleWhenTranscoding".into(),
            request.burn_subtitle.is_some_and(|index| index >= 0).to_string(),
        ));
        query.extend(
            [
                ("api_key", self.token.expose().to_owned()),
                ("DeviceId", self.device.clone()),
                ("PlaySessionId", session.into()),
                ("MediaSourceId", request.version_id.clone()),
                ("StartTimeTicks", request.position_ms.checked_mul(10_000).ok_or_else(invalid)?.to_string()),
                ("CopyTimestamps", "false".into()),
                ("Static", "false".into()),
                ("MaxWidth", "1920".into()),
                ("MaxHeight", "1080".into()),
                ("VideoBitrate", "8000000".into()),
                ("AudioBitrate", "192000".into()),
                ("MaxAudioChannels", "2".into()),
                ("AllowVideoStreamCopy", "false".into()),
                ("AllowAudioStreamCopy", "false".into()),
            ]
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value)),
        );
        let mut url = url;
        url.set_query(None);
        url.query_pairs_mut().extend_pairs(query);
        Ok(url)
    }
    fn external_subtitles(&self, source: &Value, request: &PlaybackRequest, offset_ms: u64) -> Result<Vec<ExternalSubtitle>> {
        let mut tracks = vec![];
        let mut selected = false;
        let mut seen = HashSet::new();
        for stream in source["MediaStreams"].as_array().into_iter().flatten().take(128) {
            let bitmap = matches!(
                stream["Codec"].as_str().unwrap_or_default(),
                "pgssub" | "hdmv_pgs_subtitle" | "dvdsub" | "dvbsub" | "vobsub"
            );
            if stream["Type"] != "Subtitle"
                || (!bitmap && stream["IsExternal"].as_bool() != Some(true) && stream["DeliveryMethod"] != "External")
            {
                continue;
            }
            let Some(index) = stream["Index"].as_i64().filter(|index| *index >= 0 && *index <= i32::MAX as i64) else { continue };
            if !seen.insert(index) {
                continue;
            }
            let format = match stream["Codec"].as_str().unwrap_or_default().to_ascii_lowercase().as_str() {
                "ass" | "ssa" => "ass",
                "webvtt" | "vtt" => "vtt",
                "srt" | "subrip" | "mov_text" | "ttml" => "srt",
                _ if bitmap => "bitmap",
                _ => continue,
            };
            let mut url = self.base.clone();
            url.path_segments_mut().map_err(|_| invalid())?.pop_if_empty().extend([
                "Videos",
                request.item.id.as_str(),
                request.version_id.as_str(),
                "Subtitles",
                &index.to_string(),
                &format!("Stream.{format}"),
            ]);
            url.query_pairs_mut().extend_pairs([
                ("api_key", self.token.expose().to_owned()),
                ("startPositionTicks", offset_ms.checked_mul(10_000).ok_or_else(invalid)?.to_string()),
                ("copyTimestamps", "false".into()),
            ]);
            let preferred = source["DefaultSubtitleStreamIndex"].as_i64().map_or_else(
                || stream["IsDefault"].as_bool() == Some(true) || stream["IsForced"].as_bool() == Some(true),
                |default| default == index,
            );
            let pick = preferred && !selected;
            selected |= pick;
            let language = stream["Language"].as_str().unwrap_or_default().to_owned();
            let title = stream["DisplayTitle"]
                .as_str()
                .or(stream["Title"].as_str())
                .filter(|title| !title.is_empty())
                .map(String::from)
                .unwrap_or_else(|| if language.is_empty() { format!("Subtitle {}", tracks.len() + 1) } else { language.clone() });
            tracks.push(ExternalSubtitle {
                id: index,
                url: if bitmap { String::new() } else { url.into() },
                title: if bitmap { format!("{title} · Burn in") } else { title },
                language,
                selected: pick,
                burn_in: bitmap,
            });
        }
        Ok(tracks)
    }
    /// Fetch only on a worker. URLs are generated locally, but validate scope again.
    pub fn subtitle(&self, value: &str) -> Result<String> {
        let url = Url::parse(value).map_err(|_| invalid())?;
        let path = url.path().strip_prefix(self.base.path()).ok_or_else(invalid)?;
        let parts: Vec<_> = path.split('/').collect();
        if url.origin() != self.base.origin()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || parts.len() != 6
            || parts[0] != "Videos"
            || id(parts[1]).is_err()
            || parts[3] != "Subtitles"
            || parts[4].parse::<u32>().is_err()
            || !matches!(parts[5], "Stream.srt" | "Stream.ass" | "Stream.vtt")
        {
            return Err(invalid());
        }
        let response = response(self.http.get(url).headers(headers(&self.device, Some(&self.token))?).send())?;
        let mut bytes = vec![];
        response.take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes).map_err(|_| invalid())?;
        if bytes.len() > 4 * 1024 * 1024 || bytes.contains(&0) {
            return Err(invalid());
        }
        String::from_utf8(bytes).map_err(|_| invalid())
    }
    /// Kill only the conversion belonging to this device and play session.
    pub fn stop_transcode(&self, session: &str) -> Result<()> {
        id(session)?;
        let result = response(
            self.http
                .delete(self.url("Videos/ActiveEncodings")?)
                .headers(headers(&self.device, Some(&self.token))?)
                .query(&[("deviceId", self.device.as_str()), ("playSessionId", session)])
                .send(),
        );
        match result {
            Ok(_) | Err(ProviderError::NotFound) => Ok(()),
            Err(error) => Err(error),
        }
    }
    /// Forget a resolved source that never reached playback, without reporting it watched.
    pub fn forget_session(&self, session: &str) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.remove(session);
        }
        if let Ok(mut started) = self.started.lock() {
            started.remove(session);
        }
    }
    pub fn timeline(&self, progress: PlaybackProgress) -> Result<()> {
        self.validate(&progress.item)?;
        let part = self.sessions.lock().map_err(|_| invalid())?.get(&progress.session_id).cloned();
        let mut progress = progress;
        if let Some(part) = part {
            progress.item = part.item;
            progress.version_id = part.version;
            progress.position_ms = progress.position_ms.saturating_sub(part.start).min(part.duration);
        }
        let body = json!({"ItemId":progress.item.id,"MediaSourceId":progress.version_id,"PlaySessionId":progress.session_id,
            "PositionTicks":progress.position_ms.saturating_mul(10_000),"IsPaused":matches!(progress.state,PlaybackState::Paused),"CanSeek":true,"PlayMethod":if progress.mode==PlaybackMode::Transcode{"Transcode"}else{"DirectPlay"}});
        let started = self.started.lock().map_err(|_| invalid())?.contains(&progress.session_id);
        if !started && !matches!(progress.state, PlaybackState::Stopped) {
            response(
                self.http
                    .post(self.url("Sessions/Playing")?)
                    .headers(headers(&self.device, Some(&self.token))?)
                    .json(&body)
                    .send(),
            )?;
            self.started.lock().map_err(|_| invalid())?.insert(progress.session_id.clone());
        }
        let path = if matches!(progress.state, PlaybackState::Stopped) {
            "Sessions/Playing/Stopped"
        } else {
            "Sessions/Playing/Progress"
        };
        response(self.http.post(self.url(path)?).headers(headers(&self.device, Some(&self.token))?).json(&body).send())?;
        if matches!(progress.state, PlaybackState::Stopped) {
            self.started.lock().map_err(|_| invalid())?.remove(&progress.session_id);
            self.sessions.lock().map_err(|_| invalid())?.remove(&progress.session_id);
        }
        Ok(())
    }
    pub fn apply_watch(&self, item: &ItemId, state: &WatchState) -> Result<MediaItem> {
        self.validate(item)?;
        let current = self.detail(item)?;
        let version = current.versions.first();
        let mut remaining = state.position_ms;
        let parts: Vec<_> = if let Some(version) = version {
            version
                .parts
                .iter()
                .enumerate()
                .map(|(index, part)| {
                    let key = part.split_once('/').map(|(id, _)| id).unwrap_or(&item.id);
                    let duration = version.part_durations_ms.get(index).copied().or(current.duration_ms).unwrap_or(u64::MAX);
                    (key.to_owned(), duration)
                })
                .collect()
        } else {
            vec![(item.id.clone(), current.duration_ms.unwrap_or(u64::MAX))]
        };
        for (key, duration) in parts {
            let watched = state.watched || remaining >= duration;
            let position = if watched { 0 } else { remaining.min(duration) };
            remaining = remaining.saturating_sub(duration);
            let update = response(
                self.http
                    .post(self.url(&format!("UserItems/{key}/UserData"))?)
                    .headers(headers(&self.device, Some(&self.token))?)
                    .query(&[("userId", self.user.as_str())])
                    .json(&json!({"Played":watched,"PlaybackPositionTicks":position.saturating_mul(10000)}))
                    .send(),
            );
            match update {
                Ok(_) => {}
                Err(ProviderError::NotFound) => {
                    // Older Jellyfin releases expose play-state endpoints rather than UserData updates.
                    let path = self.url(&format!("UserPlayedItems/{key}"))?;
                    let request = if watched { self.http.post(path) } else { self.http.delete(path) };
                    response(
                        request
                            .headers(headers(&self.device, Some(&self.token))?)
                            .query(&[("userId", self.user.as_str())])
                            .send(),
                    )?;
                    if !watched && position > 0 {
                        self.timeline(PlaybackProgress {
                            item: ItemId { server: self.server.clone(), id: key },
                            version_id: version.map(|v| v.id.clone()).unwrap_or_default(),
                            session_id: uuid::Uuid::new_v4().to_string(),
                            mode: PlaybackMode::Direct,
                            state: PlaybackState::Stopped,
                            position_ms: position,
                            duration_ms: Some(duration),
                        })?;
                    }
                }
                Err(error) => return Err(error),
            }
        }
        self.detail(item)
    }
    pub fn artwork(&self, key: &str, width: u32, height: u32) -> Result<Vec<u8>> {
        let components: Vec<_> = key.split('/').collect();
        if components.len() != 4
            || components[0] != "Items"
            || id(components[1]).is_err()
            || components[2] != "Images"
            || !matches!(components[3], "Primary" | "Backdrop")
        {
            return Err(invalid());
        }
        let response = response(
            self.http
                .get(self.url(key)?)
                .headers(headers(&self.device, Some(&self.token))?)
                .query(&[("maxWidth", width.clamp(1, 1920)), ("maxHeight", height.clamp(1, 1080))])
                .send(),
        )?;
        if !response.headers().get("content-type").and_then(|v| v.to_str().ok()).is_some_and(|v| v.starts_with("image/")) {
            return Err(invalid());
        }
        let mut bytes = vec![];
        response.take(8 * 1024 * 1024 + 1).read_to_end(&mut bytes).map_err(|_| invalid())?;
        if bytes.len() > 8 * 1024 * 1024 {
            return Err(invalid());
        }
        Ok(bytes)
    }
}
fn date(value: &Value) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value.as_str()?).ok().map(|d| d.timestamp())
}
/// What `Client::changed_page` asks about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeField {
    Metadata,
    Watched,
}

/// A library's parent folder and the item types it lists. The pseudo-library
/// "collections" lists box sets from anywhere.
fn library_scope(library: &str) -> (Option<&str>, &'static str) {
    if library == "collections" || library.starts_with("collections:") {
        (None, "BoxSet")
    } else {
        (Some(library), "Movie,Series,Season,Episode")
    }
}

fn iso_time(secs: i64) -> String {
    chrono::DateTime::from_timestamp(secs.max(0), 0).unwrap_or_default().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

pub fn parse_item(value: &Value, server: &ServerId, library: &str) -> Option<MediaItem> {
    let key = id(value["Id"].as_str()?).ok()?.to_owned();
    let kind = match value["Type"].as_str()? {
        "Movie" | "Trailer" => MediaKind::Movie,
        "Series" => MediaKind::Series,
        "Season" => MediaKind::Season,
        "Episode" => MediaKind::Episode,
        "BoxSet" => MediaKind::Collection,
        _ => return None,
    };
    let related = |name: &str| {
        value[name].as_str().and_then(|value| id(value).ok()).map(|id| ItemId { server: server.clone(), id: id.into() })
    };
    let ticks = |value: &Value| value.as_u64().map(|ticks| ticks / 10_000);
    let versions = value["MediaSources"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|source| {
            let source_id = source["Id"].as_str()?.to_owned();
            if source_id.is_empty() || source_id.len() > 256 || source_id.chars().any(char::is_control) {
                return None;
            }
            let video =
                source["MediaStreams"].as_array().and_then(|streams| streams.iter().find(|stream| stream["Type"] == "Video"));
            Some(MediaVersion {
                id: source_id.clone(),
                edition: source["Name"].as_str().filter(|name| !name.is_empty()).map(String::from),
                parts: if value["PartCount"].as_u64().unwrap_or(1) > 1 {
                    vec![format!("{key}/{source_id}"); value["PartCount"].as_u64().unwrap_or(1).min(64) as usize]
                } else {
                    vec![format!("{key}/{source_id}")]
                },
                part_durations_ms: vec![ticks(&value["RunTimeTicks"]).unwrap_or(0)],
                video_codec: video.and_then(|video| video["Codec"].as_str()).map(String::from),
                container: source["Container"].as_str().map(String::from),
                width: video.and_then(|video| video["Width"].as_u64()).and_then(|v| v.try_into().ok()),
                height: video.and_then(|video| video["Height"].as_u64()).and_then(|v| v.try_into().ok()),
                technical: metadata::technical(source),
            })
        })
        .collect();
    Some(MediaItem {
        id: ItemId { server: server.clone(), id: key.clone() },
        library_id: library.into(),
        kind,
        title: value["Name"].as_str()?.into(),
        tv: TvContext {
            unwatched_episodes: None,
            series_title: value["SeriesName"].as_str().map(String::from),
            season_poster: value["SeasonId"]
                .as_str()
                .and_then(|key| id(key).ok())
                .map(|key| format!("Items/{key}/Images/Primary")),
            series_poster: value["SeriesId"]
                .as_str()
                .and_then(|key| id(key).ok())
                .map(|key| format!("Items/{key}/Images/Primary")),
            episode_count: (kind == MediaKind::Season)
                .then(|| value["ChildCount"].as_u64().and_then(|n| n.try_into().ok()))
                .flatten(),
        },
        metadata: metadata::parse(value),
        summary: value["Overview"].as_str().unwrap_or_default().into(),
        year: value["ProductionYear"].as_u64().and_then(|v| v.try_into().ok()),
        external_ids: ExternalIds {
            tmdb: value["ProviderIds"]["Tmdb"].as_str().map(String::from),
            imdb: value["ProviderIds"]["Imdb"].as_str().map(String::from),
            tvdb: value["ProviderIds"]["Tvdb"].as_str().map(String::from),
        },
        parent: (kind == MediaKind::Episode).then(|| related("SeasonId")).flatten().or_else(|| related("ParentId")),
        series: related("SeriesId").or_else(|| (kind == MediaKind::Season).then(|| related("ParentId")).flatten()),
        season_number: if kind == MediaKind::Season {
            value["IndexNumber"].as_u64()
        } else {
            value["ParentIndexNumber"].as_u64()
        }
        .and_then(|v| v.try_into().ok()),
        episode_number: (kind == MediaKind::Episode)
            .then(|| value["IndexNumber"].as_u64())
            .flatten()
            .and_then(|v| v.try_into().ok()),
        duration_ms: ticks(&value["RunTimeTicks"]),
        added_at: date(&value["DateCreated"]),
        poster: value["ImageTags"]["Primary"].as_str().map(|_| format!("Items/{key}/Images/Primary")),
        backdrop: value["BackdropImageTags"]
            .as_array()
            .filter(|tags| !tags.is_empty())
            .map(|_| format!("Items/{key}/Images/Backdrop")),
        versions,
        watch: WatchState {
            position_ms: ticks(&value["UserData"]["PlaybackPositionTicks"]).unwrap_or_default(),
            watched: value["UserData"]["Played"].as_bool().unwrap_or(false),
            updated_at: date(&value["UserData"]["LastPlayedDate"]),
        },
    })
}
impl MediaProvider for Client {
    fn server(&self) -> &ServerId {
        self.server_id()
    }
    fn libraries(&self) -> ProviderFuture<'_, Vec<Library>> {
        let c = self.clone();
        Box::pin(blocking::unblock(move || c.list_libraries()))
    }
    fn items<'a>(&'a self, library: &'a str, cursor: Option<&'a str>) -> ProviderFuture<'a, Page<MediaItem>> {
        let (c, l, n) = (self.clone(), library.to_owned(), cursor.map(String::from));
        Box::pin(blocking::unblock(move || c.library_page(&l, n.as_deref())))
    }
    fn details<'a>(&'a self, item: &'a ItemId) -> ProviderFuture<'a, MediaItem> {
        let (c, i) = (self.clone(), item.clone());
        Box::pin(blocking::unblock(move || c.detail(&i)))
    }
    fn children<'a>(&'a self, item: &'a ItemId, cursor: Option<&'a str>) -> ProviderFuture<'a, Page<MediaItem>> {
        let (c, i, n) = (self.clone(), item.clone(), cursor.map(String::from));
        Box::pin(blocking::unblock(move || c.child_page(&i, "", n.as_deref())))
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
    const SERVER: &str = "11111111111111111111111111111111";
    const USER: &str = "22222222222222222222222222222222";
    const LIBRARY: &str = "33333333333333333333333333333333";
    const MOVIE: &str = "44444444444444444444444444444444";
    const SESSION: &str = "55555555555555555555555555555555";
    const CONVERTED: &str = "66666666666666666666666666666666";
    fn movie() -> Value {
        json!({"Id":MOVIE,"Type":"Movie","Name":"Fixture","RunTimeTicks":6000000000u64,
        "DateCreated":"2026-01-01T00:00:00Z","ProviderIds":{"Tmdb":"12","Imdb":"tt123"},"ImageTags":{"Primary":"tag"},
        "UserData":{"Played":false,"PlaybackPositionTicks":420000000u64},"MediaSources":[{"Id":"source-1","Name":"Director's cut","Container":"mkv",
        "MediaStreams":[{"Type":"Video","Codec":"h264","Width":1920,"Height":1080}]}]})
    }
    #[test]
    fn video_playlist_paging_preserves_duplicate_entries_and_season_art() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            for _ in 0..6 {
                let (mut stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
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
                let q: HashMap<_, _> = uri.query_pairs().into_owned().collect();
                let value = match uri.path() {
                    "/System/Info/Public" => json!({"Id":SERVER}),
                    "/UserViews" => json!({"Items":[]}),
                    "/Items" => {
                        assert_eq!(q["includeItemTypes"], "Playlist");
                        let offset = q["startIndex"].parse::<usize>().unwrap();
                        json!({"StartIndex":offset,"TotalRecordCount":2,"Items":[{"Id":LIBRARY,"Type":"Playlist","Name":"Queue","MediaType":"Video","ChildCount":2,"ImageTags":{"Primary":"cover"}}]})
                    }
                    p if p == format!("/Playlists/{LIBRARY}/Items") => {
                        assert_eq!(q["userId"], USER);
                        let offset = q["startIndex"].parse::<usize>().unwrap();
                        json!({"StartIndex":offset,"TotalRecordCount":2,"Items":[movie()]})
                    }
                    _ => panic!("Unexpected request"),
                };
                let body = value.to_string();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        let client = Client::connect(&address, Secret::new("fixture-token".into()).unwrap(), USER, "fixture").unwrap();
        let page = client.playlists(None).unwrap();
        assert_eq!(page.items[0].count, Some(2));
        assert_eq!(page.items[0].poster, Some(format!("Items/{LIBRARY}/Images/Primary")));
        assert!(client.playlists(page.next.as_deref()).unwrap().next.is_none());
        let first = client.playlist_items(&page.items[0].id, None).unwrap();
        let second = client.playlist_items(&page.items[0].id, first.next.as_deref()).unwrap();
        assert_eq!(first.items[0].id, second.items[0].id);
        assert!(second.next.is_none());
        let mut item = movie();
        item["Type"] = json!("Episode");
        item["SeriesName"] = json!("Show");
        item["SeriesId"] = json!(SERVER);
        item["SeasonId"] = json!(LIBRARY);
        item["ParentIndexNumber"] = json!(1);
        item["IndexNumber"] = json!(2);
        let episode = parse_item(&item, client.server_id(), "").unwrap();
        assert_eq!(episode.tv.series_title.as_deref(), Some("Show"));
        assert_eq!(nixe_media_model::tv::posters(&episode)[0], format!("Items/{LIBRARY}/Images/Primary"));
        let mut foreign = page.items[0].id.clone();
        foreign.server.id = "another".into();
        assert!(client.playlist_items(&foreign, None).is_err());
        server.join().unwrap();
    }
    #[test]
    fn authenticates_pages_resolves_and_reports_with_scoped_credentials() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}/jf", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let mut watch_update: Option<Value> = None;
            for _ in 0..28 {
                let (mut stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                let mut headers = String::new();
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse::<usize>().unwrap()
                    }
                    headers.push_str(&line);
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let path = first.split_whitespace().nth(1).unwrap();
                assert!(path.starts_with("/jf/"));
                if !path.contains("/Subtitles/") {
                    assert!(!path.contains("fixture-token"));
                }
                let url = Url::parse(&format!("http://fixture{path}")).unwrap();
                let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
                let path = url.path().trim_start_matches("/jf/");
                assert!(headers.contains("DeviceId=\"fixture-device\""));
                if path != "Users/AuthenticateByName" {
                    assert!(headers.contains("Token=\"fixture-token\""))
                }
                let (status, value) = match path {
                    "Users/AuthenticateByName" => {
                        let body: Value = serde_json::from_slice(&body).unwrap();
                        assert_eq!(body["Username"], "ken");
                        assert_eq!(body["Pw"], "private-password");
                        (200, json!({"AccessToken":"fixture-token","User":{"Id":USER},"ServerId":SERVER}))
                    }
                    "System/Info/Public" => (200, json!({"Id":SERVER,"ServerName":"Fixture"})),
                    "UserViews" => {
                        assert_eq!(query["userId"], USER);
                        (200, json!({"Items":[{"Id":LIBRARY,"Name":"Movies","CollectionType":"movies"}]}))
                    }
                    "Items" => {
                        let start = query["startIndex"].parse::<usize>().unwrap();
                        (200, json!({"StartIndex":start,"TotalRecordCount":2,"Items":[movie()]}))
                    }
                    "UserItems/Resume" => (200, json!({"StartIndex":0,"TotalRecordCount":1,"Items":[movie()]})),
                    p if p == format!("Items/{MOVIE}") => {
                        let mut value = movie();
                        if let Some(update) = &watch_update {
                            value["UserData"]["Played"] = update["Played"].clone();
                            value["UserData"]["PlaybackPositionTicks"] = update["PlaybackPositionTicks"].clone();
                        }
                        (200, value)
                    }
                    p if p == format!("UserItems/{MOVIE}/UserData") => {
                        assert!(first.starts_with("POST "));
                        let data: Value = serde_json::from_slice(&body).unwrap();
                        assert_eq!(data["Played"], true);
                        watch_update = Some(data);
                        (200, Value::Null)
                    }
                    p if p == format!("Items/{MOVIE}/PlaybackInfo") => {
                        if first.starts_with("POST ") {
                            let body: Value = serde_json::from_slice(&body).unwrap();
                            assert_eq!(body["MediaSourceId"], "source-1");
                            assert_eq!(body["EnableDirectPlay"], false);
                            assert_eq!(body["EnableDirectStream"], false);
                            assert_eq!(body["AllowVideoStreamCopy"], false);
                            assert_eq!(body["DeviceProfile"]["TranscodingProfiles"][0]["Protocol"], "http");
                            assert_eq!(body["DeviceProfile"]["TranscodingProfiles"][0]["Container"], "mkv");
                            assert_eq!(body["DeviceProfile"]["TranscodingProfiles"][0]["VideoCodec"], "h264");
                            assert_eq!(body["DeviceProfile"]["CodecProfiles"][0]["Conditions"][1]["Value"], "1080");
                            let allowed = body["StartTimeTicks"] == 1234560000u64;
                            assert!(allowed || body["StartTimeTicks"] == 10000u64);
                            assert_eq!(body["SubtitleStreamIndex"], if allowed { 4 } else { -1 });
                            assert_eq!(body["AlwaysBurnInSubtitleWhenTranscoding"], allowed);
                            (
                                200,
                                json!({"PlaySessionId":CONVERTED,"MediaSources":[{"Id":"source-1","SupportsTranscoding":allowed,
                                "DefaultSubtitleStreamIndex":3,"MediaStreams":[{"Type":"Subtitle","Index":3,"Codec":"subrip","IsExternal":true,"Language":"eng","DisplayTitle":"English","DeliveryUrl":"https://foreign.test/private"}],
                                "TranscodingUrl":format!("/videos/{MOVIE}/stream.mkv?api_key=stale&PlaySessionId=wrong&StartTimeTicks=0&VideoCodec=h264&AudioCodec=aac&Tag=kept"),"Error":"fixture-token private-password"}]}),
                            )
                        } else {
                            (
                                200,
                                json!({"PlaySessionId":SESSION,"MediaSources":[{"Id":"source-1","RequiresOpening":false,"DefaultSubtitleStreamIndex":3,"MediaStreams":[
                                {"Type":"Subtitle","Index":3,"Codec":"subrip","IsExternal":true,"Language":"eng","DisplayTitle":"English","DeliveryUrl":"https://foreign.test/private"},
                                {"Type":"Subtitle","Index":4,"Codec":"pgssub","IsExternal":true}]}]}),
                            )
                        }
                    }
                    p if p == format!("Videos/{MOVIE}/source-1/Subtitles/3/Stream.srt") => {
                        assert_eq!(query["api_key"], "fixture-token");
                        assert_eq!(query["copyTimestamps"], "false");
                        assert!(matches!(query["startPositionTicks"].as_str(), "0" | "1234560000"));
                        (200, json!("1\n00:00:00,000 --> 00:00:01,000\nEnglish subtitle\n"))
                    }
                    "Videos/ActiveEncodings" => {
                        assert!(first.starts_with("DELETE "));
                        assert_eq!(query["deviceId"], "fixture-device");
                        assert_eq!(query["playSessionId"], CONVERTED);
                        (404, Value::Null)
                    }
                    "Sessions/Playing" | "Sessions/Playing/Progress" | "Sessions/Playing/Stopped" => {
                        assert!(first.starts_with("POST "));
                        let body: Value = serde_json::from_slice(&body).unwrap();
                        assert_eq!(body["ItemId"], MOVIE);
                        assert_eq!(body["MediaSourceId"], "source-1");
                        if body["PlaySessionId"] == CONVERTED {
                            assert_eq!(body["PositionTicks"], 1234560000u64);
                            assert_eq!(body["PlayMethod"], "Transcode");
                        } else {
                            assert_eq!(body["PlaySessionId"], SESSION);
                            assert_eq!(body["PositionTicks"], 420000000u64);
                            assert_eq!(body["PlayMethod"], "DirectPlay");
                        }
                        (204, Value::Null)
                    }
                    _ => (401, json!({"error":"fixture-token private-password"})),
                };
                let body = if status == 204 {
                    String::new()
                } else if path.contains("/Subtitles/") {
                    value.as_str().unwrap().to_owned()
                } else {
                    value.to_string()
                };
                write!(stream,"HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            }
        });
        let (client, secret) = Client::authenticate(&address, "ken", "private-password", "fixture-device").unwrap();
        assert_eq!(format!("{secret:?}"), "Secret([redacted])");
        let libs = client.list_libraries().unwrap();
        assert_eq!(libs.len(), 2);
        assert!(libs.iter().any(|library| library.kind == MediaKind::Collection));
        let page = client.library_page(LIBRARY, None).unwrap();
        assert_eq!(page.next.as_deref(), Some("1"));
        assert!(client.library_page(LIBRARY, page.next.as_deref()).unwrap().next.is_none());
        let movie = page.items[0].clone();
        assert_eq!(movie.watch.position_ms, 42000);
        assert_eq!(movie.duration_ms, Some(600000));
        assert_eq!(movie.external_ids.tmdb.as_deref(), Some("12"));
        assert_eq!(movie.versions[0].edition.as_deref(), Some("Director's cut"));
        let source = client
            .resolve(PlaybackRequest {
                burn_subtitle: None,
                item: movie.id.clone(),
                version_id: "source-1".into(),
                position_ms: 42000,
                mode: PlaybackMode::Direct,
            })
            .unwrap();
        let url = Url::parse(&source.url).unwrap();
        assert_eq!(url.path(), format!("/jf/Videos/{MOVIE}/stream"));
        let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(query["api_key"], "fixture-token");
        assert_eq!(query["MediaSourceId"], "source-1");
        assert!(source.headers.is_empty());
        assert_eq!(source.subtitles.len(), 2);
        assert!(source.subtitles[1].burn_in);
        let track = &source.subtitles[0];
        assert_eq!(track.id, 3);
        assert_eq!(track.title, "English");
        assert!(track.selected);
        let subtitle_url = Url::parse(&track.url).unwrap();
        assert_eq!(subtitle_url.origin(), url.origin());
        assert!(!track.url.contains("foreign.test"));
        assert!(client.subtitle(&track.url).unwrap().contains("English subtitle"));
        assert!(client.subtitle("https://foreign.test/Videos/private").is_err());
        for state in [PlaybackState::Playing, PlaybackState::Paused, PlaybackState::Stopped] {
            client
                .timeline(PlaybackProgress {
                    item: movie.id.clone(),
                    version_id: "source-1".into(),
                    session_id: source.session_id.clone(),
                    mode: PlaybackMode::Direct,
                    state,
                    position_ms: 42000,
                    duration_ms: Some(600000),
                })
                .unwrap()
        }
        assert_eq!(client.get("denied", &[]).unwrap_err(), ProviderError::AuthenticationRequired);
        assert_eq!(client.continue_page().unwrap().items.len(), 1);
        assert_eq!(client.child_page(&movie.id, LIBRARY, None).unwrap().items.len(), 1);
        let request = PlaybackRequest {
            burn_subtitle: None,
            item: movie.id.clone(),
            version_id: "source-1".into(),
            position_ms: 123456,
            mode: PlaybackMode::Transcode,
        };
        let converted = client
            .resolve(PlaybackRequest {
                burn_subtitle: Some(4),
                item: request.item.clone(),
                version_id: request.version_id.clone(),
                position_ms: request.position_ms,
                mode: request.mode,
            })
            .unwrap();
        assert_eq!(converted.mode, PlaybackMode::Transcode);
        assert_eq!(converted.burned_subtitle, Some(4));
        assert_eq!(converted.stream_offset_ms, 123456);
        let url = Url::parse(&converted.url).unwrap();
        assert_eq!(url.path(), format!("/jf/videos/{MOVIE}/stream.mkv"));
        let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(query["api_key"], "fixture-token");
        assert_eq!(query["StartTimeTicks"], "1234560000");
        assert_eq!(query["PlaySessionId"], CONVERTED);
        assert_eq!(query["CopyTimestamps"], "false");
        assert_eq!(query["Static"], "false");
        assert_eq!(query["MaxHeight"], "1080");
        assert_eq!(query["Tag"], "kept");
        assert_eq!(converted.subtitles.len(), 1);
        assert!(client.subtitle(&converted.subtitles[0].url).unwrap().contains("English subtitle"));
        for state in [PlaybackState::Playing, PlaybackState::Stopped] {
            client
                .timeline(PlaybackProgress {
                    item: movie.id.clone(),
                    version_id: "source-1".into(),
                    session_id: converted.session_id.clone(),
                    mode: PlaybackMode::Transcode,
                    state,
                    position_ms: 123456,
                    duration_ms: Some(600000),
                })
                .unwrap()
        }
        client.stop_transcode(&converted.session_id).unwrap();
        assert!(client.stop_transcode("not-a-session").is_err());
        let denied = client.resolve(PlaybackRequest {
            burn_subtitle: None,
            item: movie.id.clone(),
            version_id: "source-1".into(),
            position_ms: 1,
            mode: PlaybackMode::Transcode,
        });
        let Err(error) = denied else { panic!("Conversion should be denied") };
        assert!(!error.to_string().contains("fixture-token"));
        for value in [
            format!("https://foreign.test/videos/{MOVIE}/stream.mkv"),
            format!("//foreign.test/videos/{MOVIE}/stream.mkv"),
            format!("/videos/{MOVIE}/master.m3u8"),
            format!("/videos/{USER}/stream.mkv"),
            format!("/videos/{MOVIE}/stream.mkv#private"),
        ] {
            assert!(client.conversion_url(&value, &request, CONVERTED).is_err())
        }
        let updated = client.apply_watch(&movie.id, &WatchState { position_ms: 0, watched: true, updated_at: None }).unwrap();
        assert!(updated.watch.watched);
        let mut foreign = movie.id;
        foreign.server.provider = ProviderKind::Plex;
        assert!(client.detail(&foreign).is_err());
        assert!(client.artwork("https://foreign.test/secret", 100, 100).is_err());
        server.join().unwrap();
    }
    #[test]
    fn multipart_offsets_and_progress_target_the_actual_part() {
        const PART: &str = "77777777777777777777777777777777";
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            for _ in 0..8 {
                let (mut stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse::<usize>().unwrap();
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let path = first.split_whitespace().nth(1).unwrap().split('?').next().unwrap();
                let value = match path {
                    "/System/Info/Public" => json!({"Id":SERVER}),
                    "/UserViews" => json!({"Items":[]}),
                    p if p == format!("/Items/{MOVIE}") => {
                        let mut item = movie();
                        item["PartCount"] = json!(2);
                        item["RunTimeTicks"] = json!(3000000000u64);
                        item
                    }
                    p if p == format!("/Videos/{MOVIE}/AdditionalParts") => {
                        let mut item = movie();
                        item["Id"] = json!(PART);
                        item["RunTimeTicks"] = json!(2000000000u64);
                        item["MediaSources"][0]["Id"] = json!("source-2");
                        json!({"Items":[item]})
                    }
                    p if p == format!("/Items/{PART}/PlaybackInfo") => {
                        json!({"PlaySessionId":SESSION,"MediaSources":[{"Id":"source-2"}]})
                    }
                    "/Sessions/Playing" | "/Sessions/Playing/Progress" | "/Sessions/Playing/Stopped" => {
                        let body: Value = serde_json::from_slice(&body).unwrap();
                        assert_eq!(body["ItemId"], PART);
                        assert_eq!(body["MediaSourceId"], "source-2");
                        assert_eq!(body["PositionTicks"], 1000000000u64);
                        Value::Null
                    }
                    _ => panic!("Unexpected request {first}"),
                };
                let body = value.to_string();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        let client = Client::connect(&address, Secret::new("fixture-token".into()).unwrap(), USER, "fixture").unwrap();
        let item = ItemId { server: client.server_id().clone(), id: MOVIE.into() };
        let source = client
            .resolve(PlaybackRequest {
                item: item.clone(),
                version_id: "source-1".into(),
                position_ms: 400000,
                mode: PlaybackMode::Direct,
                burn_subtitle: None,
            })
            .unwrap();
        assert!(source.url.contains(&format!("/Videos/{PART}/stream")));
        assert_eq!(source.stream_offset_ms, 300000);
        assert_eq!(source.duration_ms, Some(500000));
        assert_eq!(source.part_end_ms, Some(500000));
        for state in [PlaybackState::Playing, PlaybackState::Stopped] {
            client
                .timeline(PlaybackProgress {
                    item: item.clone(),
                    version_id: "source-1".into(),
                    session_id: source.session_id.clone(),
                    mode: PlaybackMode::Direct,
                    state,
                    position_ms: 400000,
                    duration_ms: Some(500000),
                })
                .unwrap();
        }
        assert!(client.sessions.lock().unwrap().is_empty());
        server.join().unwrap();
    }
    #[test]
    fn change_queries_use_utc_dates_and_the_library_scope() {
        assert_eq!(iso_time(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(iso_time(-1), "1970-01-01T00:00:00Z");
        assert_eq!(library_scope("collections"), (None, "BoxSet"));
        assert_eq!(library_scope("abc"), (Some("abc"), "Movie,Series,Season,Episode"));
    }
    #[test]
    fn rejects_credential_addresses_and_incomplete_multipart_metadata() {
        for address in
            ["file:///tmp/video", "http://user:password@host/", "https://host/?api_key=private", "https://host/#private"]
        {
            assert!(server_url(address).is_err())
        }
        assert_eq!(server_url("https://host/jellyfin").unwrap().path(), "/jellyfin/");
        let server = ServerId { provider: ProviderKind::Jellyfin, id: SERVER.into() };
        let mut value = movie();
        value["PartCount"] = json!(2);
        assert_eq!(parse_item(&value, &server, LIBRARY).unwrap().versions[0].parts.len(), 2);
    }
}

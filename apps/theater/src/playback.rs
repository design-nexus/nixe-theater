//! Theater's playback policy; rendering and client FFI live in nixe-playback.
use gtk::{glib, prelude::*};
use nixe_media_model::{MediaItem, PlaybackMode, PlaybackProgress, PlaybackRequest, PlaybackSource, PlaybackState};
use nixe_playback::{
    mpv::{Data, EndReason, Event, FORMAT_DOUBLE, FORMAT_FLAG, FORMAT_NODE, Mpv},
    video,
};
use std::{cell::RefCell, rc::Rc, time::Instant};

type Failure = Rc<dyn Fn(&str)>;
type ReportingHold = RefCell<Option<gtk::gio::ApplicationHoldGuard>>;
thread_local! { static REPORTING_HOLDS: RefCell<Vec<std::rc::Weak<ReportingHold>>> = const { RefCell::new(Vec::new()) }; }
fn release_after<T: 'static>(holds: Vec<Rc<RefCell<Option<T>>>>, delay: std::time::Duration) {
    if !holds.is_empty() {
        glib::timeout_add_local_once(delay, move || {
            for hold in holds {
                hold.borrow_mut().take();
            }
        });
    }
}
pub fn finish_reporting_on_close() {
    let holds = REPORTING_HOLDS.with(|holds| holds.borrow_mut().drain(..).filter_map(|hold| hold.upgrade()).collect());
    release_after(holds, std::time::Duration::from_secs(2));
}

#[derive(Default)]
pub struct Playback {
    mpv: Option<Mpv>,
    owner: std::rc::Weak<RefCell<Self>>,
    window: glib::WeakRef<gtk::ApplicationWindow>,
    failure: Option<Failure>,
    duration: f64,
    pub position: f64,
    pub title: String,
    pub loaded: bool,
    pub paused: bool,
    pub tracks: serde_json::Value,
    pub chapters: serde_json::Value,
    remote: Option<Remote>,
    resume_at: Option<f64>,
    stream_offset: f64,
    part_end: Option<f64>,
    burned_subtitle: Option<i64>,
    generation: u64,
    pending: Option<StreamPlan>,
    requested_server: Option<nixe_media_model::ServerId>,
    subtitles: Vec<nixe_media_model::ExternalSubtitle>,
    subtitle_tracks: std::collections::HashMap<i64, i64>,
    subtitle_choice: Option<i64>,
    subtitle_load: u64,
    subtitle_requests: Option<async_channel::Sender<(i64, u64)>>,
    requests: Option<async_channel::Sender<(f64, bool, PlaybackMode)>>,
    /// Where the stream last ended early and was reopened; a second early end there is a
    /// failure, not another retry.
    early_end_at: Option<f64>,
}
impl Playback {
    pub fn attach(window: &gtk::ApplicationWindow, failure: Failure) -> Rc<RefCell<Self>> {
        let (requests, receive) = async_channel::bounded(1);
        let mut state = Self::default();
        state.requests = Some(requests);
        let (subtitle_requests, subtitle_receive) = async_channel::bounded(1);
        state.subtitle_requests = Some(subtitle_requests);
        let player = Rc::new(RefCell::new(state));
        let weak_subtitles = Rc::downgrade(&player);
        glib::spawn_future_local(async move {
            while let Ok((id, generation)) = subtitle_receive.recv().await {
                let Some(player) = weak_subtitles.upgrade() else { break };
                let current = {
                    let p = player.borrow();
                    p.generation == generation && p.subtitle_choice.is_none_or(|choice| choice == id)
                };
                if current {
                    Self::select_external_subtitle(&player, id);
                }
            }
        });
        let weak_requests = Rc::downgrade(&player);
        glib::spawn_future_local(async move {
            while let Ok((position, paused, mode)) = receive.recv().await {
                let Some(player) = weak_requests.upgrade() else { break };
                Self::request_stream(&player, position, paused, mode);
            }
        });
        {
            let mut p = player.borrow_mut();
            p.owner = Rc::downgrade(&player);
            p.window = window.downgrade();
            p.failure = Some(failure);
        }
        player
    }
    fn initialize(&mut self) -> Result<(), String> {
        if self.mpv.is_some() {
            return Ok(());
        }
        let window = self.window.upgrade().ok_or("The playback window was closed")?;
        let player = self.owner.upgrade().ok_or("Playback was closed")?;
        let failure = self.failure.clone().ok_or("Playback is unavailable")?;
        let result = Mpv::new(&[
            ("vo", "libmpv"),
            ("hwdec", "auto-safe"),
            ("idle", "yes"),
            ("keep-open", "yes"),
            ("config", "no"),
            ("terminal", "no"),
            ("input-default-bindings", "no"),
        ]);
        let (mpv, rx) = match result {
            Ok(value) => value,
            Err(error) => {
                return Err(format!("Playback unavailable: {error}"));
            }
        };
        let weak_size = Rc::downgrade(&player);
        if let Err(error) = video::init(&window, &mpv, move || {
            weak_size
                .upgrade()
                .and_then(|p| {
                    let p = p.try_borrow().ok()?;
                    let m = p.mpv.as_ref()?;
                    Some((m.get_i64("dwidth").unwrap_or(1280) as i32, m.get_i64("dheight").unwrap_or(720) as i32))
                })
                .unwrap_or((1280, 720))
        }) {
            return Err(format!("Video rendering unavailable: {error}"));
        }
        mpv.observe("time-pos", FORMAT_DOUBLE);
        mpv.observe("duration", FORMAT_DOUBLE);
        mpv.observe("pause", FORMAT_FLAG);
        mpv.observe("eof-reached", FORMAT_FLAG);
        mpv.observe("track-list", FORMAT_NODE);
        mpv.observe("chapter-list", FORMAT_NODE);
        let settings = crate::prefs::get();
        let _ = mpv.set_f64("volume", settings.volume * 100.0);
        let _ = mpv.set_flag("mute", settings.muted);
        self.mpv = Some(mpv);
        let weak = Rc::downgrade(&player);
        let failure = failure.clone();
        glib::spawn_future_local(async move {
            while rx.recv().await.is_ok() {
                let Some(player) = weak.upgrade() else { break };
                let mut retry = None;
                let mut failed = false;
                let mut completed = false;
                let mut finished_item = None;
                let mut p = player.borrow_mut();
                while let Some(event) = p.mpv.as_ref().and_then(Mpv::next_event) {
                    match event {
                        Event::Property { name, data: Data::Double(value) } if name == "time-pos" => {
                            if p.pending.is_none() {
                                p.position = value + p.stream_offset;
                            }
                        }
                        Event::Property { name, data: Data::Double(value) } if name == "duration" => {
                            if p.part_end.is_none() && p.stream_offset == 0.0 && !p.is_transcoding() || p.duration <= 0.0 {
                                p.duration = value + p.stream_offset;
                            }
                        }
                        Event::FileLoaded if p.pending.is_none() => {
                            p.loaded = true;
                            if let Some(position) = p.resume_at.take() {
                                p.seek(position);
                                p.position = position;
                            }
                            let selected = p
                                .subtitle_choice
                                .or_else(|| p.subtitles.iter().find(|track| track.selected).map(|track| track.id));
                            if let Some(id) = selected {
                                if id >= 0 && p.burned_subtitle != Some(id) {
                                    if let Some(requests) = &p.subtitle_requests {
                                        let _ = requests.try_send((id, p.generation));
                                    }
                                } else if id == -1 {
                                    p.set_string("sid", "no");
                                }
                            }
                            p.report(true);
                        }
                        Event::Property { name, data: Data::Flag(true) } if name == "eof-reached" => {
                            if p.loaded && p.part_end.is_some_and(|end| end + 0.001 < p.duration) {
                                retry = Some((
                                    p.part_end.unwrap(),
                                    p.paused,
                                    p.remote.as_ref().map(|r| r.mode).unwrap_or(PlaybackMode::Direct),
                                ));
                                p.loaded = false;
                            } else if p.loaded && p.duration > 0.0 && p.position >= p.duration - 2.0 {
                                finished_item = p.remote.as_ref().map(|r| r.media.clone());
                                p.finish_remote();
                                p.loaded = false;
                                completed = true;
                            } else if p.loaded && p.duration > 0.0 && p.remote.is_some() {
                                // The stream ended well before the video did (a dropped
                                // connection): reopen it where it stopped, once.
                                let at = p.position;
                                if p.early_end_at.is_some_and(|e| (e - at).abs() < 5.0) {
                                    p.early_end_at = None;
                                    p.finish_remote();
                                    failed = true;
                                } else {
                                    p.early_end_at = Some(at);
                                    retry = Some((at, p.paused, p.remote.as_ref().map(|r| r.mode).unwrap_or(PlaybackMode::Direct)));
                                }
                                p.loaded = false;
                            }
                        }
                        Event::Property { name, data: Data::Flag(value) } if name == "pause" => {
                            p.paused = value;
                            if p.loaded {
                                p.report(true);
                            }
                        }
                        Event::Property { name, data: Data::Node(value) } if name == "track-list" => p.tracks = value,
                        Event::Property { name, data: Data::Node(value) } if name == "chapter-list" => p.chapters = value,
                        Event::EndFile { reason: EndReason::Eof, error: None } if p.loaded => {
                            if p.part_end.is_some_and(|end| end + 0.001 < p.duration) {
                                retry = Some((
                                    p.part_end.unwrap(),
                                    p.paused,
                                    p.remote.as_ref().map(|r| r.mode).unwrap_or(PlaybackMode::Direct),
                                ));
                            } else {
                                finished_item = p.remote.as_ref().map(|r| r.media.clone());
                                p.finish_remote();
                                completed = true;
                            }
                            p.loaded = false;
                        }
                        Event::EndFile { error: Some(_), .. } => {
                            if p.remote.as_ref().is_some_and(|r| r.mode == PlaybackMode::Direct && r.allow_transcode) {
                                retry = Some((p.resume_at.unwrap_or(p.position), p.paused, PlaybackMode::Transcode));
                                p.loaded = false;
                            } else if p.pending.is_none() {
                                p.finish_remote();
                                p.loaded = false;
                                failed = true;
                            }
                        }
                        _ => {}
                    }
                }
                if p.loaded {
                    p.report(false);
                }
                let failed_position = (p.position.max(0.0) * 1000.0) as u64;
                drop(p);
                if completed {
                    if crate::playlists::has_queue() {
                        crate::playlists::advance(1);
                    } else if let Some(item) = finished_item {
                        crate::library::up_next(&item);
                    }
                }
                if failed && !crate::library::try_next_source(failed_position) {
                    failure("Playback failed. Check the server connection and selected playback mode.");
                }
                if let Some((position, paused, mode)) = retry {
                    Self::request_stream(&player, position, paused, mode);
                }
            }
        });
        Ok(())
    }
    pub fn begin_request(&mut self, server: nixe_media_model::ServerId) -> u64 {
        self.requested_server = Some(server);
        self.generation = self.generation.wrapping_add(1);
        self.pending = None;
        self.generation
    }
    pub fn accepts_request(&self, generation: u64) -> bool {
        self.generation == generation
    }
    pub fn open(&mut self, uri: &str) -> Result<(), String> {
        self.initialize()?;
        self.finish_remote();
        crate::window::cancel_up_next();
        self.generation = self.generation.wrapping_add(1);
        self.pending = None;
        self.resume_at = None;
        self.stream_offset = 0.0;
        self.part_end = None;
        self.burned_subtitle = None;
        self.requested_server = None;
        self.subtitles.clear();
        self.subtitle_tracks.clear();
        self.subtitle_choice = None;
        self.subtitle_load = self.subtitle_load.wrapping_add(1);
        let mpv = self.mpv.as_ref().ok_or("Playback is unavailable")?;
        mpv.command(&["loadfile", uri, "replace"])?;
        mpv.set_flag("pause", false)?;
        self.title =
            gtk::gio::File::for_uri(uri).basename().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| "Video".into());
        self.duration = 0.0;
        self.position = 0.0;
        self.loaded = false;
        self.paused = false;
        self.tracks = serde_json::Value::Null;
        self.chapters = serde_json::Value::Null;
        Ok(())
    }
    pub fn open_remote(
        &mut self,
        source: PlaybackSource,
        item: MediaItem,
        client: crate::servers::Client,
        resume_ms: u64,
        version_id: String,
        allow_transcode: bool,
    ) -> Result<(), String> {
        let opened = (|| {
            if !source.headers.is_empty() {
                return Err("Unsupported playback authentication");
            }
            let timeline = nixe_playback::timeline::concatenate(
                std::iter::once(source.url.as_str()).chain(source.additional_urls.iter().map(String::as_str)),
            )
            .ok_or("The server returned an empty playback source")?;
            self.open(&timeline).map_err(|_| "Playback could not start")
        })();
        if let Err(error) = opened {
            client.discard_session(source.session_id, source.mode);
            return Err(error.into());
        }
        self.title = item.title.clone();
        self.duration = source.duration_ms.or(item.duration_ms).unwrap_or_default() as f64 / 1000.0;
        self.part_end = source.part_end_ms.map(|end| end as f64 / 1000.0);
        self.burned_subtitle = source.burned_subtitle;
        self.stream_offset = source.stream_offset_ms as f64 / 1000.0;
        self.position = self.stream_offset;
        self.resume_at = (source.mode == PlaybackMode::Direct && resume_ms > 0)
            .then_some((resume_ms as f64 / 1000.0 - 3.0).max(self.stream_offset));
        self.subtitles = source.subtitles;
        self.remote = Some(Remote::new(client, item, source.session_id, version_id, source.mode, allow_transcode));
        Ok(())
    }
    fn is_transcoding(&self) -> bool {
        self.pending.is_some() || self.remote.as_ref().is_some_and(|r| r.mode == PlaybackMode::Transcode)
    }
    fn request_stream(player: &Rc<RefCell<Self>>, position: f64, paused: bool, mode: PlaybackMode) {
        let prepared = {
            let mut p = player.borrow_mut();
            let plan = p
                .remote
                .as_ref()
                .map(|r| StreamPlan {
                    client: r.client.clone(),
                    item: r.media.clone(),
                    version: r.version.clone(),
                    subtitle_choice: p
                        .subtitle_choice
                        .or_else(|| p.subtitles.iter().find(|track| track.selected).map(|track| track.id)),
                    mode,
                    burn_subtitle: p
                        .subtitle_choice
                        .or_else(|| p.subtitles.iter().find(|track| track.selected).map(|track| track.id))
                        .and_then(|id| p.subtitles.iter().find(|track| track.id == id && track.burn_in).map(|_| id))
                        .or(Some(-1)),
                })
                .or_else(|| p.pending.clone());
            let Some(plan) = plan else { return };
            let position = position.max(0.0).min(if p.duration > 0.5 { p.duration - 0.5 } else { f64::MAX });
            p.finish_remote();
            if let Some(m) = &p.mpv {
                let _ = m.command(&["stop"]);
            }
            p.generation = p.generation.wrapping_add(1);
            p.pending = Some(plan.clone());
            p.loaded = false;
            p.position = position;
            p.resume_at = None;
            (plan, p.generation, position)
        };
        let (plan, generation, position) = prepared;
        crate::window::status(&format!("Preparing {} playback…", plan.client.label()));
        let worker = plan.clone();
        let label = plan.client.label();
        let weak = Rc::downgrade(player);
        nixe_core::cmd::background(
            move || {
                worker.client.resolve(PlaybackRequest {
                    burn_subtitle: worker.burn_subtitle,
                    item: worker.item.id,
                    version_id: worker.version,
                    position_ms: (position * 1000.0) as u64,
                    mode: worker.mode,
                })
            },
            move |result| {
                let Some(player) = weak.upgrade() else {
                    if let Ok(source) = result {
                        plan.client.discard_session(source.session_id, source.mode);
                    }
                    return;
                };
                let mut p = player.borrow_mut();
                if p.generation != generation {
                    if let Ok(source) = result {
                        plan.client.discard_session(source.session_id, source.mode);
                    }
                    return;
                }
                p.pending = None;
                let result = result.map_err(|error| error.to_string()).and_then(|source| {
                    p.open_remote(
                        source,
                        plan.item,
                        plan.client,
                        ((position + 3.0) * 1000.0) as u64,
                        plan.version,
                        plan.mode == PlaybackMode::Direct,
                    )?;
                    p.subtitle_choice = plan.subtitle_choice;
                    if paused {
                        p.set_string("pause", "yes");
                        p.paused = true;
                    }
                    Ok(())
                });
                drop(p);
                match result {
                    Ok(()) => crate::window::status(&format!(
                        "{label} · {}",
                        if plan.mode == PlaybackMode::Transcode { "Transcoding" } else { "Direct Play" }
                    )),
                    Err(error) => {
                        if !crate::library::try_next_source((position.max(0.0) * 1000.0) as u64) {
                            crate::window::status(&error)
                        }
                    }
                }
            },
        );
    }
    fn report(&mut self, force: bool) {
        if let Some(remote) = &mut self.remote
            && (force || remote.reported.elapsed().as_secs() >= 10) {
                remote.send(
                    if self.paused { PlaybackState::Paused } else { PlaybackState::Playing },
                    self.position,
                    self.duration,
                );
            }
    }
    fn finish_remote(&mut self) {
        if let Some(mut remote) = self.remote.take() {
            remote.send(PlaybackState::Stopped, self.resume_at.unwrap_or(self.position), self.duration);
        }
    }
    pub fn subtitle_options(&self) -> Vec<(i64, String, String, bool)> {
        self.subtitles
            .iter()
            .filter(|track| !self.subtitle_tracks.contains_key(&track.id))
            .map(|track| (track.id, track.title.clone(), track.language.clone(), self.burned_subtitle == Some(track.id)))
            .collect()
    }
    pub fn choose_track(&mut self, property: &str, id: &str) {
        if property == "sid" {
            self.subtitle_load = self.subtitle_load.wrapping_add(1);
            self.subtitle_choice = Some(if id == "no" {
                -1
            } else {
                self.subtitle_tracks
                    .iter()
                    .find_map(|(external, loaded)| (Some(*loaded) == id.parse::<i64>().ok()).then_some(*external))
                    .unwrap_or(-2)
            });
        }
        self.set_string(property, id);
        if property == "sid" && self.burned_subtitle.is_some()
            && let Some(requests) = &self.requests {
                let _ = requests.try_send((self.position, self.paused, PlaybackMode::Transcode));
            }
    }
    pub fn subtitles_burned(&self) -> bool {
        self.burned_subtitle.is_some()
    }
    pub fn select_external_subtitle(player: &Rc<RefCell<Self>>, id: i64) {
        let prepared = {
            let mut p = player.borrow_mut();
            let Some(track) = p.subtitles.iter().find(|track| track.id == id).cloned() else { return };
            let Some(remote) = &p.remote else { return };
            let client = remote.client.clone();
            p.subtitle_choice = Some(id);
            p.subtitle_load = p.subtitle_load.wrapping_add(1);
            if track.burn_in || p.burned_subtitle.is_some() {
                if p.burned_subtitle == Some(id) {
                    return;
                }
                if let Some(requests) = &p.requests {
                    let _ = requests.try_send((p.position, p.paused, PlaybackMode::Transcode));
                }
                return;
            }
            if let Some(loaded) = p.subtitle_tracks.get(&id) {
                p.set_string("sid", &loaded.to_string());
                return;
            }
            (track, client, p.generation, p.subtitle_load)
        };
        let (track, client, generation, load) = prepared;
        let url = track.url.clone();
        let weak = Rc::downgrade(player);
        crate::window::status("Loading subtitles…");
        nixe_core::cmd::background(
            move || client.subtitle(&url),
            move |result| {
                let Some(player) = weak.upgrade() else { return };
                let mut p = player.borrow_mut();
                if p.generation != generation || p.subtitle_load != load || p.subtitle_choice != Some(id) {
                    return;
                }
                let result = result.map_err(|error| error.to_string()).and_then(|text| {
                    let mpv = p.mpv.as_ref().ok_or("Playback is unavailable")?;
                    let memory = format!("memory://{text}");
                    mpv.command(&["sub-add", &memory, "select", &track.title, &track.language])
                        .map_err(|_| "Subtitles could not be loaded")?;
                    if let Some(loaded) = mpv.get_i64("sid") {
                        p.subtitle_tracks.insert(id, loaded);
                    }
                    Ok(())
                });
                drop(p);
                match result {
                    Ok(()) => crate::window::status("Subtitles loaded"),
                    Err(error) => crate::window::status(&error),
                }
            },
        );
    }
    pub fn toggle(&self) {
        if let Some(mpv) = &self.mpv {
            let _ = mpv.command(&["cycle", "pause"]);
        }
    }
    pub fn seek_by(&self, delta: f64) {
        if self.remote.is_some() || self.pending.is_some() {
            self.seek(self.position + delta);
            return;
        }
        if let Some(mpv) = &self.mpv {
            let _ = mpv.command(&["seek", &delta.to_string(), "relative"]);
        }
    }
    pub fn seek(&self, position: f64) {
        if self.is_transcoding() || self.part_end.is_some_and(|end| position < self.stream_offset || position >= end) {
            let mode = self
                .remote
                .as_ref()
                .map(|r| r.mode)
                .or_else(|| self.pending.as_ref().map(|p| p.mode))
                .unwrap_or(PlaybackMode::Direct);
            if let Some(requests) = &self.requests {
                let _ = requests.try_send((position, self.paused, mode));
            }
            return;
        }
        if let Some(mpv) = &self.mpv {
            let _ = mpv.command(&["seek", &(position - self.stream_offset).max(0.0).to_string(), "absolute+exact"]);
        }
    }
    pub fn number(&self, name: &str) -> f64 {
        self.mpv.as_ref().and_then(|m| m.get_f64(name)).unwrap_or_default()
    }
    pub fn set_number(&self, name: &str, value: f64) {
        if let Some(m) = &self.mpv {
            let _ = m.set_f64(name, value);
        }
    }
    pub fn set_string(&self, name: &str, value: &str) {
        if let Some(m) = &self.mpv {
            let _ = m.set_str(name, value);
        }
    }
    pub fn seek_fast(&self, position: f64) {
        // The shared seek bar commits an exact seek after the drag settles.
        if self.is_transcoding() || self.part_end.is_some_and(|end| position < self.stream_offset || position >= end) {
            return;
        }
        if let Some(m) = &self.mpv {
            let _ = m.command(&["seek", &(position - self.stream_offset).max(0.0).to_string(), "absolute+keyframes"]);
        }
    }
    pub fn set_volume(&self, value: f64) {
        let value = value.clamp(0.0, 1.0);
        crate::prefs::update(|p| {
            p.volume = value;
            if value > 0.0 {
                p.muted = false;
            }
        });
        self.set_number("volume", value * 100.0);
        self.set_string("mute", if crate::prefs::get().muted { "yes" } else { "no" });
    }
    pub fn toggle_mute(&self) {
        crate::prefs::update(|p| p.muted = !p.muted);
        self.set_string("mute", if crate::prefs::get().muted { "yes" } else { "no" });
    }
    pub fn uses_server(&self, server: &nixe_media_model::ServerId) -> bool {
        self.remote.as_ref().is_some_and(|remote| remote.media.id.server == *server)
            || self.pending.as_ref().is_some_and(|plan| plan.item.id.server == *server)
            || self.requested_server.as_ref() == Some(server)
    }
    pub fn cancel_server(&mut self, server: &nixe_media_model::ServerId) {
        if self.remote.as_ref().is_some_and(|remote| remote.media.id.server == *server) {
            self.close();
            return;
        }
        if self.requested_server.as_ref() == Some(server)
            || self.pending.as_ref().is_some_and(|plan| plan.item.id.server == *server)
        {
            self.generation = self.generation.wrapping_add(1);
            self.pending = None;
            self.requested_server = None;
        }
    }
    pub fn has_video(&self) -> bool {
        !self.title.is_empty() && (self.loaded || self.remote.is_some() || self.pending.is_some())
    }
    pub fn close(&mut self) {
        crate::playlists::clear_queue();
        self.finish_remote();
        self.generation = self.generation.wrapping_add(1);
        self.pending = None;
        self.resume_at = None;
        self.requested_server = None;
        self.subtitles.clear();
        self.subtitle_tracks.clear();
        self.subtitle_choice = None;
        self.subtitle_load = self.subtitle_load.wrapping_add(1);
        if let Some(m) = &self.mpv {
            let _ = m.command(&["stop"]);
        }
        video::clear();
        self.title.clear();
        self.loaded = false;
        self.position = 0.0;
        self.duration = 0.0;
    }
    pub fn duration(&self) -> f64 {
        self.duration
    }
    pub fn shutdown(&mut self) {
        self.finish_remote();
        self.generation = self.generation.wrapping_add(1);
        self.pending = None;
        video::shutdown();
        if let Some(mpv) = self.mpv.take() {
            mpv.destroy_later();
        }
    }
}
impl Drop for Playback {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[derive(Clone)]
struct StreamPlan {
    client: crate::servers::Client,
    item: MediaItem,
    version: String,
    subtitle_choice: Option<i64>,
    mode: PlaybackMode,
    burn_subtitle: Option<i64>,
}

struct Remote {
    client: crate::servers::Client,
    media: MediaItem,
    version: String,
    mode: PlaybackMode,
    allow_transcode: bool,
    session: String,
    tx: std::sync::mpsc::Sender<(PlaybackProgress, MediaItem, i64)>,
    reported: Instant,
}
impl Remote {
    fn new(
        client: crate::servers::Client,
        item: MediaItem,
        session: String,
        version: String,
        mode: PlaybackMode,
        allow_transcode: bool,
    ) -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<(PlaybackProgress, MediaItem, i64)>();
        let (errors, notices) = async_channel::unbounded::<Result<(MediaItem, i64), String>>();
        let reporting_client = client.clone();
        let cleanup_session = session.clone();
        std::thread::spawn(move || {
            let mut warned = false;
            let cache = crate::servers::database().ok();
            for (progress, item, revision) in rx {
                if cache.as_ref().is_none_or(|cache| cache.record_playback(&item, revision).is_err()) && !warned {
                    warned = true;
                    let _ = errors.try_send(Err("Playback progress could not be saved to the local cache.".into()));
                }
                let stopped = matches!(progress.state, PlaybackState::Stopped);
                let id = progress.item.clone();
                match reporting_client.timeline(progress) {
                    Ok(()) if stopped => {
                        // The provider owns completion thresholds and watched status.
                        if let Ok(mut confirmed) = reporting_client.detail(&id) {
                            if confirmed.library_id.is_empty() {
                                confirmed.library_id = item.library_id.clone()
                            }
                            let item = confirmed;
                            if let Some(cache) = &cache {
                                let _ = cache.record_playback(&item, revision);
                            }
                            let _ = errors.try_send(Ok((item, revision)));
                        }
                    }
                    Err(_) if !warned => {
                        warned = true;
                        let _ = errors.try_send(Err(format!(
                            "{} progress could not be saved. Check the server connection.",
                            reporting_client.label()
                        )));
                    }
                    _ => {}
                }
            }
            if mode == PlaybackMode::Transcode && reporting_client.stop_transcode(&cleanup_session).is_err() {
                let _ = errors.try_send(Err("Could not stop the conversion session. Check the server connection.".into()));
            }
        });
        let app = gtk::gio::Application::default();
        let hold = Rc::new(RefCell::new(app.as_ref().map(|app| app.hold())));
        REPORTING_HOLDS.with(|holds| {
            let mut holds = holds.borrow_mut();
            holds.retain(|hold| hold.strong_count() > 0);
            holds.push(Rc::downgrade(&hold));
        });
        glib::spawn_future_local(async move {
            while let Ok(message) = notices.recv().await {
                if crate::window::is_closing() {
                    continue;
                }
                match message {
                    Ok((item, revision)) => crate::servers::playback_watch(item, revision),
                    Err(message) => crate::window::status(&message),
                }
            }
            // Keep the main loop alive until the final stop report has completed.
            hold.borrow_mut().take();
        });
        Self { client, media: item, version, mode, allow_transcode, session, tx, reported: Instant::now() }
    }
    fn send(&mut self, state: PlaybackState, position: f64, duration: f64) {
        let revision = crate::servers::revision();
        self.media.watch.position_ms = (position.max(0.0) * 1000.0) as u64;
        self.media.watch.updated_at = Some(revision / 1_000_000_000);
        crate::servers::playback_watch(self.media.clone(), revision);
        let _ = self.tx.send((
            PlaybackProgress {
                item: self.media.id.clone(),
                version_id: self.version.clone(),
                session_id: self.session.clone(),
                mode: self.mode,
                state,
                position_ms: (position.max(0.0) * 1000.0) as u64,
                duration_ms: (duration > 0.0).then_some((duration * 1000.0) as u64),
            },
            self.media.clone(),
            revision,
        ));
        self.reported = Instant::now();
    }
}

#[cfg(test)]
mod shutdown_tests {
    use super::*;
    #[test]
    fn final_reporting_hold_expires_without_a_server_reply() {
        let context = glib::MainContext::default();
        let _owner = context.acquire().unwrap();
        let hold = Rc::new(RefCell::new(Some(())));
        release_after(vec![hold.clone()], std::time::Duration::from_millis(10));
        let started = Instant::now();
        while hold.borrow().is_some() && started.elapsed().as_secs() < 1 {
            context.iteration(true);
        }
        assert!(hold.borrow().is_none());
    }
}

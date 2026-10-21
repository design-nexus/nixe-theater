//! One Plex server, worker-owned network/cache I/O and generation-checked UI updates.
use crate::{
    credentials,
    prefs::{self, PlexSettings},
    window,
};
use gtk::{gio, glib, prelude::*};
use nixe_media_model::{MediaItem, MediaKind, ProviderError};
use nixe_plex::{Client, Secret};
use std::{
    cell::RefCell,
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
#[derive(Default)]
struct State {
    client: Option<Client>,
    catalog: crate::servers::Catalog,
    generation: u64,
    cancelled: Arc<AtomicBool>,
    busy: bool,
    error: Option<String>,
    /// Libraries the last refresh couldn't load (their cached titles were kept).
    skipped: Vec<String>,
}
impl std::ops::Deref for State {
    type Target = crate::servers::Catalog;
    fn deref(&self) -> &Self::Target {
        &self.catalog
    }
}
impl std::ops::DerefMut for State {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.catalog
    }
}
thread_local! { static STATE: RefCell<State> = RefCell::new(State::default()); }
/// Shared ordering for snapshots, reports and delayed server confirmations.
pub fn revision() -> i64 {
    static LAST: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
    let now = i64::try_from(now).unwrap_or(i64::MAX - 1);
    let old = LAST.try_update(Ordering::Relaxed, Ordering::Relaxed, |old| Some(now.max(old + 1))).unwrap();
    now.max(old + 1)
}
pub fn latest_watch(item: MediaItem) -> MediaItem {
    STATE.with(|s| s.borrow().catalog.latest(item))
}
pub fn details_watch(item: &MediaItem, children: &[MediaItem], started: i64) {
    STATE.with(|s| s.borrow_mut().catalog.confirmed(item, children, started))
}
pub fn playback_watch(item: MediaItem, revision: i64) {
    if !prefs::get().plex.is_some_and(|p| p.server == item.id.server) {
        return;
    }
    if STATE.with(|s| s.borrow_mut().catalog.record(item, revision)) {
        crate::library::watch_changed()
    }
}
pub fn client() -> Option<Client> {
    STATE.with(|s| s.borrow().client.clone())
}
pub fn items() -> Vec<MediaItem> {
    STATE.with(|s| s.borrow().items.clone())
}
pub fn connection_state() -> crate::servers::ConnectionState {
    STATE.with(|s| {
        let s = s.borrow();
        let connected = s.client.is_some();
        let message = if let Some(error) = &s.error {
            format!("Plex: {error}")
        } else if !s.skipped.is_empty() {
            format!("Plex connected · couldn't refresh {}", s.skipped.join(", "))
        } else if connected && s.busy {
            "Plex connected · loading library…".into()
        } else if connected {
            "Plex connected".into()
        } else if s.busy {
            "Connecting to Plex…".into()
        } else {
            "Plex is offline · cached titles remain available".into()
        };
        crate::servers::ConnectionState { provider: nixe_media_model::ProviderKind::Plex, connected, busy: s.busy, message }
    })
}
fn begin() -> (u64, Arc<AtomicBool>) {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.cancelled.store(true, Ordering::Relaxed);
        s.cancelled = Arc::new(AtomicBool::new(false));
        s.generation = s.generation.wrapping_add(1);
        s.busy = true;
        s.error = None;
        s.skipped.clear();
        (s.generation, s.cancelled.clone())
    })
}
fn current(generation: u64) -> bool {
    STATE.with(|s| s.borrow().generation == generation)
}
fn finish(generation: u64) {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if s.generation == generation {
            s.busy = false;
        }
    });
}
pub fn database() -> Result<nixe_media_cache::Cache, ProviderError> {
    nixe_media_cache::Cache::open(&nixe_core::paths::cache_dir().join("library.sqlite3"))
        .map_err(|_| ProviderError::Failed("Metadata cache is unavailable".into()))
}
/// At startup: libraries synced recently fetch only what changed.
pub fn start() {
    sync(false);
}
/// The whole of every library.
pub fn refresh() {
    sync(true);
}
/// The kinds a library's titles are counted by: (`type` for Plex, kind).
fn kinds(library: &nixe_media_model::Library) -> &'static [(Option<&'static str>, MediaKind)] {
    if library.kind == MediaKind::Series {
        &[(None, MediaKind::Series), (Some("3"), MediaKind::Season), (Some("4"), MediaKind::Episode)]
    } else {
        &[(None, MediaKind::Movie)]
    }
}
fn kind_of(library: &nixe_media_model::Library, item: &MediaItem) -> Option<usize> {
    kinds(library).iter().position(|(_, kind)| *kind == item.kind)
}
fn counts(client: &Client, library: &nixe_media_model::Library) -> Result<Vec<u64>, ProviderError> {
    kinds(library).iter().map(|(kind, _)| client.count(&library.id, *kind)).collect()
}
/// What changed in `library` since `since`, over its snapshot; None when something was
/// removed and only a full fetch will do.
fn incremental(
    client: &Client,
    cache: &nixe_media_cache::Cache,
    library: &nixe_media_model::Library,
    marks: &nixe_media_cache::SyncMarks,
    since: i64,
    cancelled: &AtomicBool,
    publish: &mut impl FnMut(Vec<MediaItem>) -> Result<(), ProviderError>,
) -> Result<Option<crate::sync::Synced>, ProviderError> {
    let mut changed = Vec::new();
    for (kind, _) in kinds(library) {
        for field in [nixe_plex::ChangeField::Metadata, nixe_plex::ChangeField::Watched] {
            let mut next = None;
            loop {
                if cancelled.load(Ordering::Relaxed) {
                    return Err(ProviderError::Failed("Refresh cancelled".into()));
                }
                let page = client.changed_page(&library.id, *kind, field, since, next.as_deref())?;
                changed.extend(page.items);
                if changed.len() > crate::servers::MAX_TITLES {
                    return Ok(None);
                }
                next = page.next;
                if next.is_none() {
                    break;
                }
            }
        }
    }
    // Collections are few; they're fetched whole.
    let collections = match fetch(client, &library.id, true, false, false, cancelled, publish) {
        Ok(collections) => collections,
        Err(ProviderError::NotFound) => Vec::new(),
        Err(e) => return Err(e),
    };
    let cached: Vec<MediaItem> = cache
        .snapshot_items(client.server_id(), &library.id)
        .map_err(|_| ProviderError::Failed("Could not read the Plex library cache".into()))?
        .into_iter()
        .filter(|i| i.kind != MediaKind::Collection)
        .collect();
    let (mut items, added) = crate::sync::apply(cached, changed.clone(), kinds(library).len(), |i| kind_of(library, i));
    let now = counts(client, library)?;
    if !crate::sync::nothing_removed(&marks.counts, &now, &added) {
        return Ok(None);
    }
    if !changed.is_empty() {
        publish(changed)?;
    }
    items.extend(collections);
    Ok(Some((items, now)))
}
/// Find the configured server through the account when its saved address no longer
/// answers: the client, and its token saved for the new address. None without an
/// account token (a manual connection).
fn rediscover(settings: &PlexSettings, client_id: &str) -> Option<Result<Client, ProviderError>> {
    let account = credentials::load_account().ok()?;
    Some((|| {
        let (client, token) = nixe_plex::rediscover(client_id, &account, &settings.server.id)?;
        credentials::save(&client.address(), &token)?;
        Ok(client)
    })())
}
fn fetch(
    client: &Client,
    library: &str,
    collections: bool,
    episodes: bool,
    seasons: bool,
    cancelled: &AtomicBool,
    publish: &mut impl FnMut(Vec<MediaItem>) -> Result<(), ProviderError>,
) -> Result<Vec<MediaItem>, ProviderError> {
    let mut items = Vec::new();
    let mut pending = Vec::new();
    let mut next = None;
    loop {
        if cancelled.load(Ordering::Relaxed) {
            return Err(ProviderError::Failed("Refresh cancelled".into()));
        }
        let page = if collections {
            client.collection_page(library, next.as_deref())
        } else if seasons {
            client.season_page(library, next.as_deref())
        } else if episodes {
            client.episode_page(library, next.as_deref())
        } else {
            client.library_page(library, next.as_deref())
        }?;
        pending.extend(page.items.iter().cloned());
        items.extend(page.items);
        if items.len() > crate::servers::MAX_TITLES {
            return Err(ProviderError::Failed(format!(
                "more than {} titles of one kind",
                crate::servers::MAX_TITLES
            )));
        }
        if (next.is_none() || pending.len() >= 500 || page.next.is_none())
            && !pending.is_empty() {
                publish(std::mem::take(&mut pending))?;
            }
        next = page.next;
        if next.is_none() {
            return Ok(items);
        }
    }
}
enum RefreshEvent {
    /// A library that couldn't be loaded: its title and why.
    Skipped(String, String),
    Cached(Vec<MediaItem>),
    Ready(Client, Vec<nixe_media_model::Library>),
    Batch(Vec<MediaItem>),
    Finished(Result<Vec<MediaItem>, ProviderError>),
}
fn merge_items(state: &mut State, items: Vec<MediaItem>) {
    state.catalog.merge(items)
}
fn sync(full: bool) {
    let Some(settings) = prefs::get().plex else { return };
    let client_id = prefs::get().client_id;
    let (generation, cancelled) = begin();
    let started = revision();
    let server = settings.server.clone();
    let (send, receive) = async_channel::bounded(2);
    window::status("Connecting to Plex…");
    crate::library::refresh();
    std::thread::spawn(move || {
        let publish = |event| send.send_blocking(event).map_err(|_| ProviderError::Failed("Refresh cancelled".into()));
        let result = (|| {
            let mut cache = database()?;
            if let Ok(items) = cache.all_items(&settings.server) {
                publish(RefreshEvent::Cached(items))?;
            }
            if cancelled.load(Ordering::Relaxed) {
                return Err(ProviderError::Failed("Refresh cancelled".into()));
            }
            let saved = credentials::load(&settings.address).and_then(|secret| Client::reconnect(&settings.address, secret, &client_id));
            let client = match saved {
                Ok(client) => client,
                // The server moved (a new address) or its token was replaced.
                Err(e @ (ProviderError::Offline | ProviderError::AuthenticationRequired)) => match rediscover(&settings, &client_id) {
                    Some(Ok(client)) => client,
                    _ => return Err(e),
                },
                Err(e) => return Err(e),
            };
            let libraries = client.list_libraries()?;
            publish(RefreshEvent::Ready(client.clone(), libraries.clone()))?;
            let continuing = client
                .continue_page()
                .map(|page| {
                    page.items
                        .into_iter()
                        .filter(|item| matches!(item.kind, MediaKind::Movie | MediaKind::Episode) && item.watch.position_ms > 0)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            publish(RefreshEvent::Batch(continuing.clone()))?;
            let mut all = Vec::new();
            let cached = cache.all_items(client.server_id()).unwrap_or_default();
            for library in &libraries {
                let mut batch = |items| publish(RefreshEvent::Batch(items));
                let marks = cache.sync_marks(client.server_id(), &library.id).ok().flatten();
                let sync_started = crate::sync::now_secs();
                let loaded = (|| {
                    if let (crate::sync::Plan::Since(since), Some(marks)) =
                        (crate::sync::plan(marks.as_ref(), kinds(library).len(), sync_started, full), &marks)
                        && let Some((items, counts)) = incremental(&client, &cache, library, marks, since, &cancelled, &mut batch)?
                    {
                        return Ok((items, counts, false));
                    }
                    let mut items = fetch(&client, &library.id, false, false, false, &cancelled, &mut batch)?;
                    match fetch(&client, &library.id, true, false, false, &cancelled, &mut batch) {
                        Ok(collections) => items.extend(collections),
                        Err(ProviderError::NotFound) => {}
                        Err(e) => return Err(e),
                    }
                    if library.kind == MediaKind::Series {
                        items.extend(fetch(&client, &library.id, false, false, true, &cancelled, &mut batch)?);
                        items.extend(fetch(&client, &library.id, false, true, false, &cancelled, &mut batch)?);
                    }
                    // Without counts the next sync is a full one again.
                    Ok((items, counts(&client, library).unwrap_or_default(), true))
                })();
                if cancelled.load(Ordering::Relaxed) {
                    return Err(ProviderError::Failed("Refresh cancelled".into()));
                }
                let (items, counts, was_full) = match loaded {
                    Ok(loaded) => loaded,
                    // Signed out, or the server's gone: that's every library, not this one.
                    Err(e @ (ProviderError::AuthenticationRequired | ProviderError::Offline)) => return Err(e),
                    // One library failing keeps the others; its cached titles stay.
                    Err(e) => {
                        publish(RefreshEvent::Skipped(library.title.clone(), e.to_string()))?;
                        all.extend(cached.iter().filter(|i| i.library_id == library.id).cloned());
                        continue;
                    }
                };
                cache
                    .replace_library_since(client.server_id(), &library.id, &items, started)
                    .map_err(|_| ProviderError::Failed("Could not save the Plex library cache".into()))?;
                let full_at = if was_full { sync_started } else { marks.map_or(sync_started, |m| m.full_at) };
                let _ = cache.set_sync_marks(
                    client.server_id(),
                    &library.id,
                    &nixe_media_cache::SyncMarks { synced_at: sync_started, full_at, counts },
                );
                all.extend(items);
            }
            let mut positions: HashMap<_, _> = all.iter().enumerate().map(|(i, item)| (item.id.clone(), i)).collect();
            for item in continuing {
                // Library pages were fetched later than onDeck; keep their movie state.
                if !positions.contains_key(&item.id) {
                    positions.insert(item.id.clone(), all.len());
                    all.push(item);
                }
            }
            for item in cache.playback_since(client.server_id(), started).map_err(|_| ProviderError::InvalidResponse)? {
                if let Some(&index) = positions.get(&item.id) {
                    all[index].watch = item.watch;
                } else {
                    positions.insert(item.id.clone(), all.len());
                    all.push(item);
                }
            }
            Ok(all)
        })();
        let _ = publish(RefreshEvent::Finished(result));
    });
    glib::spawn_future_local(async move {
        let mut connected = false;
        while let Ok(event) = receive.recv().await {
            if !current(generation) || !prefs::get().plex.is_some_and(|p| p.server == server) {
                break;
            }
            match event {
                RefreshEvent::Skipped(title, why) => {
                    STATE.with(|s| s.borrow_mut().skipped.push(title.clone()));
                    window::status(&format!("Couldn't refresh the Plex library {title} ({why}); keeping its cached titles"));
                }
                RefreshEvent::Cached(items) => {
                    STATE.with(|s| {
                        let mut state = s.borrow_mut();
                        if state.items.is_empty() || state.items.iter().any(|i| i.id.server != server) {
                            state.items = items;
                        }
                    });
                }
                RefreshEvent::Ready(client, libraries) => {
                    // A manual address must still refer to the configured server.
                    if *client.server_id() != server {
                        finish(generation);
                        STATE.with(|s| {
                            let mut s = s.borrow_mut();
                            s.client = None;
                            s.error = Some("The server identity changed. Reconnect to choose the server.".into());
                        });
                        crate::library::refresh();
                        window::status("The Plex server identity changed. Reconnect to choose the server.");
                        break;
                    }
                    let old = prefs::get().plex.map(|p| p.address).unwrap_or_default();
                    let moved = !old.is_empty() && old != client.address();
                    prefs::update(|p| {
                        p.plex = Some(PlexSettings {
                            address: client.address(),
                            name: client.name.clone(),
                            server: server.clone(),
                            libraries,
                        })
                    });
                    if moved {
                        // Found again at a new address; its token was saved there.
                        std::thread::spawn(move || {
                            let _ = credentials::delete(&old);
                        });
                    }
                    STATE.with(|s| {
                        let mut state = s.borrow_mut();
                        state.items.retain(|i| i.id.server == server);
                        state.client = Some(client);
                    });
                    connected = true;
                    crate::artwork::retry_visible();
                    crate::playlists::on_connection_changed();
                    window::status("Plex connected · loading libraries…");
                }
                RefreshEvent::Batch(items) => {
                    STATE.with(|s| merge_items(&mut s.borrow_mut(), items));
                    let count = STATE.with(|s| s.borrow().items.len());
                    window::status(&format!("Loading Plex libraries · {count} titles"));
                }
                RefreshEvent::Finished(result) => {
                    finish(generation);
                    match result {
                        Ok(items) => {
                            STATE.with(|s| {
                                let mut state = s.borrow_mut();
                                state.catalog.finish(&server, items, started);
                            });
                            let (count, skipped) = STATE.with(|s| (s.borrow().items.len(), s.borrow().skipped.clone()));
                            if skipped.is_empty() {
                                window::status(&format!("Plex connected · {count} titles"));
                            } else {
                                window::status(&format!("Plex connected · {count} titles · couldn't refresh {}", skipped.join(", ")));
                            }
                        }
                        Err(error) => {
                            STATE.with(|s| s.borrow_mut().error = Some(error.to_string()));
                            if !connected || error == ProviderError::AuthenticationRequired {
                                STATE.with(|s| s.borrow_mut().client = None);
                            }
                            window::status(&format!("{error} · keeping loaded and cached titles"));
                        }
                    }
                    crate::library::refresh();
                    break;
                }
            }
            crate::library::refresh();
        }
    });
}
pub fn disconnect() {
    let Some(settings) = prefs::get().plex else { return };
    let server = settings.server.clone();
    let (generation, _) = begin();
    nixe_core::cmd::background(
        move || {
            // Signed out of the account too; there may be none (a manual connection).
            let _ = credentials::delete_account();
            credentials::delete(&settings.address)
        },
        move |result| {
            if !current(generation) {
                return;
            }
            finish(generation);
            match result {
                Ok(()) => {
                    crate::window::stop_server(&server);
                    prefs::update(|p| p.plex = None);
                    STATE.with(|s| {
                        let mut s = s.borrow_mut();
                        s.items.clear();
                        s.watch.clear();
                        s.client = None;
                    });
                    window::status("Plex disconnected");
                    crate::library::refresh();
                }
                Err(error) => window::status(&error.to_string()),
            }
        },
    );
}
pub fn cancel() {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.cancelled.store(true, Ordering::Relaxed);
        s.generation += 1;
        s.busy = false;
    });
}

type ConnectionResult = Result<(Client, Vec<nixe_media_model::Library>), ProviderError>;
struct LoginUi {
    dialog: glib::WeakRef<gtk::Window>,
    sign: glib::WeakRef<gtk::Button>,
    manual: glib::WeakRef<gtk::Button>,
    selector: gtk::DropDown,
    selection: gtk::Box,
    address: gtk::Entry,
    token: gtk::PasswordEntry,
    message: gtk::Label,
    alive: Arc<AtomicBool>,
    servers: RefCell<Vec<nixe_plex::DiscoveredServer>>,
    account: RefCell<Option<Secret>>,
}
impl LoginUi {
    fn busy(&self, busy: bool, message: &str) {
        self.message.set_text(message);
        if let Some(sign) = self.sign.upgrade() {
            sign.set_sensitive(!busy);
            sign.set_label(if !self.servers.borrow().is_empty() {
                "Connect"
            } else if self.account.borrow().is_some() {
                "Find servers"
            } else {
                "Sign in with Plex"
            });
        }
        if let Some(manual) = self.manual.upgrade() {
            manual.set_sensitive(!busy);
        }
        self.selector.set_sensitive(!busy);
        self.address.set_sensitive(!busy);
        self.token.set_sensitive(!busy);
    }
}
pub fn connect_dialog() {
    let (dialog, card) = crate::widgets::dialog("Connect Plex", 520);
    let hint = crate::widgets::label("Sign in with Plex in your browser, then choose a server from your account.", "dim");
    hint.set_wrap(true);
    card.append(&hint);
    let selection = crate::widgets::vbox(8);
    selection.append(&crate::widgets::label("Server", "settings-option-title"));
    let selector = gtk::DropDown::from_strings(&[]);
    selection.append(&selector);
    selection.set_visible(false);
    card.append(&selection);
    let advanced = gtk::Expander::new(Some("Manual connection"));
    let fields = crate::widgets::vbox(8);
    fields.set_margin_top(12);
    let address = gtk::Entry::new();
    address.set_placeholder_text(Some("http://server-address:32400"));
    address.set_text(&prefs::get().plex.map(|s| s.address).unwrap_or_default());
    fields.append(&crate::widgets::label("Server address", "settings-option-title"));
    fields.append(&address);
    let token =
        gtk::PasswordEntry::builder().show_peek_icon(true).placeholder_text("Optional: use an existing Plex token").build();
    fields.append(&token);
    let manual = gtk::Button::with_label("Connect manually");
    manual.set_halign(gtk::Align::Start);
    fields.append(&manual);
    advanced.set_child(Some(&fields));
    card.append(&advanced);
    let message = crate::widgets::label("", "dim");
    message.set_wrap(true);
    card.append(&message);
    let actions = crate::widgets::hbox(8);
    actions.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Not now");
    let sign = gtk::Button::with_label("Sign in with Plex");
    sign.add_css_class("suggested-action");
    actions.append(&cancel);
    actions.append(&sign);
    card.append(&actions);
    let alive = Arc::new(AtomicBool::new(true));
    let a = alive.clone();
    dialog.connect_close_request(move |_| {
        a.store(false, Ordering::Relaxed);
        glib::Propagation::Proceed
    });
    let ui = std::rc::Rc::new(LoginUi {
        dialog: dialog.downgrade(),
        sign: sign.downgrade(),
        manual: manual.downgrade(),
        selector,
        selection,
        address,
        token,
        message,
        alive,
        servers: RefCell::new(vec![]),
        account: RefCell::new(None),
    });
    let weak = dialog.downgrade();
    cancel.connect_clicked(move |_| {
        if let Some(d) = weak.upgrade() {
            d.close();
        }
    });
    let state = ui.clone();
    sign.connect_clicked(move |_| {
        let selected = state.servers.borrow().get(state.selector.selected() as usize).cloned();
        if let Some(server) = selected {
            connect_discovered(state.clone(), server);
        } else if let Some(secret) = state.account.borrow().clone() {
            discover_servers(state.clone(), secret);
        } else {
            web_login(state.clone(), None);
        }
    });
    manual.connect_clicked(move |_| {
        let address = match nixe_plex::server_url(&ui.address.text()) {
            Ok(url) => url.to_string(),
            Err(e) => {
                ui.message.set_text(&e.to_string());
                return;
            }
        };
        let token = ui.token.text().to_string();
        ui.token.set_text("");
        if !token.is_empty() {
            match Secret::new(token) {
                Ok(secret) => connect_manual(ui.clone(), address, secret),
                Err(e) => ui.message.set_text(&e.to_string()),
            }
        } else if let Some(secret) = ui.account.borrow().clone() {
            connect_manual(ui.clone(), address, secret);
        } else {
            web_login(ui.clone(), Some(address));
        }
    });
    dialog.present();
}
fn connected(ui: std::rc::Rc<LoginUi>, result: ConnectionResult) {
    if !ui.alive.load(Ordering::Relaxed) {
        return;
    }
    match result {
        Ok((client, libraries)) => {
            prefs::update(|p| {
                p.plex = Some(PlexSettings {
                    address: client.address(),
                    name: client.name.clone(),
                    server: client.server_id().clone(),
                    libraries,
                })
            });
            STATE.with(|s| s.borrow_mut().client = Some(client));
            if let Some(dialog) = ui.dialog.upgrade() {
                dialog.close();
            }
            nixe_ui::settings_card::close();
            refresh();
        }
        Err(error) => ui.busy(false, &error.to_string()),
    }
}
fn prepare(client: Client, secret: Secret, account: Option<Secret>, alive: &AtomicBool) -> ConnectionResult {
    let libraries = client.list_libraries()?;
    if !alive.load(Ordering::Relaxed) {
        return Err(ProviderError::Failed("Sign-in cancelled".into()));
    }
    credentials::save(&client.address(), &secret)?;
    // Kept so the server can be found again if its address changes.
    if let Some(account) = account {
        credentials::save_account(&account)?;
    }
    Ok((client, libraries))
}
fn connect_manual(ui: std::rc::Rc<LoginUi>, address: String, token: Secret) {
    ui.busy(true, "Connecting to Plex…");
    let (client_id, alive, account) = (prefs::get().client_id, ui.alive.clone(), ui.account.borrow().clone());
    nixe_core::cmd::background(
        move || {
            let client = Client::connect(&address, token.clone(), &client_id)?;
            prepare(client, token, account, &alive)
        },
        move |result| connected(ui, result),
    );
}
fn connect_discovered(ui: std::rc::Rc<LoginUi>, server: nixe_plex::DiscoveredServer) {
    ui.busy(true, &format!("Connecting to {}…", server.name));
    let (client_id, alive, account) = (prefs::get().client_id, ui.alive.clone(), ui.account.borrow().clone());
    nixe_core::cmd::background(
        move || {
            if !alive.load(Ordering::Relaxed) {
                return Err(ProviderError::Failed("Sign-in cancelled".into()));
            }
            let (client, token) = server.connect(&client_id)?;
            prepare(client, token, account, &alive)
        },
        move |result| connected(ui, result),
    );
}
fn discover_servers(ui: std::rc::Rc<LoginUi>, secret: Secret) {
    ui.busy(true, "Finding your Plex servers…");
    let client_id = prefs::get().client_id;
    nixe_core::cmd::background(
        move || nixe_plex::discover(&client_id, &secret),
        move |result| {
            if !ui.alive.load(Ordering::Relaxed) {
                return;
            }
            match result {
                Ok(servers) => {
                    let names: Vec<&str> = servers.iter().map(|s| s.name.as_str()).collect();
                    ui.selector.set_model(Some(&gtk::StringList::new(&names)));
                    ui.selector.set_selected(0);
                    ui.selection.set_visible(!servers.is_empty());
                    let single = (servers.len() == 1).then(|| servers[0].clone());
                    let empty = servers.is_empty();
                    *ui.servers.borrow_mut() = servers;
                    if let Some(server) = single {
                        connect_discovered(ui, server);
                    } else {
                        ui.busy(
                            false,
                            if empty {
                                "No accessible servers were found. Check your Plex account or use a manual connection."
                            } else {
                                "Choose the server you want to use."
                            },
                        );
                    }
                }
                Err(error) => {
                    if error == ProviderError::AuthenticationRequired {
                        ui.account.borrow_mut().take();
                    }
                    ui.busy(false, &error.to_string());
                }
            }
        },
    );
}
fn web_login(ui: std::rc::Rc<LoginUi>, manual_address: Option<String>) {
    ui.busy(true, "Opening Plex sign-in in your browser…");
    let client_id = prefs::get().client_id;
    let worker_id = client_id.clone();
    nixe_core::cmd::background(
        move || nixe_plex::create_pin(&worker_id),
        move |result| {
            if !ui.alive.load(Ordering::Relaxed) {
                return;
            }
            let pin = match result {
                Ok(pin) => pin,
                Err(error) => {
                    ui.busy(false, &error.to_string());
                    return;
                }
            };
            if gio::AppInfo::launch_default_for_uri(&pin.login_url, None::<&gio::AppLaunchContext>).is_err() {
                ui.busy(false, "Could not open your browser");
                return;
            }
            ui.message.set_text("Complete sign-in in your browser. Theater will continue automatically.");
            let alive = ui.alive.clone();
            nixe_core::cmd::background(
                move || {
                    for _ in 0..180 {
                        if !alive.load(Ordering::Relaxed) {
                            return Err(ProviderError::Failed("Sign-in cancelled".into()));
                        }
                        if let Some(token) = nixe_plex::check_pin(&client_id, &pin)? {
                            return Ok(token);
                        }
                        std::thread::sleep(Duration::from_secs(1));
                    }
                    Err(ProviderError::Failed("Plex sign-in expired. Try again.".into()))
                },
                move |result| {
                    if !ui.alive.load(Ordering::Relaxed) {
                        return;
                    }
                    match result {
                        Ok(token) => {
                            *ui.account.borrow_mut() = Some(token.clone());
                            if let Some(address) = manual_address {
                                connect_manual(ui, address, token);
                            } else {
                                discover_servers(ui, token);
                            }
                        }
                        Err(error) => ui.busy(false, &error.to_string()),
                    }
                },
            );
        },
    );
}

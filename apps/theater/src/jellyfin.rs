//! One Jellyfin account; transport and SQLite stay on workers.
use crate::{
    credentials, library, prefs,
    servers::{self, Catalog},
    window,
};
use gtk::{glib, prelude::*};
use nixe_jellyfin::Client;
use nixe_media_model::*;
use std::{
    cell::RefCell,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
#[derive(Default)]
struct State {
    client: Option<Client>,
    catalog: Catalog,
    generation: u64,
    cancelled: Arc<AtomicBool>,
    busy: bool,
    error: Option<String>,
}
thread_local! {static STATE:RefCell<State>=RefCell::new(State::default());}
pub fn client() -> Option<Client> {
    STATE.with(|s| s.borrow().client.clone())
}
pub fn items() -> Vec<MediaItem> {
    STATE.with(|s| s.borrow().catalog.items.clone())
}
pub fn latest_watch(item: MediaItem) -> MediaItem {
    STATE.with(|s| s.borrow().catalog.latest(item))
}
pub fn details_watch(item: &MediaItem, children: &[MediaItem], started: i64) {
    STATE.with(|s| s.borrow_mut().catalog.confirmed(item, children, started))
}
pub fn playback_watch(item: MediaItem, revision: i64) {
    if !prefs::get().jellyfin.is_some_and(|p| p.server == item.id.server) {
        return;
    }
    if STATE.with(|s| s.borrow_mut().catalog.record(item, revision)) {
        library::watch_changed()
    }
}
pub fn connection_state() -> crate::servers::ConnectionState {
    STATE.with(|s| {
        let s = s.borrow();
        let connected = s.client.is_some();
        let message = if let Some(error) = &s.error {
            format!("Jellyfin: {error}")
        } else if connected && s.busy {
            "Jellyfin connected · loading library…".into()
        } else if connected {
            "Jellyfin connected".into()
        } else if s.busy {
            "Connecting to Jellyfin…".into()
        } else {
            "Jellyfin is offline · cached titles remain available".into()
        };
        crate::servers::ConnectionState { provider: nixe_media_model::ProviderKind::Jellyfin, connected, busy: s.busy, message }
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
        (s.generation, s.cancelled.clone())
    })
}
pub fn cancel() {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.cancelled.store(true, Ordering::Relaxed);
        s.generation += 1;
        s.busy = false;
    })
}
// Short-lived channel messages; the large one (a client and its libraries) comes once.
#[allow(clippy::large_enum_variant)]
enum Update {
    /// A library that couldn't be loaded: its title and why.
    Skipped(String, String),
    Cached(Vec<MediaItem>),
    Ready(Client, Vec<Library>),
    Batch(Vec<MediaItem>),
    Finished(Result<Vec<MediaItem>, ProviderError>),
}
fn scope(items: &mut [MediaItem], known: &[MediaItem]) {
    let libraries: std::collections::HashMap<_, _> = known.iter().map(|i| (&i.id, &i.library_id)).collect();
    for item in items {
        if item.library_id.is_empty()
            && let Some(library) = libraries.get(&item.id).or_else(|| item.series.as_ref().and_then(|id| libraries.get(id))) {
                item.library_id = (*library).clone()
            }
    }
}
/// At startup: libraries synced recently fetch only what changed.
pub fn start() {
    sync(false);
}
/// The whole of every library.
pub fn refresh() {
    sync(true);
}
/// What changed in `library` since `since`, over its snapshot; None when something was
/// removed and only a full fetch will do. Jellyfin counts a library's titles as one kind.
fn incremental(
    client: &Client,
    cache: &nixe_media_cache::Cache,
    library: &Library,
    marks: &nixe_media_cache::SyncMarks,
    since: i64,
    cancelled: &AtomicBool,
) -> Result<Option<crate::sync::Synced>, ProviderError> {
    let mut changed = vec![];
    for field in [nixe_jellyfin::ChangeField::Metadata, nixe_jellyfin::ChangeField::Watched] {
        let mut next = None;
        loop {
            if cancelled.load(Ordering::Relaxed) {
                return Err(ProviderError::Failed("Refresh cancelled".into()));
            }
            let page = client.changed_page(&library.id, field, since, next.as_deref())?;
            changed.extend(page.items);
            if changed.len() > servers::MAX_TITLES {
                return Ok(None);
            }
            next = page.next;
            if next.is_none() {
                break;
            }
        }
    }
    let cached = cache
        .snapshot_items(client.server_id(), &library.id)
        .map_err(|_| ProviderError::Failed("Could not read the Jellyfin library cache".into()))?;
    let (items, added) = crate::sync::apply(cached, changed, 1, |_| Some(0));
    let now = vec![client.count(&library.id)?];
    Ok(crate::sync::nothing_removed(&marks.counts, &now, &added).then_some((items, now)))
}
fn sync(full: bool) {
    let Some(settings) = prefs::get().jellyfin else { return };
    let (generation, cancelled) = begin();
    let started = servers::revision();
    let device = prefs::get().client_id;
    let server = settings.server.clone();
    let (send, receive) = async_channel::bounded(2);
    std::thread::spawn(move || {
        let publish = |update| send.send_blocking(update).map_err(|_| ProviderError::Failed("Refresh cancelled".into()));
        let result = (|| {
            let mut cache = servers::database()?;
            if let Ok(items) = cache.all_items(&settings.server) {
                publish(Update::Cached(items))?
            }
            if cancelled.load(Ordering::Relaxed) {
                return Err(ProviderError::Failed("Refresh cancelled".into()));
            }
            let secret = credentials::load_for("jellyfin", &settings.address)?;
            let client = Client::connect(&settings.address, secret, &settings.user_id, &device)?;
            if client.server_id() != &settings.server {
                return Err(ProviderError::Failed(
                    "The Jellyfin server identity changed. Reconnect to choose the server.".into(),
                ));
            }
            let libraries = client.list_libraries()?;
            publish(Update::Ready(client.clone(), libraries.clone()))?;
            let continuing = client.continue_page().map(|page| page.items).unwrap_or_default();
            publish(Update::Batch(continuing.clone()))?;
            let mut all = vec![];
            let cached = cache.all_items(client.server_id()).unwrap_or_default();
            for library in &libraries {
                let marks = cache.sync_marks(client.server_id(), &library.id).ok().flatten();
                let sync_started = crate::sync::now_secs();
                let loaded = (|| {
                    if let (crate::sync::Plan::Since(since), Some(marks)) = (crate::sync::plan(marks.as_ref(), 1, sync_started, full), &marks)
                        && let Some((items, counts)) = incremental(&client, &cache, library, marks, since, &cancelled)?
                    {
                        publish(Update::Batch(items.clone()))?;
                        return Ok((items, counts, false));
                    }
                    let mut items = vec![];
                    let mut pending = vec![];
                    let mut next = None;
                    loop {
                        if cancelled.load(Ordering::Relaxed) {
                            return Err(ProviderError::Failed("Refresh cancelled".into()));
                        }
                        let page = client.library_page(&library.id, next.as_deref())?;
                        pending.extend(page.items.iter().cloned());
                        items.extend(page.items);
                        if items.len() > servers::MAX_TITLES {
                            return Err(ProviderError::Failed(format!("more than {} titles", servers::MAX_TITLES)));
                        }
                        if (next.is_none() || pending.len() >= 500 || page.next.is_none()) && !pending.is_empty() {
                            publish(Update::Batch(std::mem::take(&mut pending)))?
                        }
                        next = page.next;
                        if next.is_none() {
                            // Without a count the next sync is a full one again.
                            let counts = client.count(&library.id).map(|n| vec![n]).unwrap_or_default();
                            return Ok((items, counts, true));
                        }
                    }
                })();
                if cancelled.load(Ordering::Relaxed) {
                    return Err(ProviderError::Failed("Refresh cancelled".into()));
                }
                let (items, counts, was_full) = match loaded {
                    Ok(loaded) => loaded,
                    Err(e @ (ProviderError::AuthenticationRequired | ProviderError::Offline)) => return Err(e),
                    // One library failing keeps the others; its cached titles stay.
                    Err(e) => {
                        publish(Update::Skipped(library.title.clone(), e.to_string()))?;
                        all.extend(cached.iter().filter(|i| i.library_id == library.id).cloned());
                        continue;
                    }
                };
                cache
                    .replace_library_since(client.server_id(), &library.id, &items, started)
                    .map_err(|_| ProviderError::Failed("Could not save the Jellyfin library cache".into()))?;
                let full_at = if was_full { sync_started } else { marks.map_or(sync_started, |m| m.full_at) };
                let _ = cache.set_sync_marks(
                    client.server_id(),
                    &library.id,
                    &nixe_media_cache::SyncMarks { synced_at: sync_started, full_at, counts },
                );
                all.extend(items);
            }
            let mut continuing = continuing;
            scope(&mut continuing, &all);
            let mut catalog = Catalog::default();
            catalog.merge(all);
            let known: std::collections::HashSet<_> = catalog.items.iter().map(|i| i.id.clone()).collect();
            let new: Vec<_> = continuing.into_iter().filter(|item| !known.contains(&item.id)).collect();
            catalog.merge(new);
            let overlays = cache.playback_since(client.server_id(), started).map_err(|_| ProviderError::InvalidResponse)?;
            catalog.merge(overlays);
            Ok(catalog.items)
        })();
        let _ = publish(Update::Finished(result));
    });
    glib::spawn_future_local(async move {
        let mut connected = false;
        while let Ok(update) = receive.recv().await {
            if STATE.with(|s| s.borrow().generation) != generation || !prefs::get().jellyfin.is_some_and(|p| p.server == server) {
                break;
            }
            let done = matches!(&update, Update::Finished(_));
            match update {
                Update::Skipped(title, why) => {
                    window::status(&format!("Couldn't refresh the Jellyfin library {title} ({why}); keeping its cached titles"))
                }
                Update::Cached(items) => STATE.with(|s| {
                    let mut s = s.borrow_mut();
                    if s.catalog.items.is_empty() {
                        s.catalog.items = items
                    }
                }),
                Update::Ready(client, libraries) => {
                    prefs::update(|p| {
                        p.jellyfin = Some(prefs::JellyfinSettings {
                            address: client.address(),
                            name: client.name.clone(),
                            server: server.clone(),
                            libraries,
                            user_id: client.user_id().into(),
                        })
                    });
                    STATE.with(|s| s.borrow_mut().client = Some(client));
                    connected = true;
                    crate::artwork::retry_visible();
                    crate::playlists::on_connection_changed();
                    window::status("Jellyfin connected · loading libraries…");
                }
                Update::Batch(mut items) => {
                    STATE.with(|s| {
                        let mut s = s.borrow_mut();
                        scope(&mut items, &s.catalog.items);
                        s.catalog.merge(items)
                    });
                    window::status("Jellyfin connected · loading libraries…");
                }
                Update::Finished(result) => {
                    STATE.with(|s| s.borrow_mut().busy = false);
                    match result {
                        Ok(items) => {
                            STATE.with(|s| s.borrow_mut().catalog.finish(&server, items, started));
                            window::status("Jellyfin libraries updated")
                        }
                        Err(error) => {
                            STATE.with(|s| s.borrow_mut().error = Some(error.to_string()));
                            if !connected || error == ProviderError::AuthenticationRequired {
                                STATE.with(|s| s.borrow_mut().client = None)
                            }
                            window::status(&format!("{error} · keeping loaded and cached Jellyfin titles"))
                        }
                    }
                }
            }
            library::refresh();
            if done {
                break;
            }
        }
    });
}
pub fn disconnect() {
    let Some(settings) = prefs::get().jellyfin else { return };
    let (generation, _) = begin();
    nixe_core::cmd::background(
        move || credentials::delete_for("jellyfin", &settings.address),
        move |result| {
            if STATE.with(|s| s.borrow().generation) != generation {
                return;
            }
            STATE.with(|s| s.borrow_mut().busy = false);
            match result {
                Ok(()) => {
                    window::stop_server(&settings_server());
                    prefs::update(|p| p.jellyfin = None);
                    STATE.with(|s| {
                        let mut s = s.borrow_mut();
                        s.catalog = Catalog::default();
                        s.client = None
                    });
                    library::refresh();
                    window::status("Jellyfin disconnected")
                }
                Err(error) => window::status(&error.to_string()),
            }
        },
    );
}
fn settings_server() -> ServerId {
    // Settings can be gone by the time a worker reports back; no server is no match.
    prefs::get().jellyfin.map(|j| j.server).unwrap_or_else(|| nixe_media_model::ServerId {
        provider: nixe_media_model::ProviderKind::Jellyfin,
        id: String::new(),
    })
}
pub fn connect_dialog() {
    let (dialog, card) = crate::widgets::dialog("Connect Jellyfin", 520);
    let address = gtk::Entry::new();
    address.set_placeholder_text(Some("https://server-address/jellyfin"));
    address.set_text(&prefs::get().jellyfin.map(|p| p.address).unwrap_or_default());
    let username = gtk::Entry::new();
    username.set_placeholder_text(Some("Username"));
    let password = gtk::PasswordEntry::builder().show_peek_icon(true).placeholder_text("Password").build();
    for (label, field) in [
        ("Server address", address.clone().upcast::<gtk::Widget>()),
        ("Username", username.clone().upcast()),
        ("Password", password.clone().upcast()),
    ] {
        card.append(&crate::widgets::label(label, "settings-option-title"));
        card.append(&field)
    }
    let message =
        crate::widgets::label("Sign in to your Jellyfin server. The access token is stored in your desktop keyring.", "dim");
    message.set_wrap(true);
    card.append(&message);
    let actions = crate::widgets::hbox(8);
    actions.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Not now");
    let connect = gtk::Button::with_label("Connect");
    connect.add_css_class("suggested-action");
    actions.append(&cancel);
    actions.append(&connect);
    card.append(&actions);
    let alive = Arc::new(AtomicBool::new(true));
    let a = alive.clone();
    dialog.connect_close_request(move |_| {
        a.store(false, Ordering::Relaxed);
        glib::Propagation::Proceed
    });
    let weak = dialog.downgrade();
    cancel.connect_clicked(move |_| {
        if let Some(dialog) = weak.upgrade() {
            dialog.close()
        }
    });
    let weak = dialog.downgrade();
    connect.connect_clicked(move |button| {
        let address = address.text().to_string();
        let username = username.text().to_string();
        let secret = password.text().to_string();
        password.set_text("");
        if username.trim().is_empty() {
            message.set_text("Enter your Jellyfin username.");
            return;
        }
        button.set_sensitive(false);
        message.set_text("Connecting to Jellyfin…");
        let button = button.downgrade();
        let message = message.clone();
        let alive = alive.clone();
        let weak = weak.clone();
        let device = prefs::get().client_id;
        nixe_core::cmd::background(
            move || {
                let (client, token) = Client::authenticate(&address, &username, &secret, &device)?;
                let libraries = client.list_libraries()?;
                if !alive.load(Ordering::Relaxed) {
                    return Err(ProviderError::Failed("Sign-in cancelled".into()));
                }
                credentials::save_for("jellyfin", &client.address(), &token)?;
                Ok::<_, ProviderError>((client, libraries))
            },
            move |result| {
                let Some(dialog) = weak.upgrade() else { return };
                if !dialog.is_visible() {
                    return;
                }
                match result {
                    Ok((client, libraries)) => {
                        let server = client.server_id().clone();
                        let changed = prefs::get().jellyfin.is_some_and(|p| p.server != server || p.user_id != client.user_id());
                        if changed {
                            window::stop_server(&settings_server());
                            STATE.with(|s| s.borrow_mut().catalog = Catalog::default())
                        }
                        prefs::update(|p| {
                            p.jellyfin = Some(prefs::JellyfinSettings {
                                address: client.address(),
                                name: client.name.clone(),
                                server,
                                libraries,
                                user_id: client.user_id().into(),
                            })
                        });
                        STATE.with(|s| s.borrow_mut().client = Some(client));
                        dialog.close();
                        nixe_ui::settings_card::close();
                        refresh();
                    }
                    Err(error) => {
                        message.set_text(&error.to_string());
                        if let Some(button) = button.upgrade() {
                            button.set_sensitive(true)
                        }
                    }
                }
            },
        );
    });
    dialog.present();
}

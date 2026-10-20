//! Native provider-independent library views built from the shared Media Player cards.
use crate::{artwork, plex, prefs, servers, widgets, window};
use gtk::{gio, glib, prelude::*};
use nixe_media_model::index::edition_key;
use nixe_media_model::{MediaItem, MediaKind, PlaybackMode, PlaybackRequest, ProviderError, ProviderKind};
use nixe_ui::media::{self, Artwork, PosterCard};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
};
struct Ui {
    grids: HashMap<&'static str, gio::ListStore>,
    details: widgets::Page,
    filter: String,
    sort: String,
    query: String,
    filters: Vec<gtk::DropDown>,
    worker: crate::catalog::Worker,
    generation: u64,
    page: String,
    dirty: bool,
    scheduled: bool,
    requested: bool,
    server_scope: String,
    home_data: crate::catalog::Home,
    home_parts: Vec<gtk::Box>,
    discover_parts: Vec<gtk::Box>,
    discover_data: crate::catalog::Home,
}
/// The details page on screen: its title, the children shown, and the season picked.
type Rendered = (MediaItem, Vec<MediaItem>, Option<u32>);
thread_local! {
    static FILTERS_UPDATING: Cell<bool> = const { Cell::new(false) };
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
    static DETAIL_GENERATION: Cell<u64> = const { Cell::new(0) };
    static RENDERED: RefCell<Option<Rendered>> = const { RefCell::new(None) };
    static CURRENT: RefCell<Option<MediaItem>> = const { RefCell::new(None) };
    static HISTORY: RefCell<Vec<MediaItem>> = const { RefCell::new(Vec::new()) };
}
fn poster_card(item: &MediaItem, parent_count: bool) -> (Artwork, PosterCard) {
    let width = crate::presentation::width();
    let art = Artwork::new(width, width * 3 / 2, "video-x-generic-symbolic");
    let card = PosterCard::new(&art.root, width);
    let count = match item.kind {
        MediaKind::Series | MediaKind::Season => item.metadata.unwatched_episodes,
        MediaKind::Episode if parent_count => item.tv.unwatched_episodes,
        _ => None,
    };
    let watched = if let Some(count) = count { count == 0 } else { item.watch.watched };
    media::set_badge(&card.badge, watched, count.unwrap_or(0) as usize);
    if let Some(count) = count.filter(|n| *n > 0) {
        let description = format!("{count} unwatched episode{}", if count == 1 { "" } else { "s" });
        card.badge.update_property(&[gtk::accessible::Property::Label(&description)]);
        card.root.set_tooltip_text(Some(&description));
    }

    let (title, subtitle, detail) = nixe_media_model::tv::labels(item);
    card.title.set_text(&title);
    card.title.set_tooltip_text(Some(&title));
    card.meta.set_text(&subtitle);
    if !detail.is_empty() {
        let label = widgets::label(&detail, "card-meta");
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_max_width_chars(1);
        card.root.append(&label);
    }
    crate::presentation::register(&art, &card);
    (art, card)
}
fn grid() -> (gtk::ScrolledWindow, gio::ListStore) {
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_bind(|_, obj| {
        let row = obj.downcast_ref::<gtk::ListItem>().expect("grid item");
        let Some(value) = row.item().and_downcast::<glib::BoxedAnyObject>() else { return };
        let item = value.borrow::<MediaItem>();
        let (art, card) = poster_card(&item, false);
        media::set_progress(
            &card.strip,
            item.duration_ms.filter(|n| *n > 0).map_or(0.0, |d| (item.watch.position_ms as f64 / d as f64).clamp(0.0, 1.0)),
        );
        artwork::load_poster(&art, &item, 420, 630);
        row.set_child(Some(&card.root));
    });
    factory.connect_unbind(|_, obj| {
        if let Some(row) = obj.downcast_ref::<gtk::ListItem>() {
            row.set_child(None::<&gtk::Widget>);
        }
    });
    let view = gtk::GridView::new(Some(gtk::NoSelection::new(Some(store.clone()))), Some(factory));
    view.add_css_class("poster-grid");
    view.set_min_columns(2);
    view.set_max_columns(12);
    view.set_single_click_activate(true);
    let data = store.clone();
    view.connect_activate(move |_, position| {
        if let Some(obj) = data.item(position).and_downcast::<glib::BoxedAnyObject>() {
            let item = obj.borrow::<MediaItem>().clone();
            show_detail(item);
        }
    });
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::External)
        .vexpand(true)
        .child(&view)
        .build();
    (scroll, store)
}
fn set(store: &gio::ListStore, items: Vec<MediaItem>) {
    let old: Vec<_> = (0..store.n_items())
        .filter_map(|n| store.item(n).and_downcast::<glib::BoxedAnyObject>())
        .map(|value| value.borrow::<MediaItem>().clone())
        .collect();
    let prefix = old.iter().zip(&items).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..].iter().rev().zip(items[prefix..].iter().rev()).take_while(|(a, b)| a == b).count();
    let data: Vec<_> = items[prefix..items.len() - suffix].iter().cloned().map(glib::BoxedAnyObject::new).collect();
    if prefix + suffix != old.len() || !data.is_empty() {
        store.splice(prefix as u32, (old.len() - prefix - suffix) as u32, &data);
    }
}
pub fn build(stack: &gtk::Stack) {
    crate::rating_icons::install();
    let home = widgets::page("home");
    stack.add_named(&home.root, Some("home"));
    let discover = widgets::page("discover");
    stack.add_named(&discover.root, Some("discover"));
    let discover_filter = widgets::dropdown(&servers::filters(), &prefs::get().library_filter);
    discover_filter.set_tooltip_text(Some("Filter discovery by provider or library"));
    discover_filter.connect_selected_notify(|dd| {
        if FILTERS_UPDATING.with(|u| u.get()) {
            return;
        }
        let key = servers::filters().get(dd.selected() as usize).map(|(key, _)| key.clone()).unwrap_or_default();
        UI.with(|u| {
            if let Some(u) = u.borrow_mut().as_mut() {
                prefs::update(|p| p.library_filter = key.clone());
                u.filter = key;
            }
        });
        refresh_grids();
    });
    discover.body.append(&discover_filter);
    let discover_parts: Vec<_> = (0..3 + crate::catalog::DISCOVERY_CATEGORIES.len()).map(|_| widgets::vbox(12)).collect();
    for part in &discover_parts {
        discover.body.append(part);
    }
    crate::playlists::build(stack);
    let mut grids = HashMap::new();
    let mut filters = vec![discover_filter];
    for id in ["movies", "tv", "collections", "search"] {
        let page = widgets::vbox(0);
        let toolbar = widgets::hbox(8);
        toolbar.add_css_class("toolbar");
        let options = servers::filters();
        let filter = widgets::dropdown(&options, &prefs::get().library_filter);
        filter.set_tooltip_text(Some("Filter by library"));
        let labels = gtk::SignalListItemFactory::new();
        labels.connect_setup(|_, row| {
            let row = row.downcast_ref::<gtk::ListItem>().expect("dropdown item");
            let label = gtk::Label::new(None);
            label.set_max_width_chars(12);
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            row.set_child(Some(&label));
        });
        labels.connect_bind(|_, row| {
            let row = row.downcast_ref::<gtk::ListItem>().expect("dropdown item");
            if let (Some(value), Some(label)) =
                (row.item().and_downcast::<gtk::StringObject>(), row.child().and_downcast::<gtk::Label>())
            {
                label.set_text(&value.string());
            }
        });
        filter.set_factory(Some(&labels));
        filter.connect_selected_notify(move |dd| {
            if FILTERS_UPDATING.with(|u| u.get()) {
                return;
            }
            let key = servers::filters().get(dd.selected() as usize).map(|(key, _)| key.clone()).unwrap_or_default();
            let mut state = prefs::browse_state(id);
            state.source = key;
            set_browse(id, state);
        });
        toolbar.append(&filter);
        filters.push(filter.clone());
        let chips = crate::browse_ui::controls(id, &toolbar);
        let spacer = widgets::hbox(0);
        spacer.set_hexpand(true);
        toolbar.append(&spacer);
        toolbar.append(&nixe_ui::chrome::status_action("view-refresh-symbolic", "Refresh libraries", servers::refresh));
        page.append(&toolbar);
        page.append(&chips);
        let (view, store) = grid();
        page.append(&view);
        grids.insert(id, store);
        stack.add_named(&page, Some(id));
    }
    let details = widgets::page("details");
    stack.add_named(&details.root, Some("details"));
    let home_parts: Vec<_> = (0..5).map(|_| widgets::vbox(12)).collect();
    for part in &home_parts {
        home.body.append(part);
    }
    let (worker, receive) = crate::catalog::Worker::start();
    UI.with(|u| {
        *u.borrow_mut() = Some(Ui {
            grids,
            details,
            filter: prefs::get().library_filter,
            sort: prefs::get().library_sort,
            query: String::new(),
            filters,
            worker,
            generation: 0,
            page: "home".into(),
            dirty: true,
            scheduled: false,
            requested: false,
            server_scope: String::new(),
            home_data: Default::default(),
            home_parts,
            discover_parts,
            discover_data: Default::default(),
        });
    });
    glib::spawn_future_local(async move {
        while let Ok(view) = receive.recv().await {
            if crate::window::is_closing() {
                break;
            }
            let store = UI.with(|u| {
                let u = u.borrow();
                let u = u.as_ref()?;
                if !view.is_current(u.generation, &u.page) {
                    return None;
                }
                Some(u.grids.get(view.page.as_str()).cloned())
            });
            match store {
                Some(Some(store)) => {
                    let selected_sort = prefs::browse_state(&view.page).sort;
                    let sourced = matches!(
                        selected_sort.as_str(),
                        "imdb" | "tmdb" | "rt_critic" | "rt_audience" | "metacritic" | "critic" | "audience"
                    );
                    let rated = view
                        .items
                        .iter()
                        .filter(|i| {
                            if sourced {
                                crate::browse::rating(i, &selected_sort).is_some()
                            } else {
                                !i.metadata.ratings.is_empty()
                            }
                        })
                        .count();
                    crate::browse_ui::count(&view.page, view.items.len(), rated);
                    set(&store, view.items);
                    crate::rating_index::ensure(&view.page, &prefs::browse_state(&view.page).sort);
                }
                Some(None) => {
                    if let Some(home) = view.home {
                        if view.page == "discover" {
                            render_discover(home);
                        } else {
                            render_home(home);
                        }
                    }
                }
                None => {}
            }
        }
    });
    let notifications = crate::metadata::start(prefs::metadata_config());
    glib::spawn_future_local(async move {
        while notifications.recv().await.is_ok() {
            if window::is_closing() {
                break;
            }
            refresh();
            crate::rich_details::refresh_current();
        }
    });
    refresh();
}
pub fn search(query: &str) {
    UI.with(|u| {
        if let Some(u) = u.borrow_mut().as_mut() {
            u.query = query.trim().to_lowercase();
        }
    });
    refresh_grids();
}
fn refresh_grids() {
    schedule(false);
}
pub fn show(page: &str) {
    UI.with(|u| {
        if let Some(u) = u.borrow_mut().as_mut() {
            u.page = page.into();
            if matches!(page, "movies" | "tv" | "collections" | "search") {
                let state = prefs::browse_state(page);
                u.filter = state.source;
                u.sort = state.sort;
            }
        }
    });
    crate::rating_index::ensure(page, &prefs::browse_state(page).sort);
    crate::browse_ui::sync(page);
    schedule(false);
}
pub fn cancel() {
    RENDERED.with(|r| r.borrow_mut().take());
    crate::metadata::cancel();
    crate::rating_index::cancel();
    DETAIL_GENERATION.with(|g| g.set(g.get().wrapping_add(1)));
    CURRENT.with(|c| c.borrow_mut().take());
    HISTORY.with(|h| h.borrow_mut().clear());
    UI.with(|u| {
        if let Some(u) = u.borrow_mut().as_mut() {
            u.generation += 1;
            u.worker.cancel();
        }
    });
}
fn schedule(changed: bool) {
    if window::is_closing() {
        return;
    }
    let schedule = UI.with(|u| {
        let mut u = u.borrow_mut();
        let Some(u) = u.as_mut() else {
            return false;
        };
        let p = prefs::get();
        let scope = serde_json::to_string(&(p.plex.map(|p| p.server), p.jellyfin.map(|p| p.server))).unwrap_or_default();
        if !changed || scope != u.server_scope {
            u.generation = u.generation.wrapping_add(1);
        }
        u.server_scope = scope;
        u.dirty |= changed;
        if u.scheduled {
            return false;
        }
        u.scheduled = true;
        true
    });
    if schedule {
        let delay = UI.with(|u| if u.borrow().as_ref().is_some_and(|u| u.requested) { 250 } else { 0 });
        glib::timeout_add_local_once(std::time::Duration::from_millis(delay), || {
            if window::is_closing() {
                return;
            }
            update_filters();
            let options = UI.with(|u| {
                let mut u = u.borrow_mut();
                let u = u.as_mut()?;
                u.scheduled = false;
                if !matches!(u.page.as_str(), "home" | "discover" | "movies" | "tv" | "collections" | "search") {
                    return None;
                }
                u.requested = true;
                let items = u.dirty.then(servers::items);
                u.dirty = false;
                let online =
                    [plex::client().map(|c| c.server_id().clone()), crate::jellyfin::client().map(|c| c.server_id().clone())]
                        .into_iter()
                        .flatten()
                        .collect();
                Some((
                    u.worker.clone(),
                    crate::catalog::Request {
                        generation: u.generation,
                        items,
                        page: u.page.clone(),
                        filter: u.filter.clone(),
                        query: u.query.clone(),
                        online,
                        cache_scope: (u.page != "search").then(|| {
                            let p = prefs::get();
                            serde_json::to_string(&(
                                3,
                                prefs::browse_state(&u.page),
                                p.plex.map(|p| p.server),
                                p.jellyfin.map(|p| p.server),
                                &u.page,
                                &u.filter,
                                &u.sort,
                                &u.query,
                            ))
                            .unwrap_or_default()
                        }),
                        browse: prefs::browse_state(&u.page),
                        loading: servers::connection_states().iter().any(|s| s.busy || !s.connected),
                    },
                ))
            });
            if let Some((worker, request)) = options {
                worker.submit(request);
            }
        });
    }
}
thread_local! {
    static WATCH_DIRTY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// When watch changes last refreshed the library, and whether one is waiting.
    static WATCH_REFRESHED: Cell<Option<std::time::Instant>> = const { Cell::new(None) };
    static WATCH_WAITING: Cell<bool> = const { Cell::new(false) };
}
/// Seconds between library refreshes for watch progress. Playback reports every 10 s,
/// and each refresh re-reads every title.
const WATCH_REFRESH_SECS: u64 = 30;
pub fn watch_changed() {
    WATCH_DIRTY.with(|dirty| dirty.set(true));
    if prefs::get().last_section == "playing" || WATCH_WAITING.with(Cell::get) {
        return;
    }
    let since = WATCH_REFRESHED.with(Cell::get).map(|t| t.elapsed().as_secs());
    match since {
        Some(s) if s < WATCH_REFRESH_SECS => {
            WATCH_WAITING.with(|w| w.set(true));
            glib::timeout_add_seconds_local_once((WATCH_REFRESH_SECS - s) as u32, || {
                WATCH_WAITING.with(|w| w.set(false));
                if prefs::get().last_section != "playing" {
                    WATCH_REFRESHED.with(|t| t.set(Some(std::time::Instant::now())));
                    refresh_watch();
                }
            });
        }
        _ => {
            WATCH_REFRESHED.with(|t| t.set(Some(std::time::Instant::now())));
            refresh_watch();
        }
    }
}
pub fn refresh_watch() {
    if !WATCH_DIRTY.with(|dirty| dirty.replace(false)) {
        return;
    }
    refresh();
    let current = CURRENT.with(|c| c.borrow().clone());
    if let Some(item) = current.filter(|i| matches!(i.kind, MediaKind::Movie | MediaKind::Episode)) {
        let item = servers::latest_watch(item);
        CURRENT.with(|c| *c.borrow_mut() = Some(item.clone()));
        detail(item, vec![]);
    }
}
pub fn refresh() {
    schedule(true);
}
fn update_filters() {
    let state = UI.with(|u| u.borrow().as_ref().map(|u| (u.filters.clone(), u.filter.clone())));
    let Some((filters, key)) = state else { return };
    let options = servers::filters();
    let selected = options.iter().position(|(id, _)| id == &key).unwrap_or(0);
    if selected == 0 && !key.is_empty() {
        UI.with(|u| {
            if let Some(u) = u.borrow_mut().as_mut() {
                u.filter.clear();
                prefs::update(|p| p.library_filter.clear());
            }
        })
    }
    let names: Vec<_> = options.into_iter().map(|(_, label)| label).collect();
    FILTERS_UPDATING.with(|updating| updating.set(true));
    for filter in filters {
        let current: Vec<String> = filter
            .model()
            .map(|m| {
                (0..m.n_items())
                    .filter_map(|i| m.item(i).and_downcast::<gtk::StringObject>().map(|s| s.string().to_string()))
                    .collect()
            })
            .unwrap_or_default();
        if current != names {
            let strings: Vec<&str> = names.iter().map(String::as_str).collect();
            filter.set_model(Some(&gtk::StringList::new(&strings)));
        }
        filter.set_selected(selected as u32);
    }
    FILTERS_UPDATING.with(|updating| updating.set(false));
}
fn clear(body: &gtk::Box) {
    while let Some(child) = body.first_child() {
        body.remove(&child);
    }
}
fn render_discover(data: crate::catalog::Home) {
    let state = UI.with(|u| {
        let mut u = u.borrow_mut();
        let u = u.as_mut()?;
        let old = std::mem::replace(&mut u.discover_data, data.clone());
        Some((u.discover_parts.clone(), old))
    });
    if let Some((parts, old)) = state {
        for (index, title, items, previous) in [
            (0, "Unwatched movies", &data.continuing, &old.continuing),
            (1, "Shows to start", &data.movies, &old.movies),
            (2, "Rediscover older movies", &data.tv, &old.tv),
        ] {
            if items != previous || parts[index].first_child().is_none() {
                row(&parts[index], title, items.clone());
            }
        }
        for (index, title) in crate::catalog::DISCOVERY_CATEGORIES.iter().enumerate() {
            let items = data.discovery.get(index).cloned().unwrap_or_default();
            let previous = old.discovery.get(index).cloned().unwrap_or_default();
            if items != previous || parts[index + 3].first_child().is_none() {
                row(&parts[index + 3], title, items);
            }
        }
    }
}
fn render_home(data: crate::catalog::Home) {
    let state = UI.with(|u| {
        let mut u = u.borrow_mut();
        let u = u.as_mut()?;
        let old = std::mem::replace(&mut u.home_data, data.clone());
        Some((u.home_parts.clone(), old))
    });
    let Some((parts, old)) = state else {
        return;
    };
    clear(&parts[0]);
    if !servers::any_configured() {
        parts[0].append(&widgets::empty_state(
            "network-server-symbolic",
            "Your movies and shows, together.",
            "Connect Plex or Jellyfin to browse and play your library.",
            Some(("Connect Plex", Box::new(plex::connect_dialog))),
        ));
        let connect = gtk::Button::with_label("Connect Jellyfin");
        connect.set_halign(gtk::Align::Center);
        connect.connect_clicked(|_| crate::jellyfin::connect_dialog());
        parts[0].append(&connect);
    } else {
        for state in servers::connection_states() {
            let line = widgets::hbox(8);
            let text = widgets::label(&state.message, "dim");
            text.set_wrap(true);
            text.set_hexpand(true);
            line.append(&text);
            if !state.connected && !state.busy {
                let retry = nixe_ui::widgets::icon_button("view-refresh-symbolic", "Try again");
                let provider = state.provider;
                retry.connect_clicked(move |_| match provider {
                    ProviderKind::Plex => plex::refresh(),
                    ProviderKind::Jellyfin => crate::jellyfin::refresh(),
                });
                line.append(&retry);
                let login = nixe_ui::widgets::icon_button("network-server-symbolic", "Sign in again");
                login.connect_clicked(move |_| match provider {
                    ProviderKind::Plex => plex::connect_dialog(),
                    ProviderKind::Jellyfin => crate::jellyfin::connect_dialog(),
                });
                line.append(&login);
            }
            parts[0].append(&line);
        }
    }
    if data.featured != old.featured || parts[1].first_child().is_none() {
        clear(&parts[1]);
        if let Some(item) = &data.featured {
            parts[1].append(&hero(item, true));
        } else if servers::any_configured() {
            parts[1].append(&widgets::label("Your library will appear here as it loads.", "dim"));
        }
    }
    for (index, heading, current, previous) in [
        (2, "Continue watching", &data.continuing, &old.continuing),
        (3, "Recently added movies", &data.movies, &old.movies),
        (4, "Recently added TV", &data.tv, &old.tv),
    ] {
        if current != previous || parts[index].first_child().is_none() {
            row(&parts[index], heading, current.clone());
        }
    }
}
fn hero(item: &MediaItem, actions: bool) -> gtk::Overlay {
    let art = Artwork::new(1, 360, "video-display-symbolic");
    art.root.add_css_class("theater-hero");
    if actions {
        art.root.add_css_class("theater-hero-home");
    }
    art.root.set_hexpand(true);
    art.root.set_halign(gtk::Align::Fill);
    art.picture.set_size_request(-1, -1);
    art.picture.set_hexpand(true);
    art.picture.set_halign(gtk::Align::Fill);
    art.placeholder.set_size_request(-1, 360);
    artwork::load(&art, &item.id.server, item.backdrop.as_deref().or(item.poster.as_deref()), 1280, 720);
    let shade = widgets::vbox(0);
    shade.add_css_class("theater-hero-shade");
    shade.set_can_target(false);
    shade.set_hexpand(true);
    shade.set_vexpand(true);
    art.root.add_overlay(&shade);
    let content = widgets::vbox(12);
    content.add_css_class("theater-hero-content");
    content.set_valign(gtk::Align::End);
    content.set_hexpand(true);
    let eyebrow = widgets::label(
        if item.watch.position_ms > 0 && !item.watch.watched { "CONTINUE WATCHING" } else { "FEATURED" },
        "theater-eyebrow",
    );
    content.append(&eyebrow);
    let title = widgets::label(&item.title, "theater-hero-title");
    title.set_wrap(true);
    title.set_lines(2);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.set_max_width_chars(1);
    content.append(&title);
    let metadata = [
        item.year.map(|year| year.to_string()),
        item.duration_ms.map(|d| format!("{} min", d / 60000)),
        Some(source_label(item)),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ");
    let metadata = widgets::label(&metadata, "theater-hero-meta");
    metadata.set_ellipsize(gtk::pango::EllipsizeMode::End);
    metadata.set_max_width_chars(1);
    content.append(&metadata);
    let summary = widgets::label(&item.summary, "theater-hero-summary");
    summary.set_wrap(true);
    summary.set_lines(2);
    summary.set_ellipsize(gtk::pango::EllipsizeMode::End);
    summary.set_max_width_chars(1);
    content.append(&summary);
    if actions {
        let buttons = widgets::hbox(8);
        let play = widgets::labeled_button(
            "media-playback-start-symbolic",
            if item.watch.position_ms > 0 && !item.watch.watched { "Resume" } else { "Play" },
        );
        play.add_css_class("suggested-action");
        let current = item.clone();
        play.connect_clicked(move |_| {
            if let Some((_, choice)) = playback_choices(&current).first() {
                start_choice(&current, choice, false, "auto");
            } else {
                show_detail(current.clone());
            }
        });
        buttons.append(&play);
        let details = gtk::Button::with_label("Details");
        let current = item.clone();
        details.connect_clicked(move |_| show_detail(current.clone()));
        buttons.append(&details);
        content.append(&buttons);
    }
    art.root.add_overlay(&content);
    art.root
}
struct HomeRow {
    content: gtk::Box,
    cards: HashMap<nixe_media_model::ItemId, (MediaItem, gtk::Button)>,
}
thread_local! { static ROWS: RefCell<HashMap<String, HomeRow>> = RefCell::new(HashMap::new()); }
fn row(body: &gtk::Box, heading: &str, items: Vec<MediaItem>) {
    body.set_visible(!items.is_empty());
    ROWS.with(|rows| {
        let mut rows = rows.borrow_mut();
        let row = rows.entry(heading.into()).or_insert_with(|| {
            body.append(&widgets::label(&heading.to_uppercase(), "group-title"));
            let content = widgets::hbox(12);
            let scroll = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::External)
                .vscrollbar_policy(gtk::PolicyType::Never)
                .child(&content)
                .build();
            body.append(&scroll);
            HomeRow { content, cards: HashMap::new() }
        });
        let mut previous = None;
        let mut cards = HashMap::new();
        for item in items {
            let old = row.cards.remove(&item.id);
            let button = if old.as_ref().is_some_and(|(old, _)| old == &item) {
                old.unwrap().1
            } else {
                if let Some((_, old)) = old {
                    row.content.remove(&old);
                }
                let (art, card) = poster_card(&item, true);
                artwork::load_poster(&art, &item, 420, 630);
                media::set_progress(
                    &card.strip,
                    item.duration_ms
                        .filter(|d| *d > 0)
                        .map_or(0.0, |d| (item.watch.position_ms as f64 / d as f64).clamp(0.0, 1.0)),
                );
                let button = gtk::Button::new();
                button.add_css_class("wide-card");
                button.set_child(Some(&card.root));
                let selected = item.clone();
                button.connect_clicked(move |_| show_detail(selected.clone()));
                row.content.append(&button);
                button
            };
            row.content.reorder_child_after(&button, previous.as_ref());
            previous = Some(button.clone());
            cards.insert(item.id.clone(), (item, button));
        }
        for (_, (_, old)) in row.cards.drain() {
            row.content.remove(&old);
        }
        row.cards = cards;
    });
}
pub fn show_detail(item: MediaItem) {
    if window::page() != "details" {
        HISTORY.with(|h| h.borrow_mut().clear());
        CURRENT.with(|c| c.borrow_mut().take());
    }
    crate::metadata::prioritize(item.clone());
    let item = servers::latest_watch(item);
    CURRENT.with(|current| {
        if let Some(previous) = current.borrow_mut().replace(item.clone())
            && previous.id != item.id {
                HISTORY.with(|h| h.borrow_mut().push(previous));
            }
    });
    let generation = DETAIL_GENERATION.with(|g| {
        let next = g.get().wrapping_add(1);
        g.set(next);
        next
    });
    window::navigate("details");
    detail(item.clone(), vec![]);
    window::status("Loading details…");
    let client = servers::client(&item.id.server);
    let online = client.is_some();
    let id = item.id.clone();
    let started = servers::revision();
    let (send, receive) = async_channel::bounded(2);
    std::thread::spawn(move || {
        let mut cache = servers::database().ok();
        if let Some((full, children)) = cache.as_ref().and_then(|cache| cache.details(&item.id).ok().flatten())
            && send.send_blocking(Ok((full, children, true))).is_err() {
                return;
            }
        let result = (|| {
            let client = client.ok_or(ProviderError::Offline)?;
            let mut full = client.detail(&item.id)?;
            if full.library_id.is_empty() {
                full.library_id = item.library_id;
            }
            let mut children = vec![];
            if matches!(full.kind, MediaKind::Series | MediaKind::Season | MediaKind::Collection) {
                let mut next = None;
                loop {
                    if send.is_closed() {
                        return Err(ProviderError::Failed("Details cancelled".into()));
                    }
                    let page = client.child_page(&full.id, &full.library_id, next.as_deref())?;
                    children.extend(page.items);
                    next = page.next;
                    if children.len() > 50_000 {
                        return Err(ProviderError::InvalidResponse);
                    }
                    if next.is_none() {
                        break;
                    }
                }
                children.sort_by_key(|i| (i.season_number, i.episode_number));
            }
            if let Some(cache) = &mut cache {
                let _ = cache.save_details(&full, &children, started);
            }
            crate::metadata::store_provider_ratings(std::slice::from_ref(&full));
            Ok((full, children, false))
        })();
        let _ = send.send_blocking(result);
    });
    glib::spawn_future_local(async move {
        let mut cached = false;
        while let Ok(result) = receive.recv().await {
            if DETAIL_GENERATION.with(|g| g.get()) != generation
                || !CURRENT.with(|c| c.borrow().as_ref().is_some_and(|i| i.id == id))
                || !servers::configured(&id.server)
            {
                break;
            }
            match result {
                Ok((item, children, from_cache)) => {
                    cached |= from_cache;
                    if !from_cache {
                        servers::details_watch(&item, &children, started);
                    }
                    CURRENT.with(|c| *c.borrow_mut() = Some(item.clone()));
                    detail(item, children);
                    window::status(if from_cache {
                        if online {
                            "Showing cached details · refreshing…"
                        } else {
                            "Server is offline · showing cached details"
                        }
                    } else {
                        "Details updated"
                    });
                }
                Err(error) => {
                    window::status(&if cached { format!("{error} · showing cached details") } else { error.to_string() })
                }
            }
        }
    });
}
pub fn back() {
    DETAIL_GENERATION.with(|g| g.set(g.get().wrapping_add(1)));
    let previous = HISTORY.with(|h| h.borrow_mut().pop());
    CURRENT.with(|c| c.borrow_mut().take());
    if let Some(item) = previous {
        show_detail(item);
    } else {
        window::back_from_details();
    }
}
fn detail(mut item: MediaItem, children: Vec<MediaItem>) {
    crate::metadata::apply(&mut item);
    let item = servers::latest_watch(item);
    let rendered = (item.clone(), children.clone(), crate::metadata::personal(&item));
    if RENDERED.with(|r| r.borrow().as_ref() == Some(&rendered)) {
        return;
    }
    RENDERED.with(|r| *r.borrow_mut() = Some(rendered));
    CURRENT.with(|c| *c.borrow_mut() = Some(item.clone()));
    let body = UI.with(|u| u.borrow().as_ref().map(|u| u.details.body.clone()));
    let Some(body) = body else { return };
    while let Some(child) = body.first_child() {
        body.remove(&child);
    }
    let back_button = widgets::labeled_button("go-previous-symbolic", "Back");
    back_button.set_halign(gtk::Align::Start);
    back_button.set_tooltip_text(Some("Back (Alt+Left)"));
    back_button.connect_clicked(|_| back());
    body.append(&back_button);
    body.append(&hero(&item, false));
    crate::rich_details::intro(&body, &item);
    let heading = widgets::hbox(24);
    let art = Artwork::new(150, 225, "video-x-generic-symbolic");
    artwork::load_poster(&art, &item, 420, 630);
    heading.append(&art.root);
    let text = widgets::vbox(12);
    text.add_css_class("theater-detail-info");
    text.set_hexpand(true);
    if matches!(item.kind, MediaKind::Movie | MediaKind::Episode | MediaKind::Season | MediaKind::Series) {
        let row = widgets::hbox(4);
        if matches!(item.kind, MediaKind::Movie | MediaKind::Episode) {
            let add = nixe_ui::widgets::icon_button("list-add-symbolic", "Add to a playlist");
            let selected = item.clone();
            add.connect_clicked(move |_| crate::playlists::add_dialog(selected.clone()));
            row.append(&add);
        }
        row.append(&watched_button(&item));
        text.append(&row);
    }
    if matches!(item.kind, MediaKind::Movie | MediaKind::Episode) && !item.versions.is_empty() {
        let choices = Rc::new(playback_choices(&item));
        let options: Vec<_> = choices.iter().enumerate().map(|(index, (label, _))| (index.to_string(), label.clone())).collect();
        let version = Rc::new(Cell::new(0usize));
        let dd = widgets::dropdown(&options, "0");
        fit_dropdown(&dd);
        dd.set_tooltip_text(Some("Playback source and version"));
        let selected = version.clone();
        dd.connect_selected_notify(move |dd| selected.set(dd.selected() as usize));
        let selectors = widgets::vbox(8);
        selectors.append(&dd);
        let playback = Rc::new(RefCell::new("auto".to_owned()));
        let options =
            widgets::opts(&[("auto", "Automatic playback"), ("direct", "Direct Play"), ("transcode", "Transcode · 1080p")]);
        let mode = widgets::dropdown(&options, "auto");
        mode.set_tooltip_text(Some("Playback mode"));
        let selected = playback.clone();
        mode.connect_selected_notify(move |dd| {
            if let Some((id, _)) = options.get(dd.selected() as usize) {
                *selected.borrow_mut() = id.clone();
            }
        });
        selectors.append(&mode);
        let progress = choices.first().is_some_and(|(_, choice)| choice_has_progress(&item, choice));
        let actions = widgets::hbox(8);
        let play = widgets::labeled_button("media-playback-start-symbolic", if progress { "Resume" } else { "Play" });
        play.add_css_class("suggested-action");
        let (i, v, m, c) = (item.clone(), version.clone(), playback.clone(), choices.clone());
        play.connect_clicked(move |_| {
            if let Some((_, choice)) = c.get(v.get()) {
                start_choice(&i, choice, false, m.borrow().as_str());
            }
        });
        actions.append(&play);
        let start = nixe_ui::widgets::icon_button("media-skip-backward-symbolic", "Start over");
        start.set_visible(progress);
        let (i, v, m, c) = (item.clone(), version.clone(), playback.clone(), choices.clone());
        start.connect_clicked(move |_| {
            if let Some((_, choice)) = c.get(v.get()) {
                start_choice(&i, choice, true, m.borrow().as_str());
            }
        });
        actions.append(&start);
        let play_label =
            play.child().and_downcast::<gtk::Box>().and_then(|content| content.last_child()).and_downcast::<gtk::Label>();
        let weak_label = play_label.map(|label| label.downgrade());
        let weak_start = start.downgrade();
        let i = item.clone();
        dd.connect_selected_notify(move |dd| {
            let progress = choices.get(dd.selected() as usize).is_some_and(|(_, choice)| choice_has_progress(&i, choice));
            if let Some(label) = weak_label.as_ref().and_then(|label| label.upgrade()) {
                label.set_text(if progress { "Resume" } else { "Play" });
            }
            if let Some(start) = weak_start.upgrade() {
                start.set_visible(progress);
            }
        });
        text.append(&actions);
        let versions = gtk::Expander::new(Some("Source and playback options"));
        versions.set_child(Some(&selectors));
        text.append(&versions);
    }
    if item.kind == MediaKind::Series {
        let sources = logical_sources(&item);
        if sources.len() > 1 {
            let options: Vec<_> =
                sources.iter().enumerate().map(|(index, source)| (index.to_string(), source_label(source))).collect();
            let selected = sources.iter().position(|source| source.id == item.id).unwrap_or(0);
            let dd = widgets::dropdown(&options, &selected.to_string());
            fit_dropdown(&dd);
            dd.set_tooltip_text(Some("Library source"));
            let selected_id = item.id.clone();
            dd.connect_selected_notify(move |dd| {
                if let Some(source) = sources.get(dd.selected() as usize)
                    && source.id != selected_id {
                        show_detail(source.clone());
                    }
            });
            text.append(&dd);
        }
    }
    heading.append(&text);
    heading.add_css_class("theater-source-section");
    body.append(&heading);
    crate::rich_details::people(&body, &item);
    crate::rich_details::television(&body, &item, &children);
    if !children.is_empty() {
        if matches!(item.kind, MediaKind::Series | MediaKind::Season) {
            body.append(&widgets::label(if item.kind == MediaKind::Series { "SEASONS" } else { "EPISODES" }, "group-title"));
        }
        let (view, store) = grid();
        view.set_margin_bottom(32);
        view.set_min_content_height(400);
        set(&store, children.into_iter().map(servers::latest_watch).collect());
        body.append(&view);
    }
    crate::rich_details::related(&body, &item);
    crate::rich_details::media(&body, &logical_sources(&item));
}
fn logical_sources(item: &MediaItem) -> Vec<MediaItem> {
    crate::catalog::snapshot().sources(item).into_iter().filter(|i| servers::configured(&i.id.server)).collect()
}

fn source_label(item: &MediaItem) -> String {
    let provider = match item.id.server.provider {
        ProviderKind::Plex => "Plex",
        ProviderKind::Jellyfin => "Jellyfin",
    };
    let library = prefs::get()
        .plex
        .filter(|p| p.server == item.id.server)
        .and_then(|p| p.libraries.into_iter().find(|library| library.id == item.library_id))
        .map(|library| library.title);
    let library = library.or_else(|| {
        prefs::get()
            .jellyfin
            .filter(|p| p.server == item.id.server)
            .and_then(|p| p.libraries.into_iter().find(|l| l.id == item.library_id))
            .map(|l| l.title)
    });
    library.map(|library| format!("{provider} · {library}")).unwrap_or_else(|| provider.to_owned())
}
// The same constrained label factory as the library filter; long source names ellipsize.
pub(crate) fn fit_dropdown(dd: &gtk::DropDown) {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, row| {
        let row = row.downcast_ref::<gtk::ListItem>().expect("dropdown item");
        let label = gtk::Label::new(None);
        label.set_max_width_chars(16);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        row.set_child(Some(&label));
    });
    factory.connect_bind(|_, row| {
        let row = row.downcast_ref::<gtk::ListItem>().expect("dropdown item");
        if let (Some(value), Some(label)) =
            (row.item().and_downcast::<gtk::StringObject>(), row.child().and_downcast::<gtk::Label>())
        {
            label.set_text(&value.string());
            label.set_tooltip_text(Some(&value.string()));
        }
    });
    dd.set_factory(Some(&factory));
}
#[allow(clippy::large_enum_variant)]
enum PlaybackChoice {
    Automatic(String),
    Source(MediaItem, String),
}
fn playback_choices(item: &MediaItem) -> Vec<(String, PlaybackChoice)> {
    let mut sources = logical_sources(item);
    sources.sort_by_key(|source| servers::client(&source.id.server).is_none());
    let mut choices = vec![];
    let mut editions = std::collections::HashSet::new();
    for source in &sources {
        for version in &source.versions {
            if version.parts.is_empty() {
                continue;
            }
            let key = edition_key(version.edition.as_deref());
            if editions.insert(key.clone()) {
                choices.push((
                    format!("Automatic · {}", version.edition.as_deref().unwrap_or("Original")),
                    PlaybackChoice::Automatic(key),
                ));
            }
        }
    }
    let mut labels = HashMap::<String, usize>::new();
    for source in sources {
        for version in &source.versions {
            if version.parts.is_empty() {
                continue;
            }
            let mut label = [
                source_label(&source),
                version.edition.clone().unwrap_or_else(|| "Original".into()),
                version.height.map(|height| format!("{height}p")).unwrap_or_default(),
                version.video_codec.clone().unwrap_or_default(),
                if version.parts.len() > 1 { format!("{} parts", version.parts.len()) } else { Default::default() },
            ]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
            let copies = labels.entry(label.clone()).or_default();
            *copies += 1;
            if *copies > 1 {
                label.push_str(&format!(" · Copy {}", copies));
            }
            choices.push((label, PlaybackChoice::Source(source.clone(), version.id.clone())));
        }
    }
    choices
}
fn resolve_choice(item: &MediaItem, choice: &PlaybackChoice) -> Option<(MediaItem, String)> {
    match choice {
        PlaybackChoice::Source(source, version) => Some((source.clone(), version.clone())),
        PlaybackChoice::Automatic(edition) => {
            let mut candidates = vec![];
            for (index, source) in logical_sources(item).into_iter().map(servers::latest_watch).enumerate() {
                for (version_index, version) in source.versions.iter().enumerate() {
                    if servers::client(&source.id.server).is_some()
                        && !version.parts.is_empty()
                        && edition_key(version.edition.as_deref()) == *edition
                    {
                        let resume = !source.watch.watched && source.watch.position_ms > 0;
                        let rank = (
                            std::cmp::Reverse(resume),
                            std::cmp::Reverse(if resume { source.watch.updated_at } else { None }),
                            source.id != item.id,
                            index,
                            version_index,
                        );
                        candidates.push((rank, source.clone(), version.id.clone()));
                    }
                }
            }
            candidates.into_iter().min_by_key(|(rank, _, _)| *rank).map(|(_, source, version)| (source, version))
        }
    }
}
fn choice_has_progress(item: &MediaItem, choice: &PlaybackChoice) -> bool {
    resolve_choice(item, choice)
        .map(|(source, _)| servers::latest_watch(source))
        .is_some_and(|source| source.watch.position_ms > 0 && !source.watch.watched)
}
struct Fallback {
    sources: Vec<(MediaItem, String)>,
    from_start: bool,
    mode: String,
}
thread_local! {static FALLBACK:RefCell<Option<Fallback>>=const{RefCell::new(None)};}
pub fn clear_fallback() {
    FALLBACK.with(|state| state.borrow_mut().take());
}
pub fn try_next_source(position_ms: u64) -> bool {
    let next = FALLBACK.with(|state| {
        let mut state = state.borrow_mut();
        let fallback = state.as_mut()?;
        while !fallback.sources.is_empty() {
            let (source, version) = fallback.sources.remove(0);
            if servers::client(&source.id.server).is_some() {
                return Some((source, version, fallback.from_start, fallback.mode.clone()));
            }
        }
        *state = None;
        None
    });
    if let Some((source, version, from_start, mode)) = next {
        start_playback(source, version, from_start, &mode, Some(position_ms));
        true
    } else {
        false
    }
}
fn start_choice(item: &MediaItem, choice: &PlaybackChoice, from_start: bool, mode: &str) {
    crate::playlists::clear_queue();
    start_selected(item, choice, from_start, mode);
}
/// The episode after `episode` in its series, on the same server: the next number in
/// its season, else the first of a later season (specials, season 0, aren't next).
pub fn next_episode(episode: &MediaItem) -> Option<MediaItem> {
    if episode.kind != MediaKind::Episode {
        return None;
    }
    let series = episode.series.as_ref()?;
    let here = (episode.season_number?, episode.episode_number?);
    servers::items()
        .into_iter()
        .filter(|i| i.kind == MediaKind::Episode && i.series.as_ref() == Some(series) && i.id.server == episode.id.server)
        .filter_map(|i| Some(((i.season_number?, i.episode_number?), i)))
        .filter(|((s, _), _)| *s > 0 || here.0 == 0)
        .filter(|(at, _)| *at > here)
        .min_by_key(|(at, _)| *at)
        .map(|(_, i)| i)
}

/// After an episode ends: offer the next one, playing it after a countdown.
pub fn up_next(finished: &MediaItem) {
    if prefs::get().autoplay_next
        && let Some(next) = next_episode(finished)
    {
        crate::window::show_up_next(next);
    }
}

/// A button that marks `item` watched or unwatched on its server (a series or season
/// with all its episodes; the server does that).
fn watched_button(item: &MediaItem) -> gtk::Button {
    let watched = item.watch.watched;
    let b = nixe_ui::widgets::icon_button(
        if watched { "edit-undo-symbolic" } else { "object-select-symbolic" },
        if watched { "Mark unwatched" } else { "Mark watched" },
    );
    let item = item.clone();
    b.connect_clicked(move |b| {
        b.set_sensitive(false);
        set_watched(&item, !watched);
    });
    b
}

/// Mark `item` watched (or not) on its server, then update the library with what the
/// server reports.
pub fn set_watched(item: &MediaItem, watched: bool) {
    let Some(client) = servers::client(&item.id.server) else {
        window::status("This server is offline. Reconnect to change watch state.");
        return;
    };
    let (id, library) = (item.id.clone(), item.library_id.clone());
    let revision = servers::revision();
    let state = nixe_media_model::WatchState { position_ms: 0, watched, updated_at: None };
    nixe_core::cmd::background(
        move || {
            let mut result = client.apply_watch(&id, &state)?;
            if result.library_id.is_empty() {
                result.library_id = library;
            }
            if let Ok(cache) = servers::database() {
                let _ = cache.record_playback(&result, revision);
            }
            Ok::<_, ProviderError>(result)
        },
        move |result| match result {
            Ok(item) => {
                window::status(if watched { "Marked watched" } else { "Marked unwatched" });
                servers::playback_watch(item, revision);
                rerender_current();
            }
            Err(e) => window::status(&e.to_string()),
        },
    );
}

/// Play `item` with its first playable source, from the start or resuming; false when
/// none is online. (Nixe Assistant's `theater.play`.)
pub fn play_item(item: &MediaItem, from_start: bool) -> bool {
    crate::playlists::clear_queue();
    match playback_choices(item).first() {
        Some((_, choice)) if resolve_choice(item, choice).is_some() => {
            start_selected(item, choice, from_start, "auto");
            true
        }
        _ => false,
    }
}

pub fn play_queued(item: &MediaItem) -> bool {
    if let Some((_, choice)) = playback_choices(item).first()
        && resolve_choice(item, choice).is_some() {
            start_selected(item, choice, false, "auto");
            return true;
        }
    false
}
fn start_selected(item: &MediaItem, choice: &PlaybackChoice, from_start: bool, mode: &str) {
    clear_fallback();
    if let Some((source, version)) = resolve_choice(item, choice) {
        if let PlaybackChoice::Automatic(edition) = choice {
            let mut remaining = vec![];
            for candidate in logical_sources(item) {
                for candidate_version in &candidate.versions {
                    if edition_key(candidate_version.edition.as_deref()) == *edition
                        && !candidate_version.parts.is_empty()
                        && (candidate.id != source.id || candidate_version.id != version)
                    {
                        remaining.push((candidate.clone(), candidate_version.id.clone()));
                    }
                }
            }
            remaining.truncate(63);
            FALLBACK.with(|state| *state.borrow_mut() = Some(Fallback { sources: remaining, from_start, mode: mode.into() }));
        }
        start_playback(source, version, from_start, mode, None);
    } else {
        window::status("No playable source is available for this edition.");
    }
}
fn start_playback(item: MediaItem, version: String, from_start: bool, choice: &str, resume: Option<u64>) {
    let item = servers::latest_watch(item);
    let Some(client) = servers::client(&item.id.server) else {
        window::status("This source is offline. Reconnect or choose another source.");
        return;
    };
    if client.server_id() != &item.id.server {
        window::status("The selected source is offline. Choose another source.");
        return;
    }
    let Some(request_id) = window::begin_playback_request(item.id.server.clone()) else { return };
    window::status(&format!("Opening {} video…", client.label()));
    let resume_ms = resume.unwrap_or(if from_start || item.watch.watched { 0 } else { item.watch.position_ms });
    let mode = if choice == "transcode" { PlaybackMode::Transcode } else { PlaybackMode::Direct };
    let auto_transcode = choice == "auto";
    let (c, i, selected) = (client.clone(), item.clone(), version.clone());
    nixe_core::cmd::background(
        move || {
            let resolve = |mode| {
                c.resolve(PlaybackRequest {
                    burn_subtitle: None,
                    item: i.id.clone(),
                    version_id: selected.clone(),
                    position_ms: resume_ms.saturating_sub(3000),
                    mode,
                })
            };
            match resolve(mode) {
                Err(_) if auto_transcode && mode == PlaybackMode::Direct => resolve(PlaybackMode::Transcode),
                result => result,
            }
        },
        move |result| match result {
            Ok(source) => window::play_remote(source, item, client, resume_ms, version, auto_transcode, request_id),
            Err(e) => {
                if window::accepts_playback_request(request_id) && !try_next_source(resume_ms) {
                    window::status(&e.to_string());
                }
            }
        },
    );
}

pub fn set_browse(page: &str, state: crate::browse::State) {
    prefs::update(|p| {
        p.browse.insert(page.into(), state.clone());
    });
    UI.with(|u| {
        if let Some(u) = u.borrow_mut().as_mut()
            && u.page == page {
                u.filter = state.source;
                u.sort = state.sort;
            }
    });
    crate::rating_index::ensure(page, &prefs::browse_state(page).sort);
    refresh_grids();
}
pub fn current() -> Option<MediaItem> {
    CURRENT.with(|c| c.borrow().clone())
}
pub fn rerender_current() {
    let item = current();
    if let Some(mut item) = item {
        let previous = item.metadata.clone();
        crate::metadata::apply(&mut item);
        if item.metadata != previous && prefs::get().last_section == "details" {
            show_detail_cached(item);
        }
    }
}
fn show_detail_cached(mut item: MediaItem) {
    crate::metadata::apply(&mut item);
    CURRENT.with(|c| *c.borrow_mut() = Some(item.clone()));
    let id = item.id.clone();
    let generation = DETAIL_GENERATION.with(|g| g.get());
    nixe_core::cmd::background(
        move || {
            servers::database()
                .ok()
                .and_then(|db| db.details(&id).ok().flatten())
                .map(|(_, children)| children)
                .unwrap_or_default()
        },
        move |children| {
            if DETAIL_GENERATION.with(|g| g.get()) == generation
                && CURRENT.with(|c| c.borrow().as_ref().is_some_and(|i| i.id == item.id))
                && prefs::get().last_section == "details"
            {
                detail(item, children);
            }
        },
    );
}
pub fn detail_row(body: &gtk::Box, title: &str, items: Vec<MediaItem>) {
    if items.is_empty() {
        return;
    }
    let section = widgets::vbox(12);
    section.add_css_class("theater-content-section");
    section.append(&widgets::label(&title.to_uppercase(), "group-title"));
    let content = widgets::hbox(16);
    for item in items.into_iter().take(24) {
        let (art, card) = poster_card(&item, false);
        artwork::load_poster(&art, &item, 420, 630);
        let button = gtk::Button::new();
        button.add_css_class("poster-card");
        button.set_child(Some(&card.root));
        button.connect_clicked(move |_| show_detail(item.clone()));
        content.append(&button);
    }
    section.append(
        &gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .child(&content)
            .build(),
    );
    body.append(&section);
}
pub fn detail_generation() -> u64 {
    DETAIL_GENERATION.with(|g| g.get())
}
pub fn begin_person() -> Option<(gtk::Box, u64)> {
    RENDERED.with(|r| r.borrow_mut().take());
    DETAIL_GENERATION.with(|g| g.set(g.get().wrapping_add(1)));
    if let Some(item) = CURRENT.with(|c| c.borrow_mut().take()) {
        HISTORY.with(|h| h.borrow_mut().push(item));
    }
    let body = UI.with(|u| u.borrow().as_ref().map(|u| u.details.body.clone()))?;
    clear(&body);
    let back = widgets::labeled_button("go-previous-symbolic", "Back");
    back.set_halign(gtk::Align::Start);
    back.connect_clicked(|_| crate::library::back());
    body.append(&back);
    window::navigate("details");
    Some((body, detail_generation()))
}

pub fn play_trailer(item: &MediaItem) {
    crate::playlists::clear_queue();
    if !play_queued(item) {
        window::status("This trailer is unavailable from the current server.");
    }
}

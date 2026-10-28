use crate::{playback::Playback, prefs, settings, widgets};
use gtk::{gdk, glib, prelude::*};
use nyx_ui::{chrome, settings_card};
use std::{cell::RefCell, rc::Rc};

struct Ui {
    window: gtk::ApplicationWindow,
    overlay: gtk::Overlay,
    nav: gtk::Box,
    stack: gtk::Stack,
    search: gtk::SearchEntry,
    /// The sidebar's foot: what the servers and the app are doing.
    status: gtk::Label,
    player: Rc<RefCell<Playback>>,
    /// The header (hidden in full screen) and its subtitle, the page you're on.
    header: gtk::WindowHandle,
    subtitle: gtk::Label,
    buttons: Vec<(&'static str, &'static str, gtk::Button)>,
}
thread_local! {
    /// The "Up next" card while it's counting down, and its timer.
    static UP_NEXT: RefCell<Option<(gtk::Box, glib::SourceId)>> = const { RefCell::new(None) };
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
    static NAV_HISTORY: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static NAV_FORWARD: RefCell<Vec<(String, Option<nyx_media_model::MediaItem>)>> = const { RefCell::new(Vec::new()) };
    static SEARCH_ORIGIN: RefCell<String> = RefCell::new(String::from("home"));
    static SEARCH_CLEARING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static CLOSING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
pub fn is_closing() -> bool {
    CLOSING.with(|c| c.get())
}
pub fn window() -> Option<gtk::ApplicationWindow> {
    UI.with(|u| u.borrow().as_ref().map(|u| u.window.clone()))
}
pub fn overlay() -> Option<gtk::Overlay> {
    UI.with(|u| u.borrow().as_ref().map(|u| u.overlay.clone()))
}
pub fn status(text: &str) {
    UI.with(|u| {
        if let Some(u) = u.borrow().as_ref() {
            u.status.set_text(text);
        }
    });
}

/// Seconds the "Up next" card counts down before playing.
const UP_NEXT_SECS: u32 = 10;

/// Offer `next` over the player: it plays when the countdown ends, or now, or not at all.
pub fn show_up_next(next: nyx_media_model::MediaItem) {
    cancel_up_next();
    let Some(overlay) = overlay() else { return };
    let card = widgets::hbox(10);
    card.add_css_class("toast");
    card.add_css_class("up-next");
    card.set_halign(gtk::Align::End);
    card.set_valign(gtk::Align::End);
    card.set_margin_end(24);
    card.set_margin_bottom(96);
    let text = widgets::vbox(2);
    let name = match (next.season_number, next.episode_number) {
        (Some(s), Some(e)) => format!("S{s} · E{e} · {}", next.title),
        _ => next.title.clone(),
    };
    text.append(&widgets::label(&format!("Up next: {name}"), ""));
    let count = widgets::label(&format!("Plays in {UP_NEXT_SECS} s"), "dim");
    text.append(&count);
    card.append(&text);
    let play = nyx_ui::widgets::icon_button("media-skip-forward-symbolic", "Play it now");
    let stop = nyx_ui::widgets::icon_button("window-close-symbolic", "Don't play it");
    card.append(&play);
    card.append(&stop);
    overlay.add_overlay(&card);
    let left = std::cell::Cell::new(UP_NEXT_SECS);
    let item = next.clone();
    let timer = glib::timeout_add_seconds_local(1, move || {
        let n = left.get().saturating_sub(1);
        left.set(n);
        if n == 0 {
            UP_NEXT.with(|u| u.borrow_mut().take()).inspect(|(c, _)| remove_card(c));
            crate::library::play_queued(&item);
            return glib::ControlFlow::Break;
        }
        count.set_text(&format!("Plays in {n} s"));
        glib::ControlFlow::Continue
    });
    let item = next;
    play.connect_clicked(move |_| {
        cancel_up_next();
        crate::library::play_queued(&item);
    });
    stop.connect_clicked(|_| cancel_up_next());
    UP_NEXT.with(|u| *u.borrow_mut() = Some((card, timer)));
}

/// Take the "Up next" card away without playing.
pub fn cancel_up_next() {
    if let Some((card, timer)) = UP_NEXT.with(|u| u.borrow_mut().take()) {
        timer.remove();
        remove_card(&card);
    }
}

fn remove_card(card: &gtk::Box) {
    if let Some(o) = card.parent().and_downcast::<gtk::Overlay>() {
        o.remove_overlay(card);
    }
}

pub fn build(app: &gtk::Application) {
    let window =
        gtk::ApplicationWindow::builder().application(app).title("Theater").default_width(1120).default_height(800).build();
    window.add_css_class("theater-window");
    window.add_css_class("playback-window");
    window.set_icon_name(Some("io.github.design_nexus.nyx.Theater"));
    window.set_titlebar(Some(&gtk::Box::new(gtk::Orientation::Horizontal, 0)));
    nyx_ui::keys::install(
        &window,
        nyx_ui::keys::Common {
            settings: Some(settings::open),
            shortcuts: Some(show_shortcuts),
            close: true,
            ..Default::default()
        },
    );
    let nav = widgets::vbox(0);
    nav.add_css_class("settings-navigation");
    nav.set_hexpand(false);
    let list = widgets::vbox(0);
    list.set_hexpand(false);
    let mut buttons = Vec::new();
    for (id, title, icon) in [
        ("home", "Home", "go-home-symbolic"),
        ("discover", "Discover", "system-search-symbolic"),
        ("movies", "Movies", "video-x-generic-symbolic"),
        ("tv", "TV shows", "video-display-symbolic"),
        ("collections", "Collections", "folder-symbolic"),
        ("playlists", "Playlists", "view-list-symbolic"),
    ] {
        let button = gtk::Button::new();
        button.add_css_class("nav-item");
        button.set_tooltip_text(Some(title));
        let content = widgets::hbox(12);
        content.append(&gtk::Image::from_icon_name(icon));
        let label = widgets::label(title, "nav-label");
        label.add_css_class("compact-hide");
        label.set_hexpand(true);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        content.append(&label);
        button.set_child(Some(&content));
        button.connect_clicked(move |_| navigate(id));
        list.append(&button);
        buttons.push((id, title, button));
    }
    nav.append(
        &gtk::ScrolledWindow::builder()
            .hexpand(false)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::External)
            .child(&list)
            .build(),
    );
    let stack = gtk::Stack::new();
    stack.set_hexpand(true);
    stack.set_vexpand(true);
    stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    stack.set_transition_duration(if prefs::get().reduce_motion { 0 } else { 160 });
    crate::library::build(&stack);
    let player = Playback::attach(&window, Rc::new(status));
    let playing = crate::now_playing::build(&player);
    stack.add_named(&playing, Some("playing"));
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Search movies and TV shows  (Ctrl+F)"));
    search.update_property(&[gtk::accessible::Property::Label("Search the library")]);
    search.add_css_class("bar-search");
    search.set_width_chars(40);
    // Typing starts from the page you were on, so its search (movies, playlists…) applies.
    let focus = gtk::EventControllerFocus::new();
    focus.connect_enter(|_| set_search_origin());
    search.add_controller(focus);
    search.connect_search_changed(|entry| {
        if SEARCH_CLEARING.with(|clearing| clearing.get()) { return; }
        let origin = SEARCH_ORIGIN.with(|origin| origin.borrow().clone());
        if origin == "playlists" { crate::playlists::search(&entry.text()); return; }
        crate::library::search(&entry.text(), &origin);
        if !entry.text().is_empty() {
            show_page("search");
        } else {
            show_page(&origin);
        }
    });
    search.connect_stop_search(|_| hide_search());
    let content = widgets::vbox(0);
    content.add_css_class("settings-content");
    content.set_hexpand(true);
    content.append(&stack);
    let body = widgets::hbox(0);
    body.set_vexpand(true);
    body.append(&nav);
    body.append(&content);
    let readout = widgets::label("No servers connected", "status-readout");
    readout.add_css_class("nav-foot");
    readout.add_css_class("compact-hide");
    readout.set_xalign(0.0);
    readout.set_ellipsize(gtk::pango::EllipsizeMode::End);
    nav.append(&readout);
    // The header: the app and the page, search, and while a video plays in the
    // background its controls, then opening a file, refreshing and Settings.
    let header = chrome::app_header(&window, "theater-film-symbolic", "Theater");
    // The sidebar's toggle sits in the header, after the app and the page.
    let collapse = nyx_ui::widgets::bar_button("sidebar-show-symbolic", "Collapse the sidebar (Ctrl+B)");
    collapse.connect_clicked(|_| toggle_sidebar());
    header.start.append(&collapse);
    header.center.append(&search);
    let bar = header.end.clone();
    let return_video = nyx_ui::widgets::bar_button("media-playback-start-symbolic", "Return to playback");
    return_video.connect_clicked(|_| navigate("playing"));
    return_video.set_visible(false);
    bar.append(&return_video);
    let mini = widgets::hbox(6);
    mini.add_css_class("theater-mini-player");
    let mini_title = widgets::label("", "status-readout");
    mini_title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    mini_title.set_max_width_chars(20);
    let mini_progress = gtk::ProgressBar::new();
    mini_progress.set_size_request(100, -1);
    let mini_back = nyx_ui::widgets::icon_button("media-seek-backward-symbolic", "Seek back");
    let mini_play = nyx_ui::widgets::icon_button("media-playback-pause-symbolic", "Play or pause");
    let mini_fwd = nyx_ui::widgets::icon_button("media-seek-forward-symbolic", "Seek forward");
    for (button, direction) in [(&mini_back, -1.0), (&mini_fwd, 1.0)] {
        let player = player.clone();
        button.connect_clicked(move |_| player.borrow().seek_by(direction * prefs::get().seek_seconds as f64));
    }
    let mini_player = player.clone();
    mini_play.connect_clicked(move |_| mini_player.borrow().toggle());
    mini.append(&mini_title);
    mini.append(&mini_progress);
    mini.append(&mini_back);
    mini.append(&mini_play);
    mini.append(&mini_fwd);
    mini.set_visible(false);
    bar.append(&mini);
    let open = nyx_ui::widgets::bar_button("document-open-symbolic", "Open a video file (Ctrl+O)");
    open.connect_clicked(|_| open_file());
    bar.append(&open);
    let refresh = nyx_ui::widgets::bar_button("view-refresh-symbolic", "Refresh libraries");
    refresh.connect_clicked(|_| crate::servers::refresh());
    bar.append(&refresh);
    bar.append(&chrome::settings_button(settings::open));
    let weak = return_video.downgrade();
    let weak_mini = mini.downgrade();
    let weak_player = Rc::downgrade(&player);
    glib::timeout_add_local(std::time::Duration::from_secs(1), move || {
        let (Some(button), Some(mini), Some(player)) = (weak.upgrade(), weak_mini.upgrade(), weak_player.upgrade()) else { return glib::ControlFlow::Break };
        if is_closing() {
            return glib::ControlFlow::Break;
        }
        let p = player.borrow();
        let loaded = p.has_video();
        crate::inhibit::set_playing(loaded && !p.paused);
        let browsing = page() != "playing";
        button.set_visible(loaded && browsing);
        mini.set_visible(loaded && browsing);
        if loaded {
            mini_title.set_text(&p.title);
            mini_progress.set_fraction(if p.duration() > 0.0 { (p.position / p.duration()).clamp(0.0, 1.0) } else { 0.0 });
            mini_play.set_icon_name(if p.paused { "media-playback-start-symbolic" } else { "media-playback-pause-symbolic" });
        }
        glib::ControlFlow::Continue
    });
    let frame = widgets::vbox(0);
    frame.add_css_class("window-frame");
    frame.append(&header.root);
    frame.append(&body);
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&frame));
    window.set_child(Some(&overlay));
    let probe = gtk::DrawingArea::new();
    probe.set_can_target(false);
    probe.set_can_focus(false);
    overlay.add_overlay(&probe);
    overlay.set_measure_overlay(&probe, false);
    probe.connect_resize(|_, _, _| {
        glib::idle_add_local_once(apply_width);
    });
    window.connect_default_width_notify(|_| apply_width());
    window.connect_realize(|_| apply_width());
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed(|_, key, _, mods| {
        let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
        if key == gdk::Key::space
            && !mods.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK | gdk::ModifierType::SUPER_MASK)
        {
            if typing() {
                return glib::Propagation::Proceed;
            }
            UI.with(|u| {
                if let Some(u) = u.borrow().as_ref()
                    && u.player.borrow().has_video()
                {
                    u.player.borrow().toggle();
                }
            });
            return glib::Propagation::Stop;
        }
        match key {
            gdk::Key::f | gdk::Key::F if ctrl => show_search(),
            gdk::Key::b | gdk::Key::B if ctrl => toggle_sidebar(),
            gdk::Key::o | gdk::Key::O if ctrl => open_file(),
            gdk::Key::F1 => show_shortcuts(),
            gdk::Key::Left if mods.contains(gdk::ModifierType::ALT_MASK) => back(),
            gdk::Key::Right if mods.contains(gdk::ModifierType::ALT_MASK) => forward(),
            gdk::Key::Escape if self::window().is_some_and(|w| w.is_fullscreen()) => leave_fullscreen(),
            // Full screen is for the player; with nothing loaded there's nothing to show.
            gdk::Key::f if !ctrl && !typing() && has_video() => toggle_fullscreen(),
            gdk::Key::m if !ctrl && !typing() => UI.with(|u| {
                if let Some(u) = u.borrow().as_ref() {
                    u.player.borrow().toggle_mute();
                }
            }),
            gdk::Key::Escape if settings_card::is_open() => settings_card::escape(),
            gdk::Key::Escape if UI.with(|u| u.borrow().as_ref().is_some_and(|u| !u.search.text().is_empty())) => {
                hide_search()
            }
            gdk::Key::Left | gdk::Key::Right if !ctrl => {
                let handled = UI.with(|u| {
                    if let Some(u) = u.borrow().as_ref() {
                        let focus = gtk::prelude::GtkWindowExt::focus(&u.window);
                        if focus.is_some_and(|w| {
                            w.is::<gtk::Text>() || w.is::<gtk::TextView>() || w.ancestor(gtk::Entry::static_type()).is_some()
                        }) {
                            return false;
                        }
                        if settings_card::is_open() {
                            return false;
                        }
                        if u.stack.visible_child_name().as_deref() == Some("playing") {
                            let p = u.player.borrow();
                            match key {
                                gdk::Key::Left => p.seek_by(-(prefs::get().seek_seconds as f64)),
                                _ => p.seek_by(prefs::get().seek_seconds as f64),
                            }
                            return true;
                        }
                    }
                    false
                });
                return if handled { glib::Propagation::Stop } else { glib::Propagation::Proceed };
            }
            _ => return glib::Propagation::Proceed,
        }
        glib::Propagation::Stop
    });
    window.add_controller(keys);
    crate::navigation::install(&window);
    window.connect_fullscreened_notify(|w| {
        UI.with(|u| {
            if let Some(u) = u.borrow().as_ref() {
                let full = w.is_fullscreen();
                if full {
                    w.add_css_class("fullscreen");
                } else {
                    w.remove_css_class("fullscreen");
                }
                u.nav.set_visible(!full);
                u.header.set_visible(!full);
            }
        });
    });
    let p = player.clone();
    window.connect_close_request(move |_| {
        CLOSING.with(|c| c.set(true));
        crate::inhibit::shutdown();
        crate::library::cancel();
        crate::playlists::cancel();
        crate::artwork::cancel();
        crate::servers::cancel();
        p.borrow_mut().shutdown();
        crate::playback::finish_reporting_on_close();
        prefs::flush();
        UI.with(|u| u.borrow_mut().take());
        glib::Propagation::Proceed
    });
    UI.with(|u| {
        *u.borrow_mut() = Some(Ui {
            window: window.clone(),
            overlay,
            nav,
            stack,
            search,
            status: readout,
            player,
            header: header.root.clone(),
            subtitle: header.subtitle.clone(),
            buttons,
        })
    });
    let p = prefs::get();
    let start = prefs::startup_page(&p).to_owned();
    navigate(&start);
    window.present();
    crate::servers::start();
    if prefs::take_broken() {
        status("Settings could not be read. Defaults are in use; a backup was kept.");
    }
    // The first database initialization stays off GTK, as future provider jobs will.
    nyx_core::cmd::background(
        || {
            nyx_media_cache::Cache::open(&nyx_core::paths::cache_dir().join("library.sqlite3"))
                .map(|_| ())
                .map_err(|_| "Metadata cache is unavailable")
        },
        |result| {
            if let Err(error) = result {
                status(error);
            }
        },
    );
}

/// Ctrl+F: into the search field.
fn show_search() {
    UI.with(|u| {
        if let Some(u) = u.borrow().as_ref() {
            u.search.grab_focus();
        }
    });
}
/// Search what the page you're on holds (movies, TV shows, playlists…), or everything.
fn set_search_origin() {
    if page() == "search" {
        return;
    }
    UI.with(|u| {
        if let Some(u) = u.borrow().as_ref() {
            let current = page();
            let origin = if matches!(current.as_str(), "home" | "discover" | "movies" | "tv" | "collections" | "playlists") {
                current
            } else { prefs::get().last_library_page };
            let origin = if origin.is_empty() { "home".into() } else { origin };
            u.search.set_placeholder_text(Some(match origin.as_str() {
                "movies" => "Search movies", "tv" => "Search TV shows", "collections" => "Search collections",
                "playlists" => "Search playlists or playlist videos",
                _ => "Search movies and TV shows",
            }));
            SEARCH_ORIGIN.with(|stored| *stored.borrow_mut() = origin);
        }
    });
}
fn hide_search() {
    SEARCH_CLEARING.with(|clearing| clearing.set(true));
    let origin = SEARCH_ORIGIN.with(|origin| origin.borrow().clone());
    UI.with(|u| if let Some(u) = u.borrow().as_ref() { u.search.set_text(""); });
    if origin == "playlists" { crate::playlists::search(""); }
    else { crate::library::search("", &origin); }
    SEARCH_CLEARING.with(|clearing| clearing.set(false));
    if page() == "search" { show_page(&origin); }
}
fn show_page(id: &str) {
    UI.with(|u| {
        if let Some(u) = u.borrow().as_ref() {
            let id = if u.stack.child_by_name(id).is_some() { id } else { "home" };
            u.stack.set_visible_child_name(id);
            let mut title = match id {
                "details" => "Details",
                "playing" => "Playback",
                _ => "Search",
            };
            for (page, name, button) in &u.buttons {
                if *page == id {
                    button.add_css_class("active");
                    title = name;
                } else {
                    button.remove_css_class("active");
                }
            }
            chrome::set_title(&u.window, "Theater", title);
            u.subtitle.set_text(title);
            u.subtitle.set_visible(true);
        }
    });
    crate::library::show(id);
}
pub fn page() -> String {
    UI.with(|u| {
        u.borrow().as_ref().and_then(|u| u.stack.visible_child_name()).map(|s| s.to_string()).unwrap_or_else(|| "home".into())
    })
}
pub fn navigate(id: &str) {
    if id != "search" && (page() == "search" || UI.with(|u| u.borrow().as_ref().is_some_and(|u| !u.search.text().is_empty()))) { hide_search(); }
    NAV_FORWARD.with(|h| h.borrow_mut().clear());
    let previous = page();
    if previous != id {
        NAV_HISTORY.with(|h| {
            let mut h = h.borrow_mut();
            if h.last() != Some(&previous) {
                h.push(previous);
            }
            if h.len() > 64 {
                h.remove(0);
            }
        });
    }
    navigate_inner(id);
}
fn navigate_inner(id: &str) {
    if id != "playing" {
        crate::library::refresh_watch();
    }
    show_page(id);
    if id == "playlists" {
        crate::playlists::show();
    }
    prefs::update(|p| {
        p.last_section = id.into();
        if matches!(id, "home" | "movies" | "tv" | "collections" | "discover" | "playlists") {
            p.last_library_page = id.into();
        }
    });
}
pub fn back() {
    if settings_card::is_open() {
        settings_card::escape();
        return;
    }
    if page() == "search" {
        hide_search();
        return;
    }
    if page() == "details" {
        let item = crate::library::current();
        crate::library::back();
        NAV_FORWARD.with(|h| h.borrow_mut().push(("details".into(), item)));
        return;
    }
    back_from_details();
}
pub fn back_from_details() {
    let current = page();
    let previous = NAV_HISTORY
        .with(|h| {
            let mut h = h.borrow_mut();
            while let Some(page) = h.pop() {
                if page != current {
                    return Some(page);
                }
            }
            None
        })
        .unwrap_or_else(|| "home".into());
    if current != "details" { NAV_FORWARD.with(|h| h.borrow_mut().push((current, None))); }
    navigate_inner(&previous);
}
pub fn forward() {
    if let Some((next, item)) = NAV_FORWARD.with(|h| h.borrow_mut().pop()) {
        let remaining = NAV_FORWARD.with(|h| h.borrow().clone());
        let current = page();
        NAV_HISTORY.with(|h| h.borrow_mut().push(current));
        if let Some(item) = item { crate::library::show_detail(item); }
        else { navigate_inner(&next); }
        NAV_FORWARD.with(|h| *h.borrow_mut() = remaining);
    }
}
fn toggle_sidebar() {
    prefs::update(|p| p.sidebar_collapsed = !p.sidebar_collapsed);
    apply_width();
}
fn compact_children(widget: &gtk::Widget, compact: bool) {
    let mut child = widget.first_child();
    while let Some(c) = child {
        if c.has_css_class("compact-hide") {
            c.set_visible(!compact);
        }
        if c.parent().is_some_and(|p| p.has_css_class("nav-item")) {
            c.set_halign(if compact { gtk::Align::Center } else { gtk::Align::Fill });
        }
        compact_children(&c, compact);
        child = c.next_sibling();
    }
}
fn apply_width() {
    UI.with(|u| {
        if let Some(u) = u.borrow().as_ref() {
            let width = if u.window.width() > 0 { u.window.width() } else { u.window.default_width() };
            let compact = width < 980 || prefs::get().sidebar_collapsed;
            if compact {
                u.nav.add_css_class("compact");
            } else {
                u.nav.remove_css_class("compact");
            }
            compact_children(u.nav.upcast_ref(), compact);
            settings_card::fit(&u.window);
            // Existing page padding narrows with the allocated window size too.
            for page in u.stack.pages().iter::<gtk::StackPage>().flatten() {
                if let Some(body) = page.child().downcast_ref::<gtk::ScrolledWindow>().and_then(|s| s.child()) {
                    if width < 980 {
                        body.add_css_class("narrow");
                    } else {
                        body.remove_css_class("narrow");
                    }
                }
            }
        }
    });
}
pub fn update_motion() {
    UI.with(|u| {
        if let Some(u) = u.borrow().as_ref() {
            u.stack.set_transition_duration(if prefs::get().reduce_motion { 0 } else { 160 });
        }
    });
}
pub fn open_file() {
    crate::playlists::clear_queue();
    crate::library::clear_fallback();
    let Some(window) = window() else { return };
    let dialog = gtk::FileDialog::builder().title("Open video").build();
    dialog.open(Some(&window), None::<&gtk::gio::Cancellable>, |result| {
        if let Ok(file) = result {
            let result = UI.with(|u| u.borrow().as_ref().map(|u| u.player.borrow_mut().open(file.uri().as_str())));
            match result {
                Some(Ok(())) => {
                    status("Local playback");
                    navigate("playing");
                }
                Some(Err(error)) => status(&error),
                None => {}
            }
        }
    });
}
pub const SHORTCUTS: &[(&[&str], &str)] = &[
    (&["Ctrl", "O"], "Open video"),
    (&["Space"], "Play or pause"),
    (&["F"], "Fullscreen"),
    (&["M"], "Mute or unmute"),
    (&["←", "→"], "Seek by chosen jump interval"),
    (&["Ctrl", "F"], "Search library"),
    (&["Alt", "←"], "Back · or swipe right with two fingers"),
    (&["Alt", "→"], "Forward · or swipe left with two fingers"),
    (&["Esc"], "Clear search or close settings"),
    (&["Ctrl", "B"], "Collapse sidebar"),
    (&["Ctrl", ","], "Settings"),
    (&["Ctrl", "Shift", "P"], "Command palette"),
    (&["F1"], "Keyboard shortcuts"),
    (&["Ctrl", "Q"], "Close window"),
];
fn show_shortcuts() {
    let (dialog, card) = widgets::dialog("Keyboard shortcuts", 520);
    let list = widgets::vbox(0);
    list.add_css_class("group-list");
    for (keys, action) in SHORTCUTS {
        list.append(&widgets::row(action, "", Some(widgets::key_caps(keys).upcast_ref())));
    }
    card.append(
        &gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::External)
            .propagate_natural_height(true)
            .max_content_height(560)
            .child(&list)
            .build(),
    );
    let close = gtk::Button::with_label("Close");
    close.set_halign(gtk::Align::End);
    let d = dialog.clone();
    close.connect_clicked(move |_| d.close());
    card.append(&close);
    dialog.present();
}

fn typing() -> bool {
    window()
        .and_then(|w| gtk::prelude::GtkWindowExt::focus(&w))
        .is_some_and(|w| w.is::<gtk::Text>() || w.is::<gtk::TextView>() || w.ancestor(gtk::Entry::static_type()).is_some())
}
/// What's playing: title, position and length in seconds, paused.
pub fn now_playing() -> Option<(String, f64, f64, bool)> {
    UI.with(|u| {
        let u = u.borrow();
        let p = u.as_ref()?.player.borrow();
        p.has_video().then(|| (p.title.clone(), p.position, p.duration(), p.paused))
    })
}

/// Work the player from outside the window (Nyx Assistant).
pub fn control(action: &str, seconds: f64) -> Result<(), String> {
    let player = UI.with(|u| u.borrow().as_ref().map(|u| u.player.clone())).ok_or("Theater isn't open")?;
    if !player.borrow().has_video() {
        return Err("nothing is playing".into());
    }
    match action {
        "pause" => player.borrow().set_string("pause", "yes"),
        "resume" => player.borrow().set_string("pause", "no"),
        "toggle" => player.borrow().toggle(),
        "forward" => player.borrow().seek_by(seconds),
        "back" => player.borrow().seek_by(-seconds),
        "stop" => {
            player.borrow_mut().close();
            back();
        }
        _ => return Err(format!("unknown action {action}")),
    }
    Ok(())
}

/// Play a local file or URI (from the command line or the file manager).
pub fn open_uri(uri: &str) {
    crate::playlists::clear_queue();
    crate::library::clear_fallback();
    let result = UI.with(|u| u.borrow().as_ref().map(|u| u.player.borrow_mut().open(uri)));
    match result {
        Some(Ok(())) => {
            status("Local playback");
            navigate("playing");
        }
        Some(Err(error)) => status(&error),
        None => {}
    }
}

fn has_video() -> bool {
    UI.with(|u| u.borrow().as_ref().is_some_and(|u| u.player.borrow().has_video()))
}

pub fn toggle_fullscreen() {
    let Some(w) = window() else { return };
    if w.is_fullscreen() {
        leave_fullscreen();
    } else {
        navigate("playing");
        w.fullscreen();
    }
}
pub fn leave_fullscreen() {
    if let Some(w) = window() {
        w.unfullscreen();
    }
}

pub fn play_remote(
    source: nyx_media_model::PlaybackSource,
    item: nyx_media_model::MediaItem,
    client: crate::servers::Client,
    resume_ms: u64,
    version: String,
    auto_transcode: bool,
    request_id: u64,
) {
    let player = UI.with(|u| u.borrow().as_ref().map(|u| u.player.clone()));
    let Some(player) = player else {
        client.discard_session(source.session_id, source.mode);
        return;
    };
    if !player.borrow().accepts_request(request_id) {
        client.discard_session(source.session_id, source.mode);
        return;
    }
    let mode = source.mode;
    let label = client.label();
    let result = player.borrow_mut().open_remote(source, item, client, resume_ms, version, auto_transcode);
    match result {
        Ok(()) => {
            status(&format!(
                "{label} · {}",
                if mode == nyx_media_model::PlaybackMode::Transcode { "Transcoding" } else { "Direct Play" }
            ));
            navigate("playing");
        }
        Err(e) => status(&e),
    }
}

pub fn begin_playback_request(server: nyx_media_model::ServerId) -> Option<u64> {
    if crate::reconcile::busy() {
        status("Wait for the watch-state update to finish before starting playback.");
        return None;
    }
    let player = UI.with(|u| u.borrow().as_ref().map(|u| u.player.clone()))?;
    let request = player.borrow_mut().begin_request(server);
    Some(request)
}

pub fn stop_server(server: &nyx_media_model::ServerId) {
    crate::library::clear_fallback();
    crate::playlists::clear_queue();
    crate::reconcile::cancel();
    let player = UI.with(|u| u.borrow().as_ref().map(|u| u.player.clone()));
    if let Some(player) = player {
        player.borrow_mut().cancel_server(server)
    }
}

pub fn accepts_playback_request(request: u64) -> bool {
    UI.with(|u| u.borrow().as_ref().is_some_and(|u| u.player.borrow().accepts_request(request)))
}

pub fn server_playing(server: &nyx_media_model::ServerId) -> bool {
    UI.with(|u| u.borrow().as_ref().is_some_and(|u| u.player.borrow().uses_server(server)))
}

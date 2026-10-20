use crate::{playback::Playback, prefs, settings, widgets};
use gtk::{gdk, glib, prelude::*};
use nixe_ui::{chrome, settings_card};
use std::{cell::RefCell, rc::Rc};

struct Ui {
    window: gtk::ApplicationWindow,
    overlay: gtk::Overlay,
    nav: gtk::Box,
    head: gtk::Box,
    stack: gtk::Stack,
    search: gtk::SearchEntry,
    status: gtk::Label,
    player: Rc<RefCell<Playback>>,
    status_bar: gtk::Box,
    buttons: Vec<(&'static str, &'static str, gtk::Button)>,
}
thread_local! {
    /// The "Up next" card while it's counting down, and its timer.
    static UP_NEXT: RefCell<Option<(gtk::Box, glib::SourceId)>> = const { RefCell::new(None) };
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
    static NAV_HISTORY: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
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
pub fn show_up_next(next: nixe_media_model::MediaItem) {
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
    let play = nixe_ui::widgets::icon_button("media-skip-forward-symbolic", "Play it now");
    let stop = nixe_ui::widgets::icon_button("window-close-symbolic", "Don't play it");
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
    window.set_icon_name(Some("io.github.design_nexus.nixe.Theater"));
    window.set_titlebar(Some(&gtk::Box::new(gtk::Orientation::Horizontal, 0)));
    nixe_ui::keys::install(
        &window,
        nixe_ui::keys::Common {
            settings: Some(settings::open),
            shortcuts: Some(show_shortcuts),
            close: true,
            ..Default::default()
        },
    );
    let nav = widgets::vbox(0);
    nav.add_css_class("settings-navigation");
    nav.set_hexpand(false);
    let head = chrome::sidebar_head("Theater", Some("Search library (Ctrl+F)"));
    head.toggle.connect_clicked(|_| toggle_sidebar());
    if let Some(search) = &head.search {
        search.connect_clicked(|_| show_search());
    }
    nav.append(&head.root);
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
    search.set_placeholder_text(Some("Search library"));
    search.set_visible(false);
    search.connect_search_changed(|entry| {
        crate::library::search(&entry.text());
        if !entry.text().is_empty() {
            show_page("search");
        } else {
            show_page(&prefs::get().last_section);
        }
    });
    search.connect_stop_search(|s| {
        s.set_text("");
        s.set_visible(false);
    });
    let content = widgets::vbox(0);
    content.add_css_class("settings-content");
    content.set_hexpand(true);
    content.append(&chrome::search_strip(&search));
    content.append(&stack);
    let body = widgets::hbox(0);
    body.set_vexpand(true);
    body.append(&nav);
    body.append(&content);
    let bar = widgets::hbox(16);
    bar.add_css_class("status-bar");
    let help = chrome::shortcuts_button(show_shortcuts);
    bar.append(&help);
    let readout = widgets::label("No servers connected", "status-readout");
    readout.set_hexpand(true);
    readout.set_xalign(1.0);
    readout.set_ellipsize(gtk::pango::EllipsizeMode::End);
    bar.append(&readout);
    let return_video = chrome::status_action("media-playback-start-symbolic", "Return to playback", || navigate("playing"));
    return_video.set_visible(false);
    bar.append(&return_video);
    let weak = return_video.downgrade();
    let weak_player = Rc::downgrade(&player);
    glib::timeout_add_local(std::time::Duration::from_secs(1), move || {
        let (Some(button), Some(player)) = (weak.upgrade(), weak_player.upgrade()) else { return glib::ControlFlow::Break };
        if is_closing() {
            return glib::ControlFlow::Break;
        }
        button.set_visible(player.borrow().has_video());
        glib::ControlFlow::Continue
    });
    bar.append(&chrome::gear(settings::open));
    let frame = widgets::vbox(0);
    frame.add_css_class("window-frame");
    frame.append(&body);
    frame.append(&bar);
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
    keys.connect_key_pressed(|_, key, _, mods| {
        let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
        match key {
            gdk::Key::f | gdk::Key::F if ctrl => show_search(),
            gdk::Key::b | gdk::Key::B if ctrl => toggle_sidebar(),
            gdk::Key::o | gdk::Key::O if ctrl => open_file(),
            gdk::Key::F1 => show_shortcuts(),
            gdk::Key::Left if mods.contains(gdk::ModifierType::ALT_MASK) => back(),
            gdk::Key::Escape if self::window().is_some_and(|w| w.is_fullscreen()) => leave_fullscreen(),
            // Full screen is for the player; with nothing loaded there's nothing to show.
            gdk::Key::f if !ctrl && !typing() && has_video() => toggle_fullscreen(),
            gdk::Key::m if !ctrl && !typing() => UI.with(|u| {
                if let Some(u) = u.borrow().as_ref() {
                    u.player.borrow().toggle_mute();
                }
            }),
            gdk::Key::Escape if settings_card::is_open() => settings_card::escape(),
            gdk::Key::Escape => UI.with(|u| {
                let s = u.borrow().as_ref().map(|u| u.search.clone());
                if let Some(s) = s {
                    s.set_text("");
                    s.set_visible(false);
                }
            }),
            gdk::Key::space | gdk::Key::Left | gdk::Key::Right if !ctrl => {
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
                                gdk::Key::space => p.toggle(),
                                gdk::Key::Left => p.seek_by(-5.0),
                                _ => p.seek_by(5.0),
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
                u.status_bar.set_visible(!full);
                if full {
                    u.search.set_visible(false);
                }
            }
        });
    });
    let p = player.clone();
    window.connect_close_request(move |_| {
        CLOSING.with(|c| c.set(true));
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
            head: head.root,
            stack,
            search,
            status: readout,
            player,
            status_bar: bar,
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
    nixe_core::cmd::background(
        || {
            nixe_media_cache::Cache::open(&nixe_core::paths::cache_dir().join("library.sqlite3"))
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

fn show_search() {
    UI.with(|u| {
        if let Some(u) = u.borrow().as_ref() {
            u.search.set_visible(true);
            u.search.grab_focus();
        }
    });
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
        let entry = UI.with(|u| u.borrow().as_ref().map(|u| u.search.clone()));
        if let Some(entry) = entry {
            entry.set_text("");
            entry.set_visible(false);
        }
        let previous = prefs::get().last_section;
        navigate_inner(&previous);
        return;
    }
    if page() == "details" {
        crate::library::back();
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
    UI.with(|u| {
        if let Some(u) = u.borrow().as_ref() {
            u.search.set_visible(false);
        }
    });
    navigate_inner(&previous);
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
            chrome::set_head_compact(&u.head, compact);
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
    (&["←", "→"], "Seek 5 seconds"),
    (&["Ctrl", "F"], "Search library"),
    (&["Alt", "←"], "Back · or swipe right with two fingers"),
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

/// Work the player from outside the window (Nixe Assistant).
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
    source: nixe_media_model::PlaybackSource,
    item: nixe_media_model::MediaItem,
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
                if mode == nixe_media_model::PlaybackMode::Transcode { "Transcoding" } else { "Direct Play" }
            ));
            navigate("playing");
        }
        Err(e) => status(&e),
    }
}

pub fn begin_playback_request(server: nixe_media_model::ServerId) -> Option<u64> {
    if crate::reconcile::busy() {
        status("Wait for the watch-state update to finish before starting playback.");
        return None;
    }
    let player = UI.with(|u| u.borrow().as_ref().map(|u| u.player.clone()))?;
    let request = player.borrow_mut().begin_request(server);
    Some(request)
}

pub fn stop_server(server: &nixe_media_model::ServerId) {
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

pub fn server_playing(server: &nixe_media_model::ServerId) -> bool {
    UI.with(|u| u.borrow().as_ref().is_some_and(|u| u.player.borrow().uses_server(server)))
}

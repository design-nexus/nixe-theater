use crate::{
    prefs,
    widgets::{self, Page},
    window,
};
use gtk::{glib, prelude::*};
use nyx_ui::settings_card::{self, About, Spec, Standard};

pub fn open() {
    let (Some(overlay), Some(window)) = (window::overlay(), window::window()) else { return };
    let page = widgets::page("settings");
    build(&page);
    settings_card::open_page(
        &overlay,
        &window,
        Spec {
            groups: &[
                ("Servers", "network-server-symbolic", "Connections and watch state"),
                ("Metadata", "dialog-information-symbolic", "Ratings, credits and recommendations"),
                ("Viewing", "video-display-symbolic", "Starting view, seeking, posters"),
            ],
            search_text: None,
            standard: Some(Standard {
                app: "theater",
                about: About {
                    name: "Theater",
                    icon: "io.github.design_nexus.nyx.Theater",
                    version: env!("CARGO_PKG_VERSION"),
                    description: "Your Plex and Jellyfin libraries in one place.",
                    licence: env!("CARGO_PKG_LICENSE"),
                    website: "https://github.com/design-nexus/nyx-theater",
                    issues: "https://github.com/design-nexus/nyx-theater/issues",
                    credits: &[("mpv", "playback")],
                },
                shortcuts: window::SHORTCUTS,
                reduce_motion: || prefs::get().reduce_motion,
                set_reduce_motion: |on| {
                    prefs::update(|p| p.reduce_motion = on);
                    window::update_motion();
                },
            }),
        },
        page.root.upcast_ref(),
        &page.body,
    );
}

/// An option row whose action is an icon button.
fn action_row(title: &str, desc: &str, icon: &str, tip: &str, act: impl Fn() + 'static) -> (gtk::Box, gtk::Button) {
    let b = nyx_ui::widgets::icon_button(icon, tip);
    b.connect_clicked(move |_| act());
    (widgets::row(title, desc, Some(b.upcast_ref())), b)
}

/// A row for something that can't come back: the first click arms the button (its
/// tooltip says to click again), the second acts; it disarms after 4 s.
fn armed_row(title: &str, desc: &str, icon: &str, tip: &'static str, act: impl Fn() + 'static) -> gtk::Box {
    let b = nyx_ui::widgets::icon_button(icon, tip);
    let armed = std::rc::Rc::new(std::cell::Cell::new(false));
    b.connect_clicked(move |b| {
        if armed.replace(!armed.get()) {
            b.remove_css_class("destructive-action");
            b.set_tooltip_text(Some(tip));
            act();
            return;
        }
        b.add_css_class("destructive-action");
        b.set_tooltip_text(Some("Click again to confirm"));
        let (weak, armed) = (b.downgrade(), armed.clone());
        glib::timeout_add_seconds_local_once(4, move || {
            if let Some(b) = weak.upgrade()
                && armed.replace(false)
            {
                b.remove_css_class("destructive-action");
                b.set_tooltip_text(Some(tip));
            }
        });
    });
    widgets::row(title, desc, Some(b.upcast_ref()))
}

fn build(page: &Page) {
    let p = prefs::get();
    let servers = page.group("Servers");
    let description = p
        .plex
        .as_ref()
        .map(|p| format!("{} · {}", p.name, p.address))
        .unwrap_or_else(|| "Connect your movie and TV libraries.".into());
    let (row, _) = action_row(
        "Plex",
        &glib::markup_escape_text(&description),
        "network-server-symbolic",
        if p.plex.is_some() { "Reconnect to Plex" } else { "Connect to Plex" },
        crate::plex::connect_dialog,
    );
    servers.add(&row);
    if p.plex.is_some() {
        let (row, _) = action_row(
            "Refresh Plex libraries",
            "Update titles, artwork and watch progress from your server.",
            "view-refresh-symbolic",
            "Refresh Plex libraries",
            crate::plex::refresh,
        );
        servers.add(&row);
        servers.add(&armed_row(
            "Disconnect Plex",
            "Remove the saved token from the desktop keyring. Signing in again brings it back.",
            "network-offline-symbolic",
            "Disconnect Plex",
            crate::plex::disconnect,
        ));
    }
    let description = p
        .jellyfin
        .as_ref()
        .map(|p| format!("{} · {}", p.name, p.address))
        .unwrap_or_else(|| "Connect your Jellyfin movie and TV libraries.".into());
    let (row, _) = action_row(
        "Jellyfin",
        &glib::markup_escape_text(&description),
        "network-server-symbolic",
        if p.jellyfin.is_some() { "Reconnect to Jellyfin" } else { "Connect to Jellyfin" },
        crate::jellyfin::connect_dialog,
    );
    servers.add(&row);
    if p.jellyfin.is_some() {
        let (row, _) = action_row(
            "Refresh Jellyfin libraries",
            "Update titles, artwork and watch progress from your server.",
            "view-refresh-symbolic",
            "Refresh Jellyfin libraries",
            crate::jellyfin::refresh,
        );
        servers.add(&row);
        servers.add(&armed_row(
            "Disconnect Jellyfin",
            "Remove the saved token from the desktop keyring. Signing in again brings it back.",
            "network-offline-symbolic",
            "Disconnect Jellyfin",
            crate::jellyfin::disconnect,
        ));
    }
    let options = widgets::opts(&[("disabled", "Disabled"), ("manual", "Review changes")]);
    let (row, choice) = widgets::choice_row(
        "Watch-state reconciliation",
        "Keep states separate, or review conflicts and choose a source before copying.",
        options,
        if p.reconciliation == nyx_media_model::Reconciliation::Manual { "manual" } else { "disabled" },
        |value| {
            prefs::update(|p| {
                p.reconciliation = if value == "manual" {
                    nyx_media_model::Reconciliation::Manual
                } else {
                    nyx_media_model::Reconciliation::Disabled
                }
            })
        },
    );
    choice.connect_selected_notify(|_| {
        if prefs::get().reconciliation == nyx_media_model::Reconciliation::Disabled {
            crate::reconcile::cancel()
        }
    });
    servers.add(&row);
    let (row, button) = action_row(
        "Review watch state",
        "Inspect conflicting titles and explicitly choose which server's state to copy.",
        "view-dual-symbolic",
        "Review watch state",
        crate::reconcile::open,
    );
    button.set_sensitive(p.reconciliation == nyx_media_model::Reconciliation::Manual);
    choice.connect_selected_notify(move |_| {
        button.set_sensitive(prefs::get().reconciliation == nyx_media_model::Reconciliation::Manual)
    });
    servers.add(&row);
    metadata_settings(page);
    // ----- Viewing -----
    let g = page.group("Viewing");
    let (row, _) = widgets::choice_row(
        "Starting view",
        "Always start at Home, or restore your last library view and its filters.",
        widgets::opts(&[("home", "Home"), ("last", "Last view")]),
        if p.restore_view { "last" } else { "home" },
        |value| prefs::update(|p| p.restore_view = value == "last"),
    );
    g.add(&row);
    let seek = p.seek_seconds.to_string();
    let (row, _) = widgets::choice_row(
        "Seek jump",
        "Distance for back and forward buttons and the arrow keys.",
        widgets::opts(&[("5", "5 seconds"), ("10", "10 seconds"), ("30", "30 seconds"), ("60", "60 seconds")]),
        &seek,
        |value| if let Ok(seconds) = value.parse::<u32>() { prefs::update(|p| p.seek_seconds = seconds); },
    );
    g.add(&row);
    let (row, _) = widgets::switch_row(
        "Play the next episode",
        "When an episode ends, play the next one after a 10-second countdown you can skip or stop.",
        p.autoplay_next,
        |on| prefs::update(|p| p.autoplay_next = on),
    );
    g.add(&row);
    let (row, _) = widgets::choice_row(
        "Poster size",
        "Resize movie and show cards throughout Theater.",
        widgets::opts(&[("small", "Small"), ("medium", "Medium"), ("large", "Large")]),
        &p.poster_size,
        |size| {
            prefs::update(|p| p.poster_size = size);
            crate::presentation::resize();
        },
    );
    g.add(&row);
}

fn metadata_settings(page: &Page) {
    let p = prefs::get();
    let group = page.group("Metadata");
    for (provider, title, description, enabled) in [
        ("tmdb", "TMDB", "People, filmography, credits, trailers and recommendations.", p.tmdb_enabled),
        ("mdblist", "MDBList", "IMDb, Rotten Tomatoes and Metacritic ratings when supplied.", p.mdblist_enabled),
    ] {
        let (row, _) = action_row(
            title,
            description,
            "dialog-password-symbolic",
            if enabled { "Change the API token" } else { "Add an API token" },
            move || metadata_credentials(provider),
        );
        group.add(&row);
        let (row, _) = widgets::switch_row(
            &format!("Use {title}"),
            "Refresh cached metadata in the background when a token is saved.",
            enabled,
            move |enabled| {
                prefs::update(|p| if provider == "tmdb" { p.tmdb_enabled = enabled } else { p.mdblist_enabled = enabled });
                crate::metadata::configure(prefs::metadata_config());
                crate::library::refresh();
            },
        );
        group.add(&row);
    }
    let options = widgets::opts(&[
        ("en-US", "English (US)"),
        ("en-GB", "English (UK)"),
        ("de-DE", "German"),
        ("es-ES", "Spanish"),
        ("fr-FR", "French"),
        ("it-IT", "Italian"),
        ("ja-JP", "Japanese"),
        ("pt-BR", "Portuguese (Brazil)"),
    ]);
    let (row, _) = widgets::choice_row(
        "Metadata language",
        "Language for biographies and optional metadata.",
        options,
        &p.metadata_language,
        |value| {
            prefs::update(|p| p.metadata_language = value);
            crate::metadata::configure(prefs::metadata_config());
        },
    );
    group.add(&row);
    let (row, _) = widgets::choice_row(
        "Certification region",
        "Use the certification supplied for this country.",
        widgets::opts(&[
            ("US", "United States"),
            ("GB", "United Kingdom"),
            ("CA", "Canada"),
            ("AU", "Australia"),
            ("DE", "Germany"),
            ("FR", "France"),
            ("ES", "Spain"),
            ("IT", "Italy"),
            ("JP", "Japan"),
            ("BR", "Brazil"),
        ]),
        &p.metadata_region,
        |value| {
            prefs::update(|p| p.metadata_region = value);
            crate::metadata::configure(prefs::metadata_config());
        },
    );
    group.add(&row);
    let credits = widgets::vbox(8);
    credits.set_margin_start(20);
    credits.set_margin_end(20);
    credits.set_margin_top(12);
    credits.set_margin_bottom(12);
    if let Ok(texture) = gtk::gdk::Texture::from_bytes(&glib::Bytes::from_static(include_bytes!("../data/tmdb.svg"))) {
        let logo = gtk::Picture::for_paintable(&texture);
        logo.set_size_request(100, 28);
        logo.set_content_fit(gtk::ContentFit::Contain);
        logo.set_halign(gtk::Align::Start);
        credits.append(&logo);
    }
    let label = widgets::label("This product uses the TMDB API but is not endorsed or certified by TMDB.", "dim");
    label.set_wrap(true);
    credits.append(&label);
    credits.append(&gtk::LinkButton::with_label("https://www.themoviedb.org/settings/api", "TMDB API settings"));
    credits.append(&gtk::LinkButton::with_label("https://mdblist.com/preferences/", "MDBList API settings"));
    group.add(&credits);
}
fn metadata_credentials(provider: &'static str) {
    let title = if provider == "tmdb" { "TMDB" } else { "MDBList" };
    let (dialog, body) = widgets::dialog(&format!("{title} metadata"), 480);
    let label = widgets::label(
        if provider == "tmdb" {
            "Enter your API Read Access Token (Bearer token) from TMDB settings."
        } else {
            "Enter your API key from MDBList preferences."
        },
        "dim",
    );
    label.set_wrap(true);
    body.append(&label);
    let entry =
        gtk::PasswordEntry::builder().show_peek_icon(true).placeholder_text("Stored securely in your desktop keyring").build();
    body.append(&entry);
    let message = widgets::label("", "dim");
    message.set_wrap(true);
    body.append(&message);
    let actions = widgets::hbox(8);
    let remove = gtk::Button::with_label("Remove token");
    let weak = dialog.downgrade();
    remove.connect_clicked(move |_| {
        let weak = weak.clone();
        nyx_core::cmd::background(
            move || crate::credentials::delete_for(provider, "metadata"),
            move |result| match result {
                Ok(()) => {
                    prefs::update(|p| if provider == "tmdb" { p.tmdb_enabled = false } else { p.mdblist_enabled = false });
                    crate::metadata::configure(prefs::metadata_config());
                    if let Some(d) = weak.upgrade() {
                        d.close();
                    }
                }
                Err(e) => window::status(&e.to_string()),
            },
        );
    });
    actions.append(&remove);
    let save = gtk::Button::with_label("Save token");
    save.add_css_class("suggested-action");
    let weak = dialog.downgrade();
    save.connect_clicked(move |button| {
        let secret = match nyx_media_model::Secret::new(entry.text().trim().into()) {
            Ok(s) => s,
            Err(e) => {
                message.set_text(&e.to_string());
                return;
            }
        };
        entry.set_text("");
        button.set_sensitive(false);
        let weak = weak.clone();
        let button = button.downgrade();
        let message = message.downgrade();
        nyx_core::cmd::background(
            move || crate::credentials::save_for(provider, "metadata", &secret),
            move |result| match result {
                Ok(()) => {
                    prefs::update(|p| if provider == "tmdb" { p.tmdb_enabled = true } else { p.mdblist_enabled = true });
                    crate::metadata::configure(prefs::metadata_config());
                    crate::library::refresh();
                    window::status("Metadata token saved · refreshing in the background");
                    if let Some(d) = weak.upgrade() {
                        d.close();
                    }
                }
                Err(e) => {
                    if let Some(b) = button.upgrade() {
                        b.set_sensitive(true);
                    }
                    if let Some(m) = message.upgrade() {
                        m.set_text(&e.to_string());
                    }
                }
            },
        );
    });
    actions.append(&save);
    body.append(&actions);
    dialog.present();
}

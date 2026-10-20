mod artwork;
mod browse;
mod browse_ui;
mod catalog;
mod control;
mod credentials;
mod jellyfin;
mod library;
mod metadata;
mod now_playing;
mod navigation;
mod playback;
mod playlist_store;
mod playlists;
mod plex;
mod prefs;
mod presentation;
mod reconcile;
mod rich_details;
mod rating_icons;
mod rating_index;
mod servers;
mod settings;
mod theme;
mod view_cache;
mod widgets;
mod window;

use gtk::{gio, glib, prelude::*};

fn main() -> glib::ExitCode {
    nixe_core::app::init(nixe_core::app::App { slug: "nixe-theater", name: "Theater" });
    if prefs::get().client_id.is_empty() {
        prefs::update(|p| p.client_id = uuid::Uuid::new_v4().to_string());
        prefs::flush();
    }
    // Keep the same GL renderer choice as Media Player on hybrid GPU laptops.
    if std::env::var_os("GSK_RENDERER").is_none() {
        // SAFETY: no threads or GTK have started yet.
        unsafe { std::env::set_var("GSK_RENDERER", "ngl") };
    }
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("Usage: nixe-theater [--section home|discover|movies|tv|collections|playlists|settings] [--toggle] [FILE|URI …]");
        return glib::ExitCode::SUCCESS;
    }
    let app = gtk::Application::builder()
        .application_id("io.github.design_nexus.nixe.Theater")
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();
    // Nixe Assistant finds and plays titles through this (primary instance only).
    app.connect_startup(|app| nixe_core::control::serve(app, control::tools(), control::handle));
    app.connect_command_line(|app, cl| {
        let argv: Vec<String> = cl.arguments().iter().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
        if argv.iter().any(|a| a == "--toggle")
            && let Some(w) = app.active_window()
        {
            w.close();
            return glib::ExitCode::SUCCESS;
        }
        let fresh = app.active_window().is_none();
        if fresh {
            theme::install();
            window::build(app);
        } else if let Some(w) = app.active_window() {
            w.present();
        }
        let section = argv.iter().position(|a| a == "--section").and_then(|i| argv.get(i + 1)).cloned();
        match section.as_deref() {
            Some("settings") => settings::open(),
            Some(page) => window::navigate(page),
            None => {}
        }
        // The first file or URI plays (one video at a time); flags and --section's value aren't files.
        let mut files = Vec::new();
        let mut rest = argv.iter();
        while let Some(a) = rest.next() {
            if a == "--section" {
                rest.next();
            } else if !a.starts_with("--") {
                files.push(a);
            }
        }
        if let Some(first) = files.first() {
            let file = cl.create_file_for_arg(first);
            window::open_uri(file.uri().as_str());
        }
        glib::ExitCode::SUCCESS
    });
    app.connect_shutdown(|_| prefs::flush());
    app.run()
}

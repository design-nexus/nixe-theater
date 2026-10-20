//! Media Player's shared controls, wired to Theater's player session.
use crate::{playback::Playback, prefs, widgets, window};
use gtk::{glib, prelude::*};
use nixe_playback::controls::{self, OsdControls, SeekWidgets, choice, fill_menu};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

pub fn build(player: &Rc<RefCell<Playback>>) -> gtk::Overlay {
    controls::install_icons();
    let overlay = gtk::Overlay::new();
    overlay.add_css_class("player-stage");
    overlay.set_vexpand(true);
    overlay.set_hexpand(true);
    overlay.set_overflow(gtk::Overflow::Hidden);
    let picture = gtk::Picture::for_paintable(&nixe_playback::video::paintable());
    picture.set_content_fit(gtk::ContentFit::Contain);
    picture.set_can_shrink(true);
    picture.set_hexpand(true);
    picture.set_vexpand(true);
    picture.add_css_class("video-surface");
    overlay.set_child(Some(&picture));
    let top = widgets::vbox(2);
    top.add_css_class("osd-top");
    top.set_valign(gtk::Align::Start);
    let title = widgets::label("", "osd-title");
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let subtitle = widgets::label("", "osd-subtitle");
    subtitle.set_ellipsize(gtk::pango::EllipsizeMode::End);
    top.append(&title);
    top.append(&subtitle);
    overlay.add_overlay(&top);

    let seek = SeekWidgets::new();
    let options = prefs::get();
    let controls = OsdControls::new(&seek.root, options.volume, 1.0, options.muted);
    controls.audio.set_tooltip_text(Some("Audio track"));
    controls.shape.set_tooltip_text(Some("Picture: shape and fill"));
    controls.prev.connect_clicked(|_| crate::playlists::advance(-1));
    controls.next.connect_clicked(|_| crate::playlists::advance(1));
    controls.prev.set_sensitive(false);
    controls.next.set_sensitive(false);
    overlay.add_overlay(&controls.root);
    for (button, action) in [(&controls.play, 0), (&controls.back, 1), (&controls.fwd, 2), (&controls.mute, 3)] {
        let p = player.clone();
        button.connect_clicked(move |_| {
            let p = p.borrow();
            match action {
                0 => p.toggle(),
                1 => p.seek_by(-5.0),
                2 => p.seek_by(5.0),
                _ => p.toggle_mute(),
            }
        });
    }
    controls.full.connect_clicked(|_| window::toggle_fullscreen());
    let p = player.clone();
    controls.close.connect_clicked(move |_| {
        p.borrow_mut().close();
        window::leave_fullscreen();
        window::navigate("home");
    });
    let p = player.clone();
    controls.volume.connect_change_value(move |_, _, value| {
        p.borrow().set_volume(value);
        glib::Propagation::Proceed
    });
    let dragging = Rc::new(Cell::new(0_u32));
    let d = dragging.clone();
    let p = player.clone();
    let pos = seek.pos.clone();
    seek.seek.connect_change_value(move |_, _, value| {
        let generation = d.get().wrapping_add(1);
        d.set(generation);
        pos.set_text(&controls::time(value));
        p.borrow().seek_fast(value);
        let (d, p) = (d.clone(), p.clone());
        glib::timeout_add_local_once(Duration::from_millis(250), move || {
            if d.get() == generation {
                d.set(0);
                p.borrow().seek(value);
            }
        });
        glib::Propagation::Proceed
    });
    let click = gtk::GestureClick::new();
    click.connect_released(|_, _, _, _| prefs::update(|p| p.time_left = !p.time_left));
    seek.dur.add_controller(click);

    let p = player.clone();
    fill_menu(&controls.speed, move |content, pop| {
        content.append(&widgets::label("SPEED", "popover-heading"));
        for speed in [0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 1.75, 2.0, 3.0] {
            let (p, pop) = (p.clone(), pop.clone());
            let selected = (p.borrow().number("speed") - speed).abs() < 0.01;
            content.append(&choice(&format!("{speed}×"), if speed == 1.0 { "normal" } else { "" }, selected, move || {
                p.borrow().set_number("speed", speed);
                pop.popdown();
            }));
        }
    });
    for (button, kind, heading, property) in
        [(&controls.audio, "audio", "SOUND", "aid"), (&controls.subs, "sub", "SUBTITLES", "sid")]
    {
        let p = player.clone();
        fill_menu(button, move |content, pop| {
            content.append(&widgets::label(heading, "popover-heading"));
            let tracks = p.borrow().tracks.as_array().cloned().unwrap_or_default();
            let tracks: Vec<_> = tracks.iter().filter(|t| t["type"].as_str() == Some(kind)).collect();
            if kind == "sub" {
                let selected = !p.borrow().subtitles_burned() && !tracks.iter().any(|t| t["selected"].as_bool() == Some(true));
                let (p, pop) = (p.clone(), pop.clone());
                content.append(&choice("Off", "", selected, move || {
                    p.borrow_mut().choose_track(property, "no");
                    pop.popdown();
                }));
            }
            let external = if kind == "sub" { p.borrow().subtitle_options() } else { vec![] };
            if tracks.is_empty() && external.is_empty() {
                content.append(&widgets::label(if kind == "audio" { "No sound" } else { "No subtitles" }, "dim"));
            }
            for (id, title, language, selected) in external {
                let (p, pop) = (p.clone(), pop.clone());
                content.append(&choice(&title, &language, selected, move || {
                    crate::playback::Playback::select_external_subtitle(&p, id);
                    pop.popdown();
                }));
            }
            for track in tracks {
                let id = track["id"].as_i64().unwrap_or_default().to_string();
                let label =
                    track["title"].as_str().or(track["lang"].as_str()).map(String::from).unwrap_or_else(|| format!("Track {id}"));
                let selected = track["selected"].as_bool().unwrap_or(false);
                let (p, pop) = (p.clone(), pop.clone());
                content.append(&choice(&label, track["codec"].as_str().unwrap_or_default(), selected, move || {
                    p.borrow_mut().choose_track(property, &id);
                    pop.popdown();
                }));
            }
        });
    }
    let p = player.clone();
    fill_menu(&controls.chapters, move |content, pop| {
        content.append(&widgets::label("CHAPTERS", "popover-heading"));
        let chapters = p.borrow().chapters.as_array().cloned().unwrap_or_default();
        let current = chapters.iter().rposition(|c| c["time"].as_f64().unwrap_or_default() <= p.borrow().position + 0.5);
        for (index, chapter) in chapters.iter().enumerate() {
            let time = chapter["time"].as_f64().unwrap_or_default();
            let title = chapter["title"].as_str().map(String::from).unwrap_or_else(|| format!("Chapter {}", index + 1));
            let (p, pop) = (p.clone(), pop.clone());
            content.append(&choice(&title, &controls::time(time), current == Some(index), move || {
                p.borrow().seek(time);
                pop.popdown();
            }));
        }
    });
    let fill = Rc::new(Cell::new(false));
    let (p, pic) = (player.clone(), picture.clone());
    fill_menu(&controls.shape, move |content, pop| {
        content.append(&widgets::label("SHAPE", "popover-heading"));
        for (ratio, label) in [("-1", "As made"), ("16:9", "16:9"), ("4:3", "4:3"), ("2.35:1", "2.35:1"), ("1:1", "Square")] {
            let (p, pop) = (p.clone(), pop.clone());
            content.append(&choice(label, "", false, move || {
                p.borrow().set_string("video-aspect-override", ratio);
                pop.popdown();
            }));
        }
        content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        let (fill, pic, pop) = (fill.clone(), pic.clone(), pop.clone());
        content.append(&choice("Fill the window", "crop", fill.get(), move || {
            fill.set(!fill.get());
            pic.set_content_fit(if fill.get() { gtk::ContentFit::Cover } else { gtk::ContentFit::Contain });
            pop.popdown();
        }));
    });
    let weak = Rc::downgrade(player);
    let subtitle_player = Rc::downgrade(player);
    let (surface, panel) = (overlay.clone(), controls.root.clone());
    let place_subs: Rc<dyn Fn(bool)> = Rc::new(move |shown| {
        let Some(p) = subtitle_player.upgrade() else { return };
        let p = p.borrow();
        let (vw, vh) = (p.number("dwidth"), p.number("dheight"));
        let (w, h) = (surface.width() as f64, surface.height() as f64);
        let top = panel.compute_bounds(&surface).map(|b| b.y() as f64);
        let position = if let (Some(top), true) = (top, shown && vw > 0.0 && vh > 0.0 && w > 0.0 && h > 0.0) {
            let shown_h = vh * (w / vw).min(h / vh);
            let overlap = (h + shown_h) / 2.0 - top;
            100.0 - if overlap > 0.0 { (overlap + 16.0) / shown_h * 100.0 } else { 0.0 }
        } else {
            100.0
        };
        p.set_number("sub-pos", position.clamp(50.0, 100.0));
    });
    let wake = controls::install_autohide(
        &overlay,
        &top,
        &controls.root,
        vec![
            controls.chapters.clone(),
            controls.shape.clone(),
            controls.speed.clone(),
            controls.audio.clone(),
            controls.subs.clone(),
        ],
        Rc::new(move || {
            weak.upgrade().is_some_and(|p| {
                let p = p.borrow();
                p.loaded && !p.paused
            })
        }),
        place_subs,
    );
    let p = player.clone();
    overlay.connect_unmap(move |_| p.borrow().set_number("sub-pos", 100.0));
    let click = gtk::GestureClick::new();
    let p = player.clone();
    let toggled = Rc::new(Cell::new(false));
    let t = toggled.clone();
    click.connect_pressed(move |g, n, _, _| {
        if n == 2 {
            if t.replace(false) {
                p.borrow().toggle();
            }
            window::toggle_fullscreen();
            g.set_state(gtk::EventSequenceState::Claimed);
        }
    });
    let p = player.clone();
    click.connect_released(move |_, n, _, _| {
        let first = n == 1 && p.borrow().loaded;
        toggled.set(first);
        if first {
            p.borrow().toggle();
        }
    });
    picture.add_controller(click);
    let weak_player = Rc::downgrade(player);
    let weak_overlay = overlay.downgrade();
    let last_paused = Cell::new(true);
    let last_title = RefCell::new(String::new());
    let last_chapters = RefCell::new(serde_json::Value::Null);
    glib::timeout_add_local(Duration::from_millis(200), move || {
        let (Some(player), Some(overlay)) = (weak_player.upgrade(), weak_overlay.upgrade()) else { return glib::ControlFlow::Break };
        // Nothing to update while the player page isn't on screen.
        if !overlay.is_mapped() {
            return glib::ControlFlow::Continue;
        }
        let p = player.borrow();
        controls.prev.set_sensitive(crate::playlists::can_step(-1));
        controls.next.set_sensitive(crate::playlists::can_step(1));
        let options = prefs::get();
        controls.root.set_visible(!p.title.is_empty());
        top.set_visible(!p.title.is_empty());
        let changed_title = *last_title.borrow() != p.title;
        if changed_title {
            title.set_text(&p.title);
            *last_title.borrow_mut() = p.title.clone();
        }
        let playing = p.loaded && !p.paused;
        controls.play.set_icon_name(if playing { "media-playback-pause-symbolic" } else { "media-playback-start-symbolic" });
        controls.play.set_tooltip_text(Some(if playing { "Pause (Space)" } else { "Play (Space)" }));
        controls.mute.set_icon_name(controls::volume_icon(options.volume, options.muted));
        controls.mute.set_tooltip_text(Some(if options.muted { "Unmute (M)" } else { "Mute (M)" }));
        if (controls.volume.value() - options.volume).abs() > 0.005 {
            controls.volume.set_value(options.volume);
        }
        controls.chapters.set_visible(p.chapters.as_array().is_some_and(|c| !c.is_empty()));
        let has_subs =
            !p.subtitle_options().is_empty() || p.tracks.as_array().is_some_and(|ts| ts.iter().any(|t| t["type"] == "sub"));
        if has_subs {
            controls.subs.remove_css_class("dim");
        } else {
            controls.subs.add_css_class("dim");
        }
        if *last_chapters.borrow() != p.chapters {
            seek.seek.clear_marks();
            if let Some(chapters) = p.chapters.as_array() {
                for c in chapters {
                    if let Some(t) = c["time"].as_f64().filter(|t| *t > 0.5) {
                        seek.seek.add_mark(t, gtk::PositionType::Bottom, None);
                    }
                }
            }
            *last_chapters.borrow_mut() = p.chapters.clone();
        }
        let duration = p.duration();
        if (seek.seek.adjustment().upper() - duration.max(1.0)).abs() > 0.5 {
            seek.seek.set_range(0.0, duration.max(1.0));
        }
        seek.seek.set_sensitive(p.loaded && duration > 0.0);
        if dragging.get() == 0 {
            seek.seek.set_value(p.position);
            seek.pos.set_text(&controls::time(p.position));
        }
        seek.dur.set_text(&if options.time_left && duration > 0.0 {
            format!("−{}", controls::time(duration - p.position))
        } else {
            controls::time(duration)
        });
        let speed = p.number("speed");
        controls.speed_label.set_text(&if (speed - 1.0).abs() > 0.001 && speed > 0.0 {
            format!("{speed}×")
        } else {
            String::new()
        });
        if let Some(w) = window::window() {
            controls.full.set_icon_name(if w.is_fullscreen() { "view-restore-symbolic" } else { "view-fullscreen-symbolic" });
        }
        // Drop the borrow before wake queries playback through the shared callback.
        let changed = last_paused.replace(p.paused) != p.paused || changed_title;
        drop(p);
        if changed {
            wake();
        }
        glib::ControlFlow::Continue
    });
    overlay
}

//! Keep the display and machine awake while a video plays, and a sleep timer
//! that stops playback and suspends at a chosen time.
//!
//! mpv draws through its render API and owns no window, so its own
//! `stop-screensaver` does nothing. The window holds a GTK idle/suspend
//! inhibitor instead, which hypridle and logind respect.

use crate::{playback::Playback, widgets, window};
use gtk::{glib, prelude::*};
use nyx_playback::controls::{choice, fill_menu};
use std::{
    cell::{Cell, RefCell},
    process::Command,
    rc::Rc,
    time::{Duration, Instant},
};

pub const INTERVALS_MIN: &[u32] = &[30, 60, 90, 120, 150, 180];

thread_local! {
    static COOKIE: Cell<Option<u32>> = const { Cell::new(None) };
    static PLAYING: Cell<bool> = const { Cell::new(false) };
    static MINUTES: Cell<u32> = const { Cell::new(0) };
    static ENDS: Cell<Option<Instant>> = const { Cell::new(None) };
    static SOURCE: RefCell<Option<glib::SourceId>> = const { RefCell::new(None) };
    static GEN: Cell<u64> = const { Cell::new(0) };
}

pub fn interval_label(minutes: u32) -> String {
    match minutes {
        0 => "Off".into(),
        m if m % 60 == 0 => format!("{} hr", m / 60),
        m if m < 60 => format!("{m} min"),
        m => format!("{} hr {} min", m / 60, m % 60),
    }
}

pub fn remaining_text(secs: u64) -> String {
    nyx_playback::controls::time(secs as f64)
}

pub fn selected_minutes() -> u32 {
    MINUTES.with(Cell::get)
}

pub fn remaining_secs() -> Option<u64> {
    ENDS.with(|e| e.get()).map(|ends| ends.saturating_duration_since(Instant::now()).as_secs())
}

/// Inhibit idle and suspend while a title is actually playing.
pub fn set_playing(playing: bool) {
    if PLAYING.replace(playing) == playing {
        return;
    }
    update_inhibit();
}

pub fn shutdown() {
    cancel_timer();
    PLAYING.set(false);
    update_inhibit();
}

pub fn install_button(player: &Rc<RefCell<Playback>>) -> (gtk::MenuButton, gtk::Label) {
    let readout = widgets::label("", "osd-speed");
    readout.add_css_class("mono");
    let button = gtk::MenuButton::new();
    button.set_icon_name("alarm-symbolic");
    button.set_tooltip_text(Some("Sleep timer"));
    button.add_css_class("flat");
    button.add_css_class("osd-button");
    button.set_focus_on_click(false);
    let pop = gtk::Popover::new();
    pop.add_css_class("menu-popover");
    pop.set_child(Some(&widgets::vbox(1)));
    button.set_popover(Some(&pop));
    let p = player.clone();
    fill_menu(&button, move |content, pop| {
        content.append(&widgets::label("SLEEP TIMER", "popover-heading"));
        let selected = selected_minutes();
        for minutes in std::iter::once(0).chain(INTERVALS_MIN.iter().copied()) {
            let (p, pop) = (p.clone(), pop.clone());
            content.append(&choice(&interval_label(minutes), "", selected == minutes, move || {
                arm(minutes, &p);
                pop.popdown();
            }));
        }
    });
    (button, readout)
}

pub fn refresh_button(button: &gtk::MenuButton, readout: &gtk::Label) {
    match remaining_secs() {
        Some(secs) => {
            let text = remaining_text(secs);
            readout.set_text(&text);
            button.set_tooltip_text(Some(&format!("Sleep in {text}")));
        }
        None => {
            readout.set_text("");
            button.set_tooltip_text(Some("Sleep timer"));
        }
    }
}

fn arm(minutes: u32, player: &Rc<RefCell<Playback>>) {
    cancel_timer();
    if minutes == 0 {
        return;
    }
    let ticket = GEN.get().wrapping_add(1);
    GEN.set(ticket);
    MINUTES.set(minutes);
    let wait = Duration::from_secs(u64::from(minutes) * 60);
    ENDS.set(Some(Instant::now() + wait));
    let player = player.clone();
    let source = glib::timeout_add_local(wait, move || {
        if GEN.get() == ticket {
            fire(&player);
        }
        glib::ControlFlow::Break
    });
    SOURCE.replace(Some(source));
}

fn cancel_timer() {
    bump_timer();
    if let Some(source) = SOURCE.take() {
        source.remove();
    }
}

fn bump_timer() {
    GEN.set(GEN.get().wrapping_add(1));
    MINUTES.set(0);
    ENDS.set(None);
}

fn fire(player: &Rc<RefCell<Playback>>) {
    // The firing source is already running; drop the id without removing it.
    SOURCE.take();
    bump_timer();
    PLAYING.set(false);
    update_inhibit();
    player.borrow_mut().close();
    window::leave_fullscreen();
    let ticket = GEN.get();
    glib::timeout_add_local_once(Duration::from_millis(250), move || {
        if GEN.get() != ticket {
            return;
        }
        if let Err(error) = Command::new("systemctl").arg("suspend").spawn() {
            window::status(&format!("Couldn't sleep: {error}"));
        }
    });
}

fn update_inhibit() {
    let want = PLAYING.get();
    let Some(win) = window::window() else {
        release();
        return;
    };
    let Some(app) = win.application() else {
        release();
        return;
    };
    match (want, COOKIE.get()) {
        (true, None) => {
            let flags = gtk::ApplicationInhibitFlags::IDLE | gtk::ApplicationInhibitFlags::SUSPEND;
            let cookie = app.inhibit(Some(&win), flags, Some("Playing a video"));
            COOKIE.set((cookie != 0).then_some(cookie));
        }
        (false, Some(cookie)) => {
            app.uninhibit(cookie);
            COOKIE.set(None);
        }
        _ => {}
    }
}

fn release() {
    let Some(cookie) = COOKIE.take() else { return };
    if let Some(win) = window::window()
        && let Some(app) = win.application()
    {
        app.uninhibit(cookie);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_labels() {
        assert_eq!(interval_label(0), "Off");
        assert_eq!(interval_label(30), "30 min");
        assert_eq!(interval_label(60), "1 hr");
        assert_eq!(interval_label(90), "1 hr 30 min");
        assert_eq!(interval_label(120), "2 hr");
        assert_eq!(interval_label(150), "2 hr 30 min");
        assert_eq!(interval_label(180), "3 hr");
    }

    #[test]
    fn remaining_labels() {
        assert_eq!(remaining_text(0), "0:00");
        assert_eq!(remaining_text(65), "1:05");
        assert_eq!(remaining_text(3661), "1:01:01");
        assert_eq!(INTERVALS_MIN, &[30, 60, 90, 120, 150, 180]);
    }
}

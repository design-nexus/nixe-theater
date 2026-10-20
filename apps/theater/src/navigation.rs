//! Two-finger touchpad scrolling is how Wayland reports a browser-style swipe.
use gtk::{gdk, glib, prelude::*};
use std::{cell::RefCell, rc::Rc};
#[derive(Default)]
struct Swipe {
    x: f64,
    y: f64,
    active: bool,
    blocked: bool,
}
impl Swipe {
    fn begin(&mut self) {
        *self = Self { active: true, ..Default::default() };
    }
    fn update(&mut self, x: f64, y: f64) {
        self.x += x;
        self.y += y.abs();
    }
    fn back(&self) -> bool {
        self.active && !self.blocked && self.x <= -120.0 && self.x.abs() > self.y * 2.0
    }
}
pub fn install(window: &gtk::ApplicationWindow) {
    let state = Rc::new(RefCell::new(Swipe::default()));
    let controller = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let s = state.clone();
    controller.connect_scroll_begin(move |_| s.borrow_mut().begin());
    let s = state.clone();
    let weak = window.downgrade();
    controller.connect_scroll(move |c, dx, dy| {
        if c.unit() != gdk::ScrollUnit::Surface {
            return glib::Propagation::Proceed;
        }
        let Some(window) = weak.upgrade() else { return glib::Propagation::Proceed };
        let mut swipe = s.borrow_mut();
        if crate::window::page() == "playing" || window.is_fullscreen() || nixe_ui::settings_card::is_open() {
            swipe.blocked = true;
            return glib::Propagation::Proceed;
        }
        if let Some((x, y)) = c.current_event().and_then(|e| e.position()) {
            let mut child = window.pick(x, y, gtk::PickFlags::DEFAULT);
            while let Some(widget) = child {
                if widget.is::<gtk::Range>()
                    || widget.is::<gtk::Text>()
                    || widget.is::<gtk::TextView>()
                    || widget.is::<gtk::Entry>()
                    || widget.is::<gtk::DropDown>()
                {
                    swipe.blocked = true;
                    break;
                }
                if let Some(scroll) = widget.downcast_ref::<gtk::ScrolledWindow>() {
                    let a = scroll.hadjustment();
                    if a.upper() - a.page_size() > a.lower() + 1.0 && a.value() > a.lower() + 1.0 {
                        swipe.blocked = true;
                        break;
                    }
                }
                child = widget.parent();
            }
        }
        swipe.update(dx, dy);
        glib::Propagation::Proceed
    });
    controller.connect_scroll_end(move |_| {
        let back = state.borrow().back();
        *state.borrow_mut() = Swipe::default();
        if back {
            crate::window::back();
        }
    });
    window.add_controller(controller);
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deliberate_horizontal_swipes_only() {
        let mut s = Swipe::default();
        s.begin();
        s.update(-140.0, 8.0);
        assert!(s.back());
        s.begin();
        s.update(-20.0, 200.0);
        assert!(!s.back());
        s.begin();
        s.update(160.0, 0.0);
        assert!(!s.back());
        s.begin();
        s.update(-200.0, 0.0);
        s.blocked = true;
        assert!(!s.back());
    }
}

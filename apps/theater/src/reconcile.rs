//! Explicit, per-conflict review. Never writes from indexing or playback callbacks.
use crate::{prefs, servers, widgets, window};
use gtk::{glib, prelude::*};
use nixe_media_model::{
    index::unify,
    reconcile::{changed, desired, progress_compatible},
    *,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
thread_local! {static ACTIVE:std::cell::RefCell<Option<Arc<AtomicBool>>>=const{std::cell::RefCell::new(None)};}
pub fn busy() -> bool {
    ACTIVE.with(|active| active.borrow().as_ref().is_some_and(|job| job.load(Ordering::Relaxed)))
}
pub fn cancel() {
    ACTIVE.with(|active| {
        if let Some(job) = active.borrow().as_ref() {
            job.store(false, Ordering::Relaxed)
        }
    })
}
fn finish(job: &Arc<AtomicBool>) {
    ACTIVE.with(|active| {
        if active.borrow().as_ref().is_some_and(|current| Arc::ptr_eq(current, job)) {
            active.borrow_mut().take();
        }
    })
}
pub fn open() {
    if prefs::get().reconciliation != Reconciliation::Manual {
        window::status("Enable watch-state review in Settings first.");
        return;
    }
    let (dialog, body) = widgets::dialog("Review watch state", 760);
    let hint = widgets::label(
        "Choose which server's state to copy for each title. Different editions copy only watched status. Servers are checked again before writing.",
        "dim",
    );
    hint.set_wrap(true);
    body.append(&hint);
    let content = widgets::vbox(12);
    let scroll = gtk::ScrolledWindow::builder()
        .max_content_height(500)
        .propagate_natural_height(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::External)
        .child(&content)
        .build();
    body.append(&scroll);
    let alive = Arc::new(AtomicBool::new(true));
    let live = alive.clone();
    dialog.connect_close_request(move |_| {
        live.store(false, Ordering::Relaxed);
        glib::Propagation::Proceed
    });
    let message = widgets::label("", "dim");
    message.set_wrap(true);
    body.append(&message);
    // Matching every title across servers is the slow part: off the GTK thread.
    let looking = widgets::label("Looking for titles whose watch state differs…", "dim");
    content.append(&looking);
    let items = servers::items();
    nixe_core::cmd::background(
        move || unify(items),
        move |groups| {
            content.remove(&looking);
            let mut count = 0;
            for group in groups {
                for (left, source) in group.sources.iter().enumerate() {
                    if !matches!(source.kind, MediaKind::Movie | MediaKind::Episode) {
                        continue;
                    }
                    for target in group.sources.iter().skip(left + 1) {
                        if source.id.server.provider == target.id.server.provider
                            || (source.watch.watched == target.watch.watched
                                && source.watch.position_ms.abs_diff(target.watch.position_ms) <= 5000)
                        {
                            continue;
                        }
                        let (Some(a), Some(b)) = (servers::client(&source.id.server), servers::client(&target.id.server)) else {
                            continue;
                        };
                        count += 1;
                        let card = widgets::vbox(8);
                        card.add_css_class("group-list");
                        let title = widgets::label(&source.title, "settings-option-title");
                        title.set_wrap(true);
                        card.append(&title);
                        for (item, label) in [(source, a.label()), (target, b.label())] {
                            let state = format!(
                                "{label}: {} · {} min · {}",
                                if item.watch.watched { "Watched" } else { "Unwatched" },
                                item.watch.position_ms / 60000,
                                item.versions.first().and_then(|v| v.edition.as_deref()).unwrap_or("Original")
                            );
                            let state = widgets::label(&state, "dim");
                            state.set_wrap(true);
                            card.append(&state);
                        }
                        let actions = widgets::hbox(8);
                        let compatible = progress_compatible(source, target);
                        for (from, to, _client, label) in [
                            (source.clone(), target.clone(), b.clone(), a.label()),
                            (target.clone(), source.clone(), a.clone(), b.label()),
                        ] {
                            let button = gtk::Button::with_label(&format!(
                                "Use {label} {}",
                                if compatible { "progress" } else { "status" }
                            ));
                            let message = message.clone();
                            let alive = alive.clone();
                            let weak_card = card.downgrade();
                            button.connect_clicked(move |button| {
                                if prefs::get().reconciliation != Reconciliation::Manual {
                                    return;
                                }
                                if window::server_playing(&from.id.server) || window::server_playing(&to.id.server) {
                                    message.set_text("Stop playback on these servers before reconciling watch state.");
                                    return;
                                }
                                if busy() {
                                    message.set_text("Wait for the current watch-state update to finish.");
                                    return;
                                }
                                let (Some(source_client), Some(client)) =
                                    (servers::client(&from.id.server), servers::client(&to.id.server))
                                else {
                                    message.set_text("Reconnect both servers before reconciling watch state.");
                                    return;
                                };
                                let job = Arc::new(AtomicBool::new(true));
                                ACTIVE.with(|active| *active.borrow_mut() = Some(job.clone()));
                                button.set_sensitive(false);
                                let weak = button.downgrade();
                                let (from, to, alive, message, weak_card) =
                                    (from.clone(), to.clone(), alive.clone(), message.clone(), weak_card.clone());
                                let revision = servers::revision();
                                let worker_job = job.clone();
                                nixe_core::cmd::background(
                                    move || {
                                        let source = source_client.detail(&from.id)?;
                                        let target = client.detail(&to.id)?;
                                        if changed(&source.watch, &from.watch) || changed(&target.watch, &to.watch) {
                                            return Err(ProviderError::Failed(
                                                "Watch state changed on a server. Refresh libraries and review again.".into(),
                                            ));
                                        }
                                        if !alive.load(Ordering::Relaxed) || !worker_job.load(Ordering::Relaxed) {
                                            return Err(ProviderError::Failed("Review closed".into()));
                                        }
                                        let state = desired(&source, &target);
                                        let mut result = client.apply_watch(&target.id, &state)?;
                                        result.library_id = to.library_id;
                                        servers::database()?.record_playback(&result, revision).map_err(|_| {
                                            ProviderError::Failed(
                                                "State updated, but the local cache could not be saved. Refresh libraries."
                                                    .into(),
                                            )
                                        })?;
                                        Ok(result)
                                    },
                                    move |result| {
                                        finish(&job);
                                        match result {
                                            Ok(item) => {
                                                servers::playback_watch(item, revision);
                                                if let Some(card) = weak_card.upgrade() {
                                                    card.set_sensitive(false)
                                                }
                                                message.set_text("Watch state updated.");
                                            }
                                            Err(error) => {
                                                message.set_text(&error.to_string());
                                                if let Some(button) = weak.upgrade() {
                                                    button.set_sensitive(true)
                                                }
                                            }
                                        }
                                    },
                                );
                            });
                            actions.append(&button);
                        }
                        card.append(&actions);
                        content.append(&card);
                    }
                }
            }
            if count == 0 {
                content.append(&widgets::label("No conflicting watch state between connected servers.", "dim"));
            }
        },
    );
    let close = gtk::Button::with_label("Close");
    close.set_halign(gtk::Align::End);
    let weak = dialog.downgrade();
    close.connect_clicked(move |_| {
        if let Some(dialog) = weak.upgrade() {
            dialog.close()
        }
    });
    body.append(&close);
    dialog.present();
}

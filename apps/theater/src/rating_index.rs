//! Populate omitted Plex rating fields only when a sourced-rating sort needs them.
use nixe_media_model::*;
use std::{
    cell::RefCell,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
#[derive(Default)]
struct Job {
    key: String,
    cancelled: Arc<AtomicBool>,
    running: bool,
}
thread_local! {static JOB:RefCell<Job>=RefCell::new(Job::default());}
pub fn cancel() {
    JOB.with(|j| {
        let mut j = j.borrow_mut();
        j.cancelled.store(true, Ordering::Relaxed);
        *j = Job::default();
    });
}
pub fn ensure(page: &str, source: &str) {
    let kind = match page {
        "tv" => MediaKind::Series,
        "movies" => MediaKind::Movie,
        _ => {
            cancel();
            return;
        }
    };
    if !matches!(source, "imdb" | "rt_critic" | "rt_audience" | "metacritic") {
        cancel();
        return;
    }
    let Some(client) = crate::plex::client() else { return };
    let key = format!("{:?}/{page}/{source}", client.server_id());
    let cancelled = JOB.with(|job| {
        let mut j = job.borrow_mut();
        if j.running && j.key == key && !j.cancelled.load(Ordering::Relaxed) {
            return None;
        }
        j.cancelled.store(true, Ordering::Relaxed);
        let cancelled = Arc::new(AtomicBool::new(false));
        *j = Job { key: key.clone(), cancelled: cancelled.clone(), running: true };
        Some(cancelled)
    });
    let Some(cancelled) = cancelled else { return };
    let snapshot = crate::catalog::snapshot();
    let source = source.to_owned();
    let server = client.server_id().clone();
    let scope = server.clone();
    let (tx, rx) = async_channel::bounded(2);
    let stop = cancelled.clone();
    std::thread::spawn(move || {
        let items: Vec<_> = snapshot
            .items()
            .filter(|i| i.kind == kind && i.id.server == server)
            .filter_map(|i| {
                let mut i = i.clone();
                crate::metadata::apply(&mut i);
                (!i.metadata.ratings.iter().any(|r| r.source == source) && !crate::metadata::provider_ratings_fresh(&i))
                    .then_some(i)
            })
            .collect();
        let mut processed = 0;
        let mut notified = 0;
        let mut notice = Instant::now();
        let mut failure = None;
        for chunk in items.chunks(24) {
            if stop.load(Ordering::Relaxed) || tx.is_closed() {
                return;
            }
            let ids = chunk.iter().map(|i| i.id.clone()).collect::<Vec<_>>();
            match client.rating_details(&ids) {
                Ok(details) => {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    let overlays: Vec<_> = chunk
                        .iter()
                        .map(|base| {
                            let mut overlay = base.clone();
                            overlay.metadata.ratings =
                                details.iter().find(|i| i.id == base.id).map(|i| i.metadata.ratings.clone()).unwrap_or_default();
                            overlay
                        })
                        .collect();
                    crate::metadata::store_provider_ratings(&overlays);
                    processed += overlays.len();
                }
                Err(e) => {
                    failure = Some(e);
                    break;
                }
            }
            if notice.elapsed() >= Duration::from_secs(2) {
                notified = processed;
                if tx.send_blocking((processed, false, None)).is_err() {
                    return;
                }
                notice = Instant::now();
            }
        }
        let _ = tx.send_blocking((processed.saturating_sub(notified), true, failure));
    });
    gtk::glib::spawn_future_local(async move {
        while let Ok((changed, done, error)) = rx.recv().await {
            if cancelled.load(Ordering::Relaxed) || crate::window::is_closing() || !crate::servers::configured(&scope) {
                break;
            }
            if done {
                JOB.with(|j| {
                    let mut j = j.borrow_mut();
                    if j.key == key {
                        j.running = false;
                    }
                });
            }
            if changed > 0 {
                crate::library::refresh();
            }
            if let Some(error) = error {
                crate::window::status(&format!("{error} · using cached ratings"));
            }
        }
    });
}

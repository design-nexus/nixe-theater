//! Server lists are read-only. Local curation and network operations stay on workers.
use crate::{
    playlist_store::{Entry, Playlist, Store},
    prefs, servers, widgets, window,
};
use gtk::{glib, prelude::*};
use nixe_media_model::{MediaItem, MediaKind, ProviderError};
use std::{cell::RefCell, rc::Rc};
struct Ui {
    body: gtk::Box,
    lists: Vec<Playlist>,
    selected: Option<String>,
    generation: u64,
}
#[derive(Default)]
struct Queue {
    entries: Vec<Entry>,
    index: Option<usize>,
    generation: u64,
}
thread_local! {
    static UI:RefCell<Option<Ui>>=const{RefCell::new(None)};
    static QUEUE:RefCell<Queue>=RefCell::new(Queue::default());
}
pub fn build(stack: &gtk::Stack) {
    let page = widgets::page("playlists");
    stack.add_named(&page.root, Some("playlists"));
    UI.with(|ui| *ui.borrow_mut() = Some(Ui { body: page.body, lists: vec![], selected: None, generation: 0 }));
}
pub fn show() {
    reload(true);
}
pub fn on_connection_changed() {
    if prefs::get().last_section == "playlists" {
        reload(true);
    }
}
pub fn cancel() {
    clear_queue();
    UI.with(|ui| {
        if let Some(ui) = ui.borrow_mut().as_mut() {
            ui.generation += 1;
        }
    });
}
/// A new generation for a load; 0 (never current) once the page has gone.
fn generation() -> u64 {
    UI.with(|ui| {
        let mut ui = ui.borrow_mut();
        let Some(ui) = ui.as_mut() else { return 0 };
        ui.generation += 1;
        ui.generation
    })
}
fn apply(lists: Vec<Playlist>, generation: u64) {
    if window::is_closing() {
        return;
    }
    let valid = UI.with(|ui| {
        let mut ui = ui.borrow_mut();
        let Some(ui) = ui.as_mut() else { return false };
        if ui.generation != generation {
            return false;
        }
        let mut lists: Vec<_> =
            lists.into_iter().filter(|list| list.origin.as_ref().is_none_or(|id| servers::configured(&id.server))).collect();
        lists.sort_by_cached_key(|list| list.title.to_lowercase());
        if ui.lists == lists && ui.body.first_child().is_some() {
            return false;
        }
        ui.lists = lists;
        true
    });
    if valid {
        render();
    }
}
fn clients() -> Vec<servers::Client> {
    let p = prefs::get();
    p.plex
        .and_then(|p| servers::client(&p.server))
        .into_iter()
        .chain(p.jellyfin.and_then(|p| servers::client(&p.server)))
        .collect()
}
fn reload(network: bool) {
    let generation = generation();
    let clients = if network { clients() } else { vec![] };
    let (send, receive) = async_channel::bounded(2);
    std::thread::spawn(move || {
        let result = (|| {
            let store = Store::open()?;
            let cached = store.list()?;
            if send.send_blocking(Ok(cached)).is_err() {
                return Ok(());
            }
            for client in clients {
                let result = (|| {
                    let mut next = None;
                    let mut pages = 0;
                    loop {
                        if send.is_closed() {
                            return Ok(());
                        }
                        let page = client.playlists(next.as_deref())?;
                        for metadata in page.items {
                            let key = format!(
                                "server:{}",
                                serde_json::to_string(&metadata.id).map_err(|_| ProviderError::InvalidResponse)?
                            );
                            let mut list = store.load(&key).unwrap_or_else(|_| Playlist {
                                id: key,
                                title: metadata.title.clone(),
                                origin: Some(metadata.id),
                                entries: vec![],
                                count: metadata.count,
                                poster: metadata.poster.clone(),
                            });
                            list.poster = metadata.poster;
                            list.title = metadata.title;
                            list.count = metadata.count;
                            store.save(&list)?;
                        }
                        next = page.next;
                        pages += 1;
                        if next.is_none() {
                            break;
                        }
                        if pages >= 500 {
                            return Err(ProviderError::Failed("Playlist limit reached".into()));
                        }
                    }
                    Ok::<_, ProviderError>(())
                })();
                if let Err(error) = result {
                    let _ = send.send_blocking(Err(error));
                }
                let _ = send.send_blocking(Ok(store.list()?));
            }
            Ok::<_, ProviderError>(())
        })();
        if let Err(error) = result {
            let _ = send.send_blocking(Err(error));
        }
    });
    glib::spawn_future_local(async move {
        while let Ok(result) = receive.recv().await {
            if window::is_closing() || !UI.with(|ui| ui.borrow().as_ref().is_some_and(|ui| ui.generation == generation)) {
                break;
            }
            match result {
                Ok(lists) => apply(lists, generation),
                Err(e) => {
                    if UI.with(|ui| ui.borrow().as_ref().is_some_and(|ui| ui.generation == generation)) {
                        window::status(&format!("{e} · showing saved playlists"));
                    }
                }
            }
        }
    });
}
/// Show one playlist (or, with None, all of them); nothing if the page has gone.
fn select(id: Option<String>) {
    UI.with(|ui| {
        if let Some(ui) = ui.borrow_mut().as_mut() {
            ui.selected = id;
        }
    });
}
fn render() {
    let state = UI.with(|ui| ui.borrow().as_ref().map(|ui| (ui.body.clone(), ui.lists.clone(), ui.selected.clone())));
    let Some((body, lists, selected)) = state else { return };
    while let Some(child) = body.first_child() {
        body.remove(&child);
    }
    let toolbar = widgets::hbox(8);
    let new = nixe_ui::widgets::icon_button("list-add-symbolic", "New playlist");
    new.connect_clicked(|_| name_dialog(None, None));
    toolbar.append(&new);
    let refresh = nixe_ui::widgets::icon_button("view-refresh-symbolic", "Refresh server playlists");
    refresh.connect_clicked(|_| reload(true));
    toolbar.append(&refresh);
    body.append(&toolbar);
    if let Some(list) = selected.and_then(|id| lists.iter().find(|p| p.id == id).cloned()) {
        let back = nixe_ui::widgets::icon_button("go-previous-symbolic", "All playlists");
        back.set_halign(gtk::Align::Start);
        back.connect_clicked(|_| {
            select(None);
            render();
        });
        body.append(&back);
        let title = widgets::label(&list.title, "card-title");
        title.set_wrap(true);
        body.append(&title);
        let actions = widgets::hbox(8);
        let play = nixe_ui::widgets::icon_button("media-playback-start-symbolic", "Play playlist");
        play.add_css_class("suggested-action");
        let entries = list.entries.clone();
        play.set_sensitive(!entries.is_empty());
        play.connect_clicked(move |_| start(entries.clone(), 0));
        actions.append(&play);
        if list.origin.is_none() {
            let rename = nixe_ui::widgets::icon_button("document-edit-symbolic", "Rename playlist");
            let selected = list.clone();
            rename.connect_clicked(move |_| name_dialog(Some(selected.clone()), None));
            actions.append(&rename);
            let delete = nixe_ui::widgets::icon_button("user-trash-symbolic", "Delete playlist");
            let selected = list.clone();
            delete.connect_clicked(move |_| delete_dialog(selected.clone()));
            actions.append(&delete);
        } else {
            body.append(&widgets::label("Server playlist · read-only", "dim"));
        }
        body.append(&actions);
        if list.entries.is_empty() {
            body.append(&widgets::label("No playable videos yet. Add movies or episodes from their details.", "dim"));
        }
        let list = Rc::new(list);
        let model = gtk::gio::ListStore::new::<glib::BoxedAnyObject>();
        for index in 0..list.entries.len() {
            model.append(&glib::BoxedAnyObject::new(index));
        }
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_bind(move |_, row| {
            let row = row.downcast_ref::<gtk::ListItem>().expect("playlist row");
            let Some(value) = row.item().and_downcast::<glib::BoxedAnyObject>() else { return };
            let index = *value.borrow::<usize>();
            row.set_child(Some(&entry_row(&list, index, &list.entries[index])));
        });
        factory.connect_unbind(|_, row| {
            if let Some(row) = row.downcast_ref::<gtk::ListItem>() {
                row.set_child(None::<&gtk::Widget>);
            }
        });
        let view = gtk::ListView::new(Some(gtk::NoSelection::new(Some(model))), Some(factory));
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::External)
            .min_content_height(400)
            .vexpand(true)
            .child(&view)
            .build();
        body.append(&scroll);
    } else {
        if lists.is_empty() {
            body.append(&widgets::label("Create a playlist, or connect a server to load its video playlists.", "dim"));
        }
        let data = Rc::new(lists);
        let model = gtk::gio::ListStore::new::<glib::BoxedAnyObject>();
        for index in 0..data.len() {
            model.append(&glib::BoxedAnyObject::new(index));
        }
        let factory = gtk::SignalListItemFactory::new();
        let cards = data.clone();
        factory.connect_bind(move |_, obj| {
            let row = obj.downcast_ref::<gtk::ListItem>().expect("playlist card");
            let Some(value) = row.item().and_downcast::<glib::BoxedAnyObject>() else { return };
            let list = &cards[*value.borrow::<usize>()];
            let width = crate::presentation::width();
            let art = nixe_ui::media::Artwork::new(width, width * 3 / 2, "view-list-symbolic");
            let card = nixe_ui::media::PosterCard::new(&art.root, width);
            card.title.set_text(&list.title);
            card.title.set_tooltip_text(Some(&list.title));
            let source = list
                .origin
                .as_ref()
                .map(|id| match id.server.provider {
                    nixe_media_model::ProviderKind::Plex => "Plex",
                    _ => "Jellyfin",
                })
                .unwrap_or("Theater");
            card.meta.set_text(&format!("{source} · {} videos", list.count.unwrap_or(list.entries.len() as u64)));
            crate::presentation::register(&art, &card);
            crate::artwork::load_playlist(&art, list, 420, 630);
            row.set_child(Some(&card.root));
        });
        factory.connect_unbind(|_, obj| {
            if let Some(row) = obj.downcast_ref::<gtk::ListItem>() {
                row.set_child(None::<&gtk::Widget>);
            }
        });
        let view = gtk::GridView::new(Some(gtk::NoSelection::new(Some(model))), Some(factory));
        view.add_css_class("poster-grid");
        view.set_min_columns(2);
        view.set_max_columns(12);
        view.set_single_click_activate(true);
        view.connect_activate(move |_, position| {
            if let Some(list) = data.get(position as usize) {
                open(list.clone());
            }
        });
        body.append(
            &gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vscrollbar_policy(gtk::PolicyType::External)
                .min_content_height(440)
                .vexpand(true)
                .child(&view)
                .build(),
        );
    }
}
fn entry_row(list: &Rc<Playlist>, position: usize, entry: &Entry) -> gtk::Box {
    let line = widgets::hbox(8);
    let media = servers::latest_watch(entry.media.clone());
    let (title, subtitle, detail) = nixe_media_model::tv::labels(&media);
    let labels = widgets::vbox(2);
    labels.set_hexpand(true);
    let title = widgets::label(&title, "settings-option-title");
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.set_max_width_chars(1);
    labels.append(&title);
    let offline = servers::client(&media.id.server).is_none();
    let description = [subtitle, detail, if offline { "Unavailable · reconnect the source".into() } else { String::new() }]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    let meta = widgets::label(&description, "dim");
    meta.set_wrap(true);
    meta.set_max_width_chars(1);
    labels.append(&meta);
    line.append(&labels);
    let play = gtk::Button::from_icon_name("media-playback-start-symbolic");
    play.set_tooltip_text(Some("Play from here"));
    let selected = list.clone();
    play.connect_clicked(move |_| start(selected.entries.clone(), position));
    line.append(&play);
    let details = gtk::Button::from_icon_name("dialog-information-symbolic");
    details.set_tooltip_text(Some("Details"));
    details.connect_clicked(move |_| crate::library::show_detail(media.clone()));
    line.append(&details);
    if list.origin.is_none() {
        for (icon, delta) in [("go-up-symbolic", -1), ("go-down-symbolic", 1)] {
            let button = nixe_ui::widgets::icon_button(icon, if delta < 0 { "Move up" } else { "Move down" });
            button.set_sensitive(if delta < 0 { position > 0 } else { position + 1 < list.entries.len() });
            let (id, key) = (list.id.clone(), entry.id.clone());
            button.connect_clicked(move |_| {
                let key = key.clone();
                edit(id.clone(), move |list| list.move_entry(&key, delta));
            });
            line.append(&button);
        }
        let remove = nixe_ui::widgets::icon_button("list-remove-symbolic", "Remove from playlist");
        let (id, key) = (list.id.clone(), entry.id.clone());
        remove.connect_clicked(move |_| {
            let key = key.clone();
            edit(id.clone(), move |list| list.entries.retain(|e| e.id != key));
        });
        line.append(&remove);
    }
    line
}
fn open(list: Playlist) {
    select(Some(list.id.clone()));
    render();
    let Some(origin) = list.origin.clone() else { return };
    let Some(client) = servers::client(&origin.server) else {
        window::status("Server offline · showing saved playlist entries");
        return;
    };
    let generation = generation();
    window::status("Loading playlist…");
    nixe_core::cmd::background(
        move || {
            let mut list = list;
            let mut entries = vec![];
            let mut cursor = None;
            let mut pages = 0;
            loop {
                pages += 1;
                if pages > 500 {
                    return Err(ProviderError::Failed("Playlist limit reached".into()));
                }
                let page = client.playlist_items(&origin, cursor.as_deref())?;
                entries.extend(page.items);
                cursor = page.next;
                if entries.len() > 50000 {
                    return Err(ProviderError::InvalidResponse);
                }
                if cursor.is_none() {
                    break;
                }
            }
            list.entries = entries.into_iter().map(|media| Entry { id: uuid::Uuid::new_v4().to_string(), media }).collect();
            list.count = Some(list.entries.len() as u64);
            let store = Store::open()?;
            store.save(&list)?;
            store.list()
        },
        move |result| {
            if window::is_closing() || !UI.with(|ui| ui.borrow().as_ref().is_some_and(|ui| ui.generation == generation)) {
                return;
            }
            match result {
                Ok(lists) => {
                    apply(lists, generation);
                    window::status("Playlist updated");
                }
                Err(e) => window::status(&format!("{e} · showing saved playlist entries")),
            }
        },
    );
}
fn edit(id: String, change: impl FnOnce(&mut Playlist) + Send + 'static) {
    nixe_core::cmd::background(
        move || Store::open()?.edit(&id, change),
        |result| match result {
            Ok(()) => reload(false),
            Err(e) => window::status(&e.to_string()),
        },
    );
}
fn name_dialog(existing: Option<Playlist>, item: Option<MediaItem>) {
    let (dialog, body) = widgets::dialog(if existing.is_some() { "Rename playlist" } else { "New playlist" }, 420);
    let name = gtk::Entry::new();
    name.set_placeholder_text(Some("Playlist name"));
    name.set_max_length(120);
    if let Some(list) = &existing {
        name.set_text(&list.title);
    }
    body.append(&name);
    let actions = widgets::hbox(8);
    let cancel = gtk::Button::with_label("Cancel");
    let weak = dialog.downgrade();
    cancel.connect_clicked(move |_| {
        if let Some(d) = weak.upgrade() {
            d.close();
        }
    });
    actions.append(&cancel);
    let save = gtk::Button::with_label("Save");
    let weak = dialog.downgrade();
    save.connect_clicked(move |_| {
        let title = name.text().trim().to_owned();
        if title.is_empty() {
            name.grab_focus();
            return;
        }
        if let Some(list) = &existing {
            edit(list.id.clone(), move |list| list.title = title);
        } else {
            let mut list = Playlist::new(title);
            if let Some(item) = &item {
                list.add(item.clone());
            }
            nixe_core::cmd::background(
                move || Store::open()?.save(&list),
                |result| match result {
                    Ok(()) => reload(false),
                    Err(e) => window::status(&e.to_string()),
                },
            );
        }
        if let Some(dialog) = weak.upgrade() {
            dialog.close();
        }
    });
    actions.append(&save);
    body.append(&actions);
    dialog.present();
}
fn delete_dialog(list: Playlist) {
    let (dialog, body) = widgets::dialog("Delete playlist?", 420);
    let label = widgets::label(&format!("Delete “{}” from Theater? Your media files stay in place.", list.title), "dim");
    label.set_wrap(true);
    body.append(&label);
    let actions = widgets::hbox(8);
    let cancel = gtk::Button::with_label("Cancel");
    let weak = dialog.downgrade();
    cancel.connect_clicked(move |_| {
        if let Some(d) = weak.upgrade() {
            d.close();
        }
    });
    actions.append(&cancel);
    let delete = gtk::Button::with_label("Delete");
    let weak = dialog.downgrade();
    delete.connect_clicked(move |_| {
        let id = list.id.clone();
        nixe_core::cmd::background(
            move || Store::open()?.delete_local(&id),
            |result| match result {
                Ok(()) => {
                    select(None);
                    reload(false);
                }
                Err(e) => window::status(&e.to_string()),
            },
        );
        if let Some(d) = weak.upgrade() {
            d.close();
        }
    });
    actions.append(&delete);
    body.append(&actions);
    dialog.present();
}
pub fn add_dialog(item: MediaItem) {
    nixe_core::cmd::background(
        || Store::open()?.list(),
        move |result| {
            let lists = match result {
                Ok(lists) => lists,
                Err(e) => {
                    window::status(&e.to_string());
                    return;
                }
            };
            if window::is_closing() {
                return;
            }
            let (dialog, body) = widgets::dialog("Add to playlist", 440);
            for list in lists.into_iter().filter(|p| p.origin.is_none()) {
                let button = gtk::Button::with_label(&list.title);
                let selected = item.clone();
                let weak = dialog.downgrade();
                button.connect_clicked(move |_| {
                    let item = selected.clone();
                    edit(list.id.clone(), move |list| list.add(item));
                    if let Some(d) = weak.upgrade() {
                        d.close();
                    }
                });
                body.append(&button);
            }
            let new = gtk::Button::with_label("New playlist");
            let weak = dialog.downgrade();
            new.connect_clicked(move |_| {
                if let Some(d) = weak.upgrade() {
                    d.close();
                }
                name_dialog(None, Some(item.clone()));
            });
            body.append(&new);
            let close = gtk::Button::with_label("Close");
            let weak = dialog.downgrade();
            close.connect_clicked(move |_| {
                if let Some(d) = weak.upgrade() {
                    d.close();
                }
            });
            body.append(&close);
            dialog.present();
        },
    );
}
pub fn clear_queue() {
    QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        q.entries.clear();
        q.index = None;
        q.generation += 1;
    });
}
pub fn start(entries: Vec<Entry>, position: usize) {
    QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        q.entries = entries;
        q.index = None;
        q.generation += 1;
    });
    play_at(position, 1);
}
fn next_index(index: Option<usize>, delta: isize, len: usize) -> Option<usize> {
    index.and_then(|index| index.checked_add_signed(delta)).filter(|&n| n < len)
}
/// True while a playlist is being played through.
pub fn has_queue() -> bool {
    QUEUE.with(|q| !q.borrow().entries.is_empty())
}
pub fn can_step(delta: isize) -> bool {
    QUEUE.with(|q| {
        let q = q.borrow();
        next_index(q.index, delta, q.entries.len()).is_some()
    })
}
pub fn advance(delta: isize) {
    let (index, has_queue) = QUEUE.with(|q| {
        let q = q.borrow();
        (next_index(q.index, delta, q.entries.len()), !q.entries.is_empty())
    });
    if let Some(index) = index {
        play_at(index, delta);
    } else if has_queue && delta > 0 {
        window::status("Playlist finished.");
    }
}
fn play_at(mut index: usize, delta: isize) {
    if delta != 1 && delta != -1 {
        return;
    }
    let mut skipped = false;
    let candidate = QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        while index < q.entries.len() {
            let media = q.entries[index].media.clone();
            if let Some(client) = servers::client(&media.id.server) {
                q.index = Some(index);
                q.generation += 1;
                return Some((media, client, q.generation));
            }
            skipped = true;
            let Some(next) = index.checked_add_signed(delta) else { break };
            index = next;
        }
        None
    });
    let Some((media, client, generation)) = candidate else {
        window::status("No more available videos in this playlist.");
        return;
    };
    let Some(request) = window::begin_playback_request(media.id.server.clone()) else { return };
    window::status(if skipped {
        "Skipping unavailable entries · opening playlist video…"
    } else {
        "Opening playlist video…"
    });
    nixe_core::cmd::background(
        move || client.detail(&media.id),
        move |result| {
            if window::is_closing()
                || !window::accepts_playback_request(request)
                || !QUEUE.with(|q| q.borrow().generation == generation)
            {
                return;
            }
            match result {
                Ok(item) if matches!(item.kind, MediaKind::Movie | MediaKind::Episode) && crate::library::play_queued(&item) => {}
                _ => {
                    window::status("Playlist video unavailable · trying the next entry…");
                    advance(delta);
                }
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn queue_boundaries_and_clear_invalidate_pending_entries() {
        assert_eq!(next_index(Some(0), -1, 3), None);
        assert_eq!(next_index(Some(0), 1, 3), Some(1));
        assert_eq!(next_index(Some(2), 1, 3), None);
        assert_eq!(next_index(None, 1, 3), None);
        assert_eq!(next_index(Some(usize::MAX), 1, 3), None);
        let generation = QUEUE.with(|q| q.borrow().generation);
        clear_queue();
        assert!(QUEUE.with(|q| q.borrow().generation) > generation);
        assert!(!can_step(1));
    }
}

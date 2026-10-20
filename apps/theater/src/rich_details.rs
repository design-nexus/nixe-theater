//! Native details and person pages, using the suite's cards and artwork widgets.
use crate::{library, metadata, prefs, servers, widgets, window};
use gtk::{glib, prelude::*};
use nixe_media_model::*;
use nixe_ui::media::Artwork;
fn prose(body: &gtk::Box, text: &str, class: &str) {
    if text.is_empty() {
        return;
    }
    let label = widgets::label(text, class);
    label.set_wrap(true);
    label.set_max_width_chars(1);
    label.set_hexpand(true);
    label.add_css_class("theater-prose");
    body.append(&label);
}
fn expand(body: &gtk::Box, title: &str, text: &str) {
    if text.is_empty() {
        return;
    }
    let exp = gtk::Expander::new(Some(title));
    exp.add_css_class("theater-disclosure");
    let child = widgets::vbox(8);
    prose(&child, text, "dim");
    exp.set_child(Some(&child));
    body.append(&exp);
}
pub fn refresh_current() {
    library::rerender_current();
}
pub fn intro(body: &gtk::Box, i: &MediaItem) {
    let overview = widgets::vbox(12);
    overview.add_css_class("theater-overview");
    body.append(&overview);
    let body = &overview;
    let ratings = gtk::FlowBox::new();
    ratings.add_css_class("theater-ratings");
    ratings.set_selection_mode(gtk::SelectionMode::None);
    ratings.set_halign(gtk::Align::Start);
    ratings.set_hexpand(false);
    ratings.set_homogeneous(false);
    ratings.set_max_children_per_line(8);
    ratings.set_min_children_per_line(1);
    ratings.set_column_spacing(12);
    ratings.set_row_spacing(8);
    for r in &i.metadata.ratings {
        let content = widgets::hbox(6);
        content.set_halign(gtk::Align::Start);
        content.append(&crate::rating_icons::icon(&r.source));
        let score = if r.scale == 10000 {
            if r.source.starts_with("rt_") { format!("{}%", r.value / 100) } else { format!("{}", r.value / 100) }
        } else {
            format!("{:.1}", r.value as f64 / 100.0)
        };
        content.append(&widgets::label(&score, "theater-rating-score"));
        let badge: gtk::Widget = if let Some(url) = r.url.as_deref().filter(|u| metadata::safe_link(u)) {
            let link = gtk::LinkButton::with_label(url, "");
            link.set_child(Some(&content));
            link.upcast()
        } else {
            content.upcast()
        };
        badge.add_css_class("theater-rating-badge");
        badge.set_halign(gtk::Align::Start);
        badge.set_hexpand(false);
        badge.set_tooltip_text(Some(&r.label()));
        badge.update_property(&[gtk::accessible::Property::Label(&r.label())]);
        ratings.insert(&badge, -1);
    }
    ratings.set_visible(!i.metadata.ratings.is_empty());
    body.append(&ratings);
    let row = widgets::hbox(8);
    row.set_halign(gtk::Align::Start);
    let star = gtk::Image::from_icon_name("starred-symbolic");
    star.set_pixel_size(18);
    star.set_tooltip_text(Some("Your rating"));
    row.append(&star);
    let mut options = vec![("".into(), "Unrated".into())];
    options.extend((1..=20).map(|n| ((n * 50).to_string(), format!("{:.1}/10", n as f64 / 2.0))));
    let selected = metadata::personal(i).map(|n| n.to_string()).unwrap_or_default();
    let dd = widgets::dropdown(&options, &selected);
    dd.set_tooltip_text(Some("Your rating"));
    let item = i.clone();
    dd.connect_selected_notify(move |d| {
        if let Some((key, _)) = options.get(d.selected() as usize) {
            metadata::set_personal(item.clone(), key.parse().ok());
        }
    });
    row.append(&dd);
    body.append(&row);
    prose(body, &i.metadata.tagline, "theater-hero-meta");
    expand(body, "Synopsis", &i.summary);
    let mut facts = vec![];
    macro_rules! fact {
        ($label:literal,$v:expr) => {
            if let Some(s) = $v {
                facts.push(format!("{}: {}", $label, s));
            }
        };
    }
    fact!("Released", i.metadata.release_date.as_deref());
    fact!("Certification", i.metadata.certification.as_deref());
    fact!("Original language", i.metadata.original_language.as_deref());
    if !i.metadata.genres.is_empty() {
        facts.push(format!("Genres: {}", i.metadata.genres.join(", ")));
    }
    if !i.metadata.countries.is_empty() {
        facts.push(format!("Country: {}", i.metadata.countries.join(", ")));
    }
    fact!("Awards", i.metadata.awards.as_deref());
    prose(body, &facts.join(" · "), "dim");
    let links = gtk::FlowBox::new();
    links.add_css_class("theater-detail-tags");
    links.set_halign(gtk::Align::Start);
    links.set_column_spacing(8);
    links.set_row_spacing(8);
    links.set_selection_mode(gtk::SelectionMode::None);
    links.set_max_children_per_line(4);
    links.set_min_children_per_line(1);
    for (name, field) in i.metadata.genres.iter().map(|s| (s, "genres")).chain(i.metadata.studios.iter().map(|s| (s, "studios")))
    {
        let button = gtk::Button::with_label(name);
        if let Some(label) = button.child().and_downcast::<gtk::Label>() {
            label.set_max_width_chars(24);
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        }
        button.set_tooltip_text(Some(name));
        let field = field.to_owned();
        let value = name.clone();
        let tv = i.kind != MediaKind::Movie;
        button.connect_clicked(move |_| filter_library(&field, &value, tv));
        links.insert(&button, -1);
    }
    body.append(&links);
    for trailer in &i.metadata.provider_trailers {
        if trailer.versions.iter().any(|v| !v.parts.is_empty()) {
            let button = widgets::labeled_button("media-playback-start-symbolic", &format!("Trailer · {}", trailer.title));
            let item = trailer.clone();
            button.connect_clicked(move |_| library::play_trailer(&item));
            body.append(&button);
        }
    }
    for (n, url) in i.metadata.trailers.iter().filter(|u| metadata::safe_link(u)).take(2).enumerate() {
        let b = gtk::LinkButton::with_label(
            url,
            &format!("Watch trailer{}", if n == 0 { String::new() } else { format!(" {}", n + 1) }),
        );
        b.set_halign(gtk::Align::Start);
        body.append(&b);
    }
}
fn filter_library(field: &str, value: &str, tv: bool) {
    let page = if tv { "tv" } else { "movies" };
    let mut state = prefs::browse_state(page);
    match field {
        "genres" => state.filter.genres = vec![value.into()],
        "studios" => state.filter.studios = vec![value.into()],
        _ => state.filter.people = vec![value.into()],
    };
    library::set_browse(page, state);
    crate::browse_ui::sync(page);
    window::navigate(page);
}
type PhotoJob = (String, async_channel::Sender<Option<std::path::PathBuf>>);
static PHOTOS: std::sync::OnceLock<std::sync::mpsc::SyncSender<PhotoJob>> = std::sync::OnceLock::new();
fn photo(art: &Artwork, path: &str, item: Option<&MediaItem>) {
    if !metadata::public_image(path) && (!path.starts_with('/') || path.starts_with("/library/")) {
        if let Some(i) = item {
            crate::artwork::load(art, &i.id.server, Some(path), 185, 278);
        }
        return;
    }
    let jobs = PHOTOS.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::sync_channel::<PhotoJob>(64);
        let rx = std::sync::Arc::new(std::sync::Mutex::new(rx));
        for _ in 0..2 {
            let rx = rx.clone();
            std::thread::spawn(move || {
                loop {
                    let job = rx.lock().unwrap().recv();
                    let Ok((path, send)) = job else { break };
                    if !send.is_closed() {
                        let _ = send.send_blocking(metadata::image(&path));
                    }
                }
            });
        }
        tx
    });
    let (tx, rx) = async_channel::bounded(1);
    if jobs.try_send((path.into(), tx)).is_err() {
        return;
    }
    let picture = art.picture.downgrade();
    let placeholder = art.placeholder.downgrade();
    glib::spawn_future_local(async move {
        if let Ok(Some(path)) = rx.recv().await
            && let (Some(picture), Some(placeholder)) = (picture.upgrade(), placeholder.upgrade())
                && let Ok(texture) = gtk::gdk::Texture::from_file(&gtk::gio::File::for_path(path)) {
                    picture.set_paintable(Some(&texture));
                    picture.set_visible(true);
                    placeholder.set_visible(false);
                }
    });
}
pub fn people(body: &gtk::Box, i: &MediaItem) {
    if i.metadata.credits.is_empty() {
        return;
    }
    let mut seen = std::collections::HashSet::new();
    let section = widgets::vbox(12);
    section.add_css_class("theater-content-section");
    body.append(&section);
    let body = &section;
    let content = widgets::hbox(16);
    body.append(&widgets::label("CAST AND CREW", "group-title"));
    for credit in i
        .metadata
        .credits
        .iter()
        .filter(|c| c.department == "Cast")
        .chain(i.metadata.credits.iter().filter(|c| c.department != "Cast"))
        .filter(|c| seen.insert((c.name.clone(), c.role.clone())))
        .take(36)
    {
        let art = Artwork::new(112, 168, "avatar-default-symbolic");
        if let Some(path) = &credit.photo {
            photo(&art, path, Some(i));
        }
        let card = nixe_ui::media::PosterCard::new(&art.root, 112);
        card.title.set_text(&credit.name);
        card.meta.set_text(&credit.role);
        card.title.set_tooltip_text(Some(&credit.name));
        card.meta.set_tooltip_text(Some(&credit.role));
        let button = gtk::Button::new();
        button.add_css_class("poster-card");
        button.set_child(Some(&card.root));
        let c = credit.clone();
        let origin = i.clone();
        button.connect_clicked(move |_| person(c.clone(), origin.clone()));
        content.append(&button);
    }
    body.append(
        &gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .child(&content)
            .build(),
    );
}
pub fn television(body: &gtk::Box, i: &MediaItem, children: &[MediaItem]) {
    if !matches!(i.kind, MediaKind::Series | MediaKind::Season) {
        return;
    }
    let section = widgets::vbox(12);
    section.add_css_class("theater-content-section");
    body.append(&section);
    let body = &section;
    let mut facts = vec![];
    if let Some(s) = &i.metadata.status {
        facts.push(s.clone());
    }
    if let Some(n) = i.metadata.season_count {
        facts.push(format!("{n} seasons"));
    }
    if let Some(n) = i.metadata.episode_count.or(i.tv.episode_count) {
        facts.push(format!("{n} episodes"));
    }
    if let Some(date) = &i.metadata.next_air_date {
        facts.push(format!("Next airing: {date}"));
    }
    if !i.metadata.networks.is_empty() {
        facts.push(format!("Network: {}", i.metadata.networks.join(", ")));
    }
    prose(body, &facts.join(" · "), "dim");
    let mut episodes: Vec<_> = crate::catalog::snapshot()
        .episodes(&i.id)
        .into_iter()
        .chain(children.iter().filter(|e| e.kind == MediaKind::Episode).cloned())
        .map(servers::latest_watch)
        .filter(|e| !e.watch.watched)
        .collect();
    episodes.sort_by_key(|e| (e.season_number, e.episode_number));
    if let Some(next) = episodes.first() {
        let button = widgets::labeled_button(
            "media-playback-start-symbolic",
            &format!(
                "Next unwatched · S{} E{} · {}",
                next.season_number.unwrap_or(0),
                next.episode_number.unwrap_or(0),
                next.title
            ),
        );
        button.set_halign(gtk::Align::Start);
        let next = next.clone();
        button.connect_clicked(move |_| library::show_detail(next.clone()));
        body.append(&button);
    }
}
pub fn related(body: &gtk::Box, i: &MediaItem) {
    let container = widgets::vbox(0);
    body.append(&container);
    let weak = container.downgrade();
    let generation = library::detail_generation();
    let item = i.clone();
    let snapshot = crate::catalog::snapshot();
    nixe_core::cmd::background(
        move || related_rows(&item, snapshot.items().cloned().collect()),
        move |rows| {
            if library::detail_generation() != generation {
                return;
            }
            if let Some(body) = weak.upgrade() {
                for (title, items) in rows {
                    library::detail_row(&body, &title, items);
                }
            }
        },
    );
}
fn related_rows(i: &MediaItem, mut all: Vec<MediaItem>) -> Vec<(String, Vec<MediaItem>)> {
    let mut rows = vec![];
    for item in &mut all {
        metadata::apply(item);
    }
    let parent = match i.kind {
        MediaKind::Series => Some(i.id.clone()),
        MediaKind::Season | MediaKind::Episode => i.series.clone().or_else(|| i.parent.clone()),
        _ => None,
    };
    let current = parent.as_ref().and_then(|id| all.iter().find(|s| &s.id == id)).unwrap_or(i);
    let groups = nixe_media_model::index::unify(all.clone());
    let excluded: std::collections::HashSet<_> = groups
        .iter()
        .filter(|g| g.sources.iter().any(|s| s.id == current.id || s.id == i.id))
        .flat_map(|g| g.sources.iter().map(|s| s.id.clone()))
        .collect();
    let credits = i.metadata.credits.iter().chain(current.metadata.credits.iter()).collect::<Vec<_>>();
    let mut cast_candidates: std::collections::HashMap<ItemId, std::collections::HashSet<String>> = Default::default();
    for entry in &all {
        if !entry.metadata.credits.iter().any(|c| credits.iter().any(|p| p.name.eq_ignore_ascii_case(&c.name))) {
            continue;
        }
        match entry.kind {
            MediaKind::Movie | MediaKind::Series => {
                cast_candidates
                    .entry(entry.id.clone())
                    .or_default()
                    .extend(entry.metadata.credits.iter().map(|c| c.name.to_lowercase()));
            }
            MediaKind::Season | MediaKind::Episode => {
                if let Some(parent) = &entry.series {
                    cast_candidates
                        .entry(parent.clone())
                        .or_default()
                        .extend(entry.metadata.credits.iter().map(|c| c.name.to_lowercase()));
                }
            }
            _ => {}
        }
    }
    let mut more_candidates = vec![];
    let mut related_titles = vec![];
    for group in groups {
        if group.sources.iter().any(|s| excluded.contains(&s.id)) {
            continue;
        }
        if let Some(entry) = group
            .sources
            .iter()
            .find(|s| matches!(s.kind, MediaKind::Movie | MediaKind::Series) && cast_candidates.contains_key(&s.id))
        {
            more_candidates.push(entry.clone());
        }
        if let Some(entry) =
            group.sources.into_iter().find(|s| s.kind == current.kind && matches!(s.kind, MediaKind::Movie | MediaKind::Series))
        {
            related_titles.push(entry);
        }
    }
    let all = related_titles;
    let mut recommendations: Vec<_> = all
        .iter()
        .filter(|s| {
            i.metadata.recommendations.iter().any(|r| {
                s.external_ids.tmdb.as_deref() == Some(r.tmdb_id.to_string().as_str())
                    && r.series == (s.kind == MediaKind::Series)
            })
        })
        .cloned()
        .collect();
    if recommendations.is_empty() {
        recommendations =
            all.iter().filter(|s| s.metadata.genres.iter().any(|g| i.metadata.genres.contains(g))).take(18).cloned().collect();
    }
    rows.push(("More like this · in your library".into(), recommendations));
    if let Some(name) = i.metadata.collection.as_ref() {
        let mut collection: Vec<_> = all
            .iter()
            .filter(|s| s.metadata.collection.as_ref() == Some(name) || s.metadata.collections.contains(name))
            .cloned()
            .collect();
        collection.push(i.clone());
        collection.sort_by_key(|s| (s.metadata.release_date.clone(), s.year, s.title.clone()));
        rows.push((name.clone(), collection));
    }
    for credit in
        [credits.iter().copied().find(|c| c.department == "Cast"), credits.iter().copied().find(|c| c.department == "Directing")]
            .into_iter()
            .flatten()
    {
        let titles = more_candidates
            .iter()
            .filter(|s| cast_candidates.get(&s.id).is_some_and(|names| names.contains(&credit.name.to_lowercase())))
            .take(18)
            .cloned()
            .collect();
        rows.push((format!("More with {}", credit.name), titles));
    }
    rows
}
pub fn media(body: &gtk::Box, sources: &[MediaItem]) {
    let mut lines = vec![];
    for source in sources {
        for v in &source.versions {
            let t = &v.technical;
            let parts = [
                Some(format!("{:?}", source.id.server.provider)),
                Some(source.title.clone()),
                Some(v.edition.clone().unwrap_or_else(|| "Original".into())),
                v.height.map(|n| format!("{n}p")),
                t.hdr.clone(),
                v.video_codec.clone(),
                v.container.clone(),
                (!t.audio_codecs.is_empty()).then(|| format!("Audio: {}", t.audio_codecs.join(", "))),
                (!t.audio_languages.is_empty()).then(|| format!("Languages: {}", t.audio_languages.join(", "))),
                (!t.subtitle_languages.is_empty()).then(|| format!("Subtitles: {}", t.subtitle_languages.join(", "))),
            ];
            lines.push(parts.into_iter().flatten().collect::<Vec<_>>().join(" · "));
        }
    }
    expand(body, "Media details", &lines.join("\n\n"));
}
fn person(credit: Credit, origin: MediaItem) {
    let Some((body, generation)) = library::begin_person() else { return };
    prose(&body, &credit.name, "theater-hero-title");
    let art = Artwork::new(150, 225, "avatar-default-symbolic");
    if let Some(path) = &credit.photo {
        photo(&art, path, Some(&origin));
    }
    body.append(&art.root);
    let owned = widgets::vbox(8);
    body.append(&owned);
    let info = widgets::vbox(8);
    body.append(&info);
    let name = credit.name.clone();
    let snapshot = crate::catalog::snapshot();
    nixe_core::cmd::background(
        move || {
            let mut items: Vec<_> = snapshot.items().cloned().collect();
            for i in &mut items {
                metadata::apply(i);
            }
            let mut owned: Vec<_> = nixe_media_model::index::unify(items)
                .into_iter()
                .filter_map(|g| g.sources.into_iter().next())
                .filter(|i| {
                    matches!(i.kind, MediaKind::Movie | MediaKind::Series)
                        && i.metadata.credits.iter().any(|c| c.name.eq_ignore_ascii_case(&name))
                })
                .collect();
            owned.sort_by_key(|i| std::cmp::Reverse(i.year));
            owned
        },
        move |items| {
            if library::detail_generation() == generation {
                library::detail_row(&owned, "In your library", items);
            }
        },
    );
    let Some(id) = credit.tmdb_id else {
        prose(&info, "Biography and full filmography become available when TMDB credits are connected.", "dim");
        return;
    };
    let config = prefs::metadata_config();
    let weak = info.downgrade();
    let snapshot = crate::catalog::snapshot();
    nixe_core::cmd::background(
        move || {
            let mut person = metadata::person(id, config)?;
            let mut seen = std::collections::HashSet::new();
            person.titles.retain(|t| seen.insert((t.tmdb_id, t.series)));
            person.titles.sort_by_key(|t| std::cmp::Reverse(t.year));
            let lookup: std::collections::HashMap<_, _> = snapshot
                .items()
                .filter(|i| matches!(i.kind, MediaKind::Movie | MediaKind::Series))
                .cloned()
                .filter_map(|i| Some(((i.kind == MediaKind::Series, i.external_ids.tmdb.clone()?.parse::<u64>().ok()?), i)))
                .collect();
            let films = std::mem::take(&mut person.titles)
                .into_iter()
                .map(|t| {
                    let owned = lookup.get(&(t.series, t.tmdb_id)).cloned();
                    (t, owned)
                })
                .collect::<Vec<_>>();
            Ok::<_, ProviderError>((person, films))
        },
        move |result| {
            if library::detail_generation() != generation {
                return;
            }
            let Some(info) = weak.upgrade() else { return };
            match result {
                Ok((person, films)) => {
                    let dates = [person.birthday, person.birthplace].into_iter().flatten().collect::<Vec<_>>().join(" · ");
                    prose(&info, &dates, "dim");
                    expand(&info, "Biography", &person.biography);
                    if let Some(path) = person.photo {
                        photo(&art, &path, None);
                    }
                    info.append(&widgets::label("FILMOGRAPHY", "group-title"));
                    filmography(&info, std::rc::Rc::new(films), 0);
                }
                Err(e) => prose(&info, &e.to_string(), "dim"),
            }
        },
    );
}

fn filmography(body: &gtk::Box, titles: std::rc::Rc<Vec<(RelatedTitle, Option<MediaItem>)>>, start: usize) {
    let end = (start + 50).min(titles.len());
    for (title, owned) in &titles[start..end] {
        let label = format!(
            "{}{} · {}{}",
            title.title,
            title.year.map(|y| format!(" ({y})")).unwrap_or_default(),
            title.character,
            if owned.is_some() { " · In your library" } else { "" }
        );
        if let Some(item) = owned {
            let b = gtk::Button::with_label(&label);
            if let Some(label) = b.child().and_downcast::<gtk::Label>() {
                label.set_wrap(true);
            }
            let item = item.clone();
            b.connect_clicked(move |_| library::show_detail(item.clone()));
            body.append(&b);
        } else {
            prose(body, &label, "dim");
        }
    }
    if end < titles.len() {
        let more = nixe_ui::widgets::icon_button("view-more-horizontal-symbolic", &format!("Show {} more", titles.len() - end));
        let weak = body.downgrade();
        more.connect_clicked(move |button| {
            if let Some(body) = weak.upgrade() {
                body.remove(button);
                filmography(&body, titles.clone(), end);
            }
        });
        body.append(&more);
    }
}

#[cfg(test)]
mod related_tests {
    use super::*;
    fn item(id: &str, kind: &str, tmdb: &str, parent: Option<&str>, actor: bool) -> MediaItem {
        let server = ServerId { provider: ProviderKind::Plex, id: "fixture".into() };
        let mut value = serde_json::json!({"ratingKey":id,"type":kind,"title":id,"Guid":[{"id":format!("tmdb://{tmdb}")} ]});
        if let Some(parent) = parent {
            value["grandparentRatingKey"] = parent.into();
            value["parentIndex"] = 1.into();
            value["index"] = 1.into();
        }
        if actor {
            value["Role"] = serde_json::json!([{"tag":"Actor","role":"Character"}]);
        }
        nixe_plex::parse_item(&value, &server, "1").unwrap()
    }
    #[test]
    fn more_with_promotes_episodes_and_excludes_every_copy_of_current_series() {
        let show = item("1", "show", "100", None, true);
        let episode = item("2", "episode", "200", Some("1"), true);
        let mut duplicate = show.clone();
        duplicate.id.server.id = "another-server".into();
        duplicate.id.id = "duplicate".into();
        let other = item("3", "show", "300", None, false);
        let guest = item("4", "episode", "400", Some("3"), true);
        let movie = item("5", "movie", "500", None, true);
        let rows = related_rows(&episode, vec![show.clone(), episode.clone(), duplicate, other.clone(), guest, movie.clone()]);
        let titles = &rows.iter().find(|(name, _)| name == "More with Actor").unwrap().1;
        assert!(titles.iter().all(|i| matches!(i.kind, MediaKind::Movie | MediaKind::Series)));
        assert!(titles.iter().any(|i| i.id == other.id));
        assert!(titles.iter().any(|i| i.id == movie.id));
        assert!(!titles.iter().any(|i| i.external_ids.tmdb == show.external_ids.tmdb));
    }
}

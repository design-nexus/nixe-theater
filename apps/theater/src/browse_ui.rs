use crate::{
    browse::{self, Filter, SavedView, State},
    prefs, widgets, window,
};
use gtk::prelude::*;
use nixe_media_model::ProviderError;
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeSet, HashMap},
    rc::Rc,
};
struct Controls {
    sort: gtk::DropDown,
    reverse: gtk::ToggleButton,
    chips: gtk::FlowBox,
    count: gtk::Label,
    options: Vec<(String, String)>,
}
thread_local! {static CONTROLS:RefCell<HashMap<String,Controls>>=RefCell::new(HashMap::new());static UPDATING:Cell<bool>=const{Cell::new(false)};}
fn update(page: &str, change: impl FnOnce(&mut State)) {
    let mut state = prefs::browse_state(page);
    change(&mut state);
    crate::library::set_browse(page, state);
    sync(page);
}
pub fn controls(page: &'static str, toolbar: &gtk::Box) -> gtk::FlowBox {
    let options = browse::sorts(page == "tv");
    let state = prefs::browse_state(page);
    let sort = widgets::dropdown(&options, &state.sort);
    sort.set_tooltip_text(Some("Sort library"));
    crate::library::fit_dropdown(&sort);
    let opts = options.clone();
    sort.connect_selected_notify(move |d| {
        if UPDATING.with(|u| u.get()) {
            return;
        }
        if let Some((key, _)) = opts.get(d.selected() as usize) {
            update(page, |s| s.sort = key.clone());
        }
    });
    toolbar.append(&sort);
    let reverse = gtk::ToggleButton::new();
    reverse.set_icon_name("view-sort-descending-symbolic");
    reverse.set_tooltip_text(Some("Descending order"));
    reverse.set_active(state.descending);
    reverse.connect_toggled(move |b| {
        if !UPDATING.with(|u| u.get()) {
            update(page, |s| s.descending = b.is_active());
        }
    });
    toolbar.append(&reverse);
    toolbar.append(&nixe_ui::chrome::status_action("media-playlist-shuffle-symbolic", "Reshuffle", move || {
        update(page, |s| {
            s.shuffle = s.shuffle.wrapping_add(1);
            s.sort = "shuffle".into();
        })
    }));
    let button = widgets::labeled_button("theater-filter-symbolic", "Filter");
    button.connect_clicked(move |_| open(page));
    toolbar.append(&button);
    toolbar.append(&nixe_ui::chrome::status_action("starred-symbolic", "Saved views", move || saved(page)));
    let chips = gtk::FlowBox::new();
    chips.set_selection_mode(gtk::SelectionMode::None);
    chips.set_column_spacing(6);
    chips.set_row_spacing(4);
    chips.set_min_children_per_line(1);
    chips.set_max_children_per_line(10);
    chips.set_margin_start(16);
    chips.set_margin_end(16);
    let count = widgets::label("", "dim");
    count.set_tooltip_text(Some("Matching titles · ratings coverage"));
    CONTROLS.with(|c| {
        c.borrow_mut().insert(page.into(), Controls { sort, reverse, chips: chips.clone(), count, options });
    });
    sync(page);
    chips
}
pub fn count(page: &str, total: usize, rated: usize) {
    CONTROLS.with(|c| {
        if let Some(c) = c.borrow().get(page) {
            let status = crate::metadata::status();
            let sort = prefs::browse_state(page).sort;
            let source = match sort.as_str() {
                "imdb" => "IMDb",
                "tmdb" => "TMDB",
                "rt_critic" => "RT critics",
                "rt_audience" => "RT audience",
                "metacritic" => "Metacritic",
                "critic" => "Critic",
                "audience" => "Audience",
                _ => "",
            };
            c.count.set_text(&if source.is_empty() {
                format!("{total} titles · {rated} with ratings")
            } else {
                format!("{total} titles · {source}: {rated} rated")
            });
            c.count.set_tooltip_text(Some(if status.is_empty() { "Cached metadata · background updates" } else { &status }));
        }
    });
}
pub fn sync(page: &str) {
    let state = prefs::browse_state(page);
    UPDATING.with(|u| u.set(true));
    CONTROLS.with(|c| {
        if let Some(c) = c.borrow().get(page) {
            c.sort.set_selected(c.options.iter().position(|(k, _)| k == &state.sort).unwrap_or(0) as u32);
            c.reverse.set_active(state.descending);
            while let Some(child) = c.chips.first_child() {
                c.chips.remove(&child);
            }
            c.chips.insert(&c.count, -1);
            for (key, label) in browse::chips(&state.filter) {
                let button = gtk::Button::with_label(&format!("{label} ×"));
                let p = page.to_owned();
                button.set_tooltip_text(Some("Remove filter"));
                button.connect_clicked(move |_| update(&p, |s| browse::clear(&mut s.filter, key)));
                c.chips.insert(&button, -1);
            }
            if state.filter != Filter::default() {
                let clear = nixe_ui::widgets::icon_button("edit-clear-all-symbolic", "Clear all filters");
                let p = page.to_owned();
                clear.connect_clicked(move |_| update(&p, |s| s.filter = Filter::default()));
                c.chips.insert(&clear, -1);
            }
        }
    });
    UPDATING.with(|u| u.set(false));
}
fn values(items: &[nixe_media_model::MediaItem], field: &str) -> Vec<String> {
    let mut set = BTreeSet::new();
    for i in items {
        let m = &i.metadata;
        let v: Vec<String> = match field {
            "genres" => m.genres.clone(),
            "people" => m.credits.iter().map(|c| c.name.clone()).collect(),
            "studios" => m.studios.clone(),
            "collections" => m.collections.iter().cloned().chain(m.collection.clone()).collect(),
            "certifications" => m.certification.clone().into_iter().collect(),
            "status" => m.status.clone().into_iter().collect(),
            "audio" => i.versions.iter().flat_map(|v| v.technical.audio_languages.clone()).collect(),
            "subtitles" => i.versions.iter().flat_map(|v| v.technical.subtitle_languages.clone()).collect(),
            "editions" => i.versions.iter().map(|v| v.edition.clone().unwrap_or_else(|| "Original".into())).collect(),
            _ => vec![],
        };
        set.extend(v.into_iter().filter(|s| !s.is_empty()));
    }
    set.into_iter().collect()
}
#[derive(Clone)]
struct Facet {
    selected: Rc<RefCell<Vec<String>>>,
}
impl Facet {
    fn values(&self) -> Vec<String> {
        self.selected.borrow().clone()
    }
}
fn field(body: &gtk::Box, label: &str, control: &impl IsA<gtk::Widget>) {
    let row = widgets::hbox(16);
    row.add_css_class("theater-filter-row");
    let title = widgets::label(label, "theater-filter-label");
    title.set_width_chars(14);
    row.append(&title);
    control.set_hexpand(true);
    row.append(control);
    body.append(&row);
}
fn selection_label(label: &gtk::Label, selected: &[String]) {
    label.set_text(&match selected {
        [] => "All".into(),
        [one] => one.clone(),
        _ => format!("{} selected", selected.len()),
    });
    label.set_tooltip_text((!selected.is_empty()).then(|| selected.join(", ")).as_deref());
}
fn facet(body: &gtk::Box, label: &str, mut options: Vec<String>, current: &[String]) -> Facet {
    options.extend(current.iter().cloned());
    options.sort();
    options.dedup();
    let selected = Rc::new(RefCell::new(current.to_vec()));
    let choices = Rc::new(RefCell::new(options));
    let menu = gtk::MenuButton::new();
    menu.add_css_class("theater-filter-picker");
    let caption = widgets::label("", "theater-filter-value");
    caption.set_hexpand(true);
    caption.set_max_width_chars(24);
    caption.set_ellipsize(gtk::pango::EllipsizeMode::End);
    selection_label(&caption, current);
    let content = widgets::hbox(8);
    content.append(&caption);
    content.append(&gtk::Image::from_icon_name("pan-down-symbolic"));
    menu.set_child(Some(&content));
    menu.set_tooltip_text(Some(label));
    let pop = gtk::Popover::new();
    pop.add_css_class("theater-filter-popover");
    let list = widgets::vbox(8);
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Search"));
    search.set_visible(choices.borrow().len() > 10 || label == "Playlist");
    list.append(&search);
    let checks = widgets::vbox(4);
    {
        let checks = checks.clone();
        let selected = selected.clone();
        let choices = choices.clone();
        let caption = caption.clone();
        search.connect_search_changed(move |s| populate(&checks, &caption, &selected, &choices.borrow(), &s.text()));
    }
    {
        let checks = checks.clone();
        let selected = selected.clone();
        let choices = choices.clone();
        let caption = caption.clone();
        let search = search.clone();
        pop.connect_map(move |_| populate(&checks, &caption, &selected, &choices.borrow(), &search.text()));
    }
    list.append(
        &gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::External)
            .min_content_height(60)
            .max_content_height(260)
            .propagate_natural_height(true)
            .min_content_width(260)
            .child(&checks)
            .build(),
    );
    let clear = gtk::Button::with_label("Clear selection");
    {
        let selected = selected.clone();
        let caption = caption.clone();
        let choices = choices.clone();
        let checks = checks.clone();
        let search = search.clone();
        clear.connect_clicked(move |_| {
            selected.borrow_mut().clear();
            selection_label(&caption, &[]);
            populate(&checks, &caption, &selected, &choices.borrow(), &search.text());
        });
    }
    list.append(&clear);
    if label == "Playlist" {
        let loaded = Cell::new(false);
        let choices = choices.clone();
        let selected = selected.clone();
        let caption = caption.clone();
        let checks = checks.clone();
        let search = search.clone();
        pop.connect_map(move |_| {
            if loaded.replace(true) {
                return;
            }
            let clients = crate::servers::clients();
            let choices = choices.clone();
            let selected = selected.clone();
            let caption = caption.downgrade();
            let checks = checks.downgrade();
            let search = search.clone();
            nixe_core::cmd::background(
                move || {
                    let mut names = BTreeSet::new();
                    for client in clients {
                        let mut cursor = None;
                        while let Ok(page) = client.playlists(cursor.as_deref()) {
                            names.extend(page.items.into_iter().map(|l| l.title));
                            cursor = page.next;
                            if cursor.is_none() {
                                break;
                            }
                        }
                    }
                    names
                },
                move |names| {
                    let (Some(checks), Some(caption)) = (checks.upgrade(), caption.upgrade()) else { return };
                    let mut choices = choices.borrow_mut();
                    choices.extend(names);
                    choices.sort();
                    choices.dedup();
                    populate(&checks, &caption, &selected, &choices, &search.text());
                },
            );
        });
    }
    pop.set_child(Some(&list));
    menu.set_popover(Some(&pop));
    field(body, label, &menu);
    Facet { selected }
}
fn populate(checks: &gtk::Box, caption: &gtk::Label, selected: &Rc<RefCell<Vec<String>>>, choices: &[String], query: &str) {
    while let Some(c) = checks.first_child() {
        checks.remove(&c);
    }
    let query = query.to_lowercase();
    let matching: Vec<_> = choices.iter().filter(|v| v.to_lowercase().contains(&query)).take(101).collect();
    for value in matching.iter().take(100) {
        let check = gtk::CheckButton::with_label(value);
        check.set_active(selected.borrow().contains(value));
        let selected = selected.clone();
        let caption = caption.clone();
        let value = (*value).clone();
        check.connect_toggled(move |c| {
            let mut values = selected.borrow_mut();
            values.retain(|v| v != &value);
            if c.is_active() {
                values.push(value.clone());
            }
            selection_label(&caption, &values);
        });
        checks.append(&check);
    }
    if matching.len() > 100 {
        checks.append(&widgets::label("Search to narrow results", "dim"));
    } else if matching.is_empty() {
        checks.append(&widgets::label("No matching options", "dim"));
    }
}
#[derive(Clone)]
struct Preset {
    dropdown: gtk::DropDown,
    ranges: Vec<(Option<u32>, Option<u32>)>,
}
impl Preset {
    fn value(&self) -> (Option<u32>, Option<u32>) {
        self.ranges.get(self.dropdown.selected() as usize).copied().unwrap_or_default()
    }
}
fn preset(
    body: &gtk::Box,
    label: &str,
    mut options: Vec<(String, Option<u32>, Option<u32>)>,
    current: (Option<u32>, Option<u32>),
) -> Preset {
    if !options.iter().any(|(_, a, b)| (*a, *b) == current) {
        options.push((
            match current {
                (Some(a), Some(b)) if a == b => a.to_string(),
                (Some(a), Some(b)) => format!("{a}–{b}"),
                (Some(a), None) => format!("{a}+"),
                (None, Some(b)) => format!("Up to {b}"),
                _ => "All".into(),
            },
            current.0,
            current.1,
        ));
    }
    let selected = options.iter().position(|(_, a, b)| (*a, *b) == current).unwrap_or(0);
    let choices: Vec<_> = options.iter().enumerate().map(|(n, (label, _, _))| (n.to_string(), label.clone())).collect();
    let dropdown = widgets::dropdown(&choices, &selected.to_string());
    crate::library::fit_dropdown(&dropdown);
    field(body, label, &dropdown);
    Preset { dropdown, ranges: options.into_iter().map(|(_, a, b)| (a, b)).collect() }
}
fn choices(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).into()).collect()
}
pub fn open(page: &str) {
    let page = page.to_owned();
    let snapshot = crate::catalog::snapshot();
    nixe_core::cmd::background(
        move || {
            let mut items: Vec<_> = snapshot.items().cloned().collect();
            for i in &mut items {
                crate::metadata::apply(i);
            }
            let lists = crate::playlist_store::Store::open().and_then(|s| s.list()).unwrap_or_default();
            let names: BTreeSet<_> = lists.iter().map(|l| l.title.clone()).collect();
            let fields =
                ["genres", "people", "studios", "collections", "certifications", "status", "audio", "subtitles", "editions"];
            let mut facets = fields.into_iter().map(|field| (field, values(&items, field))).collect::<HashMap<_, _>>();
            let years: BTreeSet<_> = items.iter().filter_map(|i| i.year).map(|y| y.to_string()).collect();
            facets.insert("years", years.into_iter().collect());
            (facets, names.into_iter().collect())
        },
        move |(facets, playlists)| panel(&page, facets, playlists),
    );
}
fn panel(page: &str, facets: HashMap<&'static str, Vec<String>>, playlists: Vec<String>) {
    let state = prefs::browse_state(page);
    let f = &state.filter;
    let (dialog, body) = widgets::dialog("Library filters", 600);
    body.add_css_class("theater-filter-panel");
    let stack = gtk::Stack::new();
    stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    stack.set_transition_duration(if prefs::get().reduce_motion { 0 } else { 160 });
    stack.set_vhomogeneous(false);
    let weak = stack.downgrade();
    body.append(&widgets::segmented(
        &widgets::opts(&[("library", "Library"), ("media", "Media"), ("ratings", "Ratings"), ("people", "People")]),
        "library",
        move |key| {
            if let Some(stack) = weak.upgrade() {
                stack.set_visible_child_name(&key);
            }
        },
    ));
    let mut groups = HashMap::new();
    for id in ["library", "media", "ratings", "people"] {
        let content = widgets::vbox(4);
        content.set_margin_top(12);
        content.set_margin_bottom(12);
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::External)
            .min_content_height(360)
            .max_content_height(420)
            .propagate_natural_height(true)
            .child(&content)
            .build();
        stack.add_named(&scroll, Some(id));
        groups.insert(id, content);
    }
    body.append(&stack);
    let get = |name| facets.get(name).cloned().unwrap_or_default();
    let mut inputs: HashMap<&str, Facet> = HashMap::new();
    for (group, key, label, options, selected) in [
        ("library", "watch", "Watch state", choices(&["Unwatched", "In progress", "Watched"]), &f.watch),
        ("library", "genres", "Genres", get("genres"), &f.genres),
        ("library", "certifications", "Certification", get("certifications"), &f.certifications),
        ("library", "collections", "Collection", get("collections"), &f.collections),
        ("library", "playlists", "Playlist", playlists, &f.playlists),
        ("library", "availability", "Availability", choices(&["Online", "Offline"]), &f.availability),
        ("media", "quality", "Resolution", choices(&["SD", "720p", "1080p", "4K"]), &f.quality),
        ("media", "hdr", "Dynamic range", choices(&["SDR", "HDR", "Dolby Vision"]), &f.hdr),
        ("media", "audio", "Audio language", get("audio"), &f.audio),
        ("media", "subtitles", "Subtitles", get("subtitles"), &f.subtitles),
        ("media", "editions", "Edition", get("editions"), &f.editions),
        ("people", "people", "Cast and crew", get("people"), &f.people),
        ("people", "studios", "Studio", get("studios"), &f.studios),
    ] {
        inputs.insert(key, facet(&groups[group], label, options, selected));
    }
    let tv = page == "tv" || page == "search";
    if tv {
        inputs.insert("status", facet(&groups["library"], "TV status", get("status"), &f.status));
    }
    let mut years = vec![("All years".into(), None, None)];
    let mut decades = BTreeSet::new();
    let available: Vec<u32> = get("years").iter().filter_map(|s| s.parse().ok()).collect();
    for year in &available {
        decades.insert(year / 10 * 10);
    }
    for decade in decades.into_iter().rev() {
        years.push((format!("{decade}s"), Some(decade), Some(decade + 9)));
    }
    for year in available.into_iter().rev() {
        years.push((year.to_string(), Some(year), Some(year)));
    }
    let year = preset(&groups["library"], "Release year", years, (f.year_min, f.year_max));
    let runtime = preset(
        &groups["library"],
        "Runtime",
        vec![
            ("Any length".into(), None, None),
            ("Under 90 minutes".into(), None, Some(89)),
            ("90–120 minutes".into(), Some(90), Some(120)),
            ("Over 120 minutes".into(), Some(121), None),
        ],
        (f.runtime_min, f.runtime_max),
    );
    let unwatched = gtk::CheckButton::with_label("Has unwatched episodes");
    unwatched.set_active(f.unwatched);
    unwatched.set_visible(tv);
    unwatched.set_margin_top(8);
    groups["library"].append(&unwatched);
    let opts: Vec<_> = browse::sorts(false)
        .into_iter()
        .filter(|(k, _)| {
            matches!(
                k.as_str(),
                "imdb" | "tmdb" | "rt_critic" | "rt_audience" | "metacritic" | "personal" | "critic" | "audience"
            )
        })
        .collect();
    let source = widgets::dropdown(&opts, if f.rating_source.is_empty() { "imdb" } else { &f.rating_source });
    crate::library::fit_dropdown(&source);
    field(&groups["ratings"], "Source", &source);
    let mut ratings = vec![("Any rating".into(), None, None)];
    for n in [50, 60, 65, 70, 75, 80, 85, 90] {
        ratings.push((format!("{:.1}+ / 10", n as f64 / 10.0), Some(n), None));
    }
    let rating = preset(&groups["ratings"], "Minimum rating", ratings, (f.rating_min, None));
    let mut vote_options = vec![("Any vote count".into(), None, None)];
    for n in [100, 1000, 10000, 50000, 100000, 500000] {
        vote_options.push((format!("{n}+"), Some(n), None));
    }
    let votes = preset(&groups["ratings"], "Minimum votes", vote_options, (f.votes_min.and_then(|v| v.try_into().ok()), None));
    let hint = widgets::label("Rating thresholds use a 0–10 scale.", "dim");
    hint.set_margin_top(12);
    hint.set_wrap(true);
    groups["ratings"].append(&hint);
    let actions = widgets::hbox(8);
    actions.add_css_class("theater-filter-footer");
    let clear = gtk::Button::with_label("Clear all");
    let p = page.to_owned();
    let weak = dialog.downgrade();
    clear.connect_clicked(move |_| {
        update(&p, |s| s.filter = Filter::default());
        if let Some(d) = weak.upgrade() {
            d.close();
        }
    });
    actions.append(&clear);
    let spacer = widgets::hbox(0);
    spacer.set_hexpand(true);
    actions.append(&spacer);
    let cancel = gtk::Button::with_label("Cancel");
    let weak = dialog.downgrade();
    cancel.connect_clicked(move |_| {
        if let Some(d) = weak.upgrade() {
            d.close();
        }
    });
    actions.append(&cancel);
    let apply = gtk::Button::with_label("Apply");
    apply.add_css_class("suggested-action");
    let p = page.to_owned();
    let weak = dialog.downgrade();
    apply.connect_clicked(move|_|{
        let(a,b)=year.value();let(c,d)=runtime.value();let mut f=Filter{year_min:a,year_max:b,runtime_min:c,runtime_max:d,rating_min:rating.value().0,votes_min:votes.value().0.map(u64::from),rating_source:if rating.value().0.is_some()||votes.value().0.is_some(){opts.get(source.selected()as usize).map(|v|v.0.clone()).unwrap_or_default()}else{String::new()},unwatched:tv&&unwatched.is_active(),..Default::default()};
        macro_rules! fields{($($field:ident),*)=>{$(f.$field=inputs.get(stringify!($field)).map(Facet::values).unwrap_or_default();)*}}fields!(watch,genres,certifications,quality,hdr,audio,subtitles,editions,availability,status,people,studios,collections,playlists);
        let mut s=prefs::browse_state(&p);s.filter=f;apply_state(p.clone(),s);if let Some(d)=weak.upgrade(){d.close();}
    });
    actions.append(&apply);
    body.append(&actions);
    dialog.present();
}

pub fn apply_state(page: String, mut state: State) {
    if state.filter.playlists.is_empty() {
        crate::library::set_browse(&page, state);
        sync(&page);
        return;
    }
    window::status("Loading playlist membership…");
    let clients = crate::servers::clients();
    nixe_core::cmd::background(
        move || {
            let selected = &state.filter.playlists;
            let mut members = vec![];
            for list in crate::playlist_store::Store::open()?
                .list()?
                .into_iter()
                .filter(|l| l.origin.is_none() && selected.iter().any(|s| s.eq_ignore_ascii_case(&l.title)))
            {
                members.extend(list.entries.into_iter().map(|e| e.media.id));
            }
            for client in clients {
                let mut cursor = None;
                loop {
                    let p = client.playlists(cursor.as_deref())?;
                    for list in p.items.into_iter().filter(|l| selected.iter().any(|s| s.eq_ignore_ascii_case(&l.title))) {
                        let mut next = None;
                        loop {
                            let p = client.playlist_items(&list.id, next.as_deref())?;
                            members.extend(p.items.into_iter().map(|i| i.id));
                            next = p.next;
                            if next.is_none() {
                                break;
                            }
                        }
                    }
                    cursor = p.next;
                    if cursor.is_none() {
                        break;
                    }
                }
            }
            state.filter.playlist_members = members;
            Ok::<_, ProviderError>((page, state))
        },
        |result| match result {
            Ok((page, state)) => {
                crate::library::set_browse(&page, state);
                sync(&page);
                window::status("Filters applied");
            }
            Err(e) => window::status(&e.to_string()),
        },
    );
}
fn saved(page: &str) {
    let page = page.to_owned();
    nixe_core::cmd::background(crate::metadata::saved_views, move |result| {
        let (dialog, body) = widgets::dialog("Saved library views", 480);
        let name = gtk::Entry::new();
        name.set_placeholder_text(Some("Name this view"));
        name.set_max_length(100);
        body.append(&name);
        let save = gtk::Button::with_label("Save current view");
        let p = page.clone();
        let weak = dialog.downgrade();
        save.connect_clicked(move |_| {
            let name = name.text().trim().to_owned();
            if name.is_empty() {
                return;
            }
            let view = SavedView { name, page: p.clone(), state: prefs::browse_state(&p) };
            nixe_core::cmd::background(
                move || crate::metadata::save_view(&view),
                |r| {
                    if let Err(e) = r {
                        window::status(&e.to_string())
                    }
                },
            );
            if let Some(d) = weak.upgrade() {
                d.close();
            }
        });
        body.append(&save);
        match result {
            Ok(views) => {
                for view in views {
                    let row = widgets::hbox(8);
                    let load = gtk::Button::with_label(&format!("{} · {}", view.name, view.page));
                    load.set_hexpand(true);
                    let v = view.clone();
                    let weak = dialog.downgrade();
                    load.connect_clicked(move |_| {
                        apply_state(v.page.clone(), v.state.clone());
                        window::navigate(&v.page);
                        if let Some(d) = weak.upgrade() {
                            d.close();
                        }
                    });
                    row.append(&load);
                    let delete = gtk::Button::from_icon_name("edit-delete-symbolic");
                    delete.set_tooltip_text(Some("Delete saved view"));
                    let weak = row.downgrade();
                    delete.connect_clicked(move |_| {
                        let name = view.name.clone();
                        let weak = weak.clone();
                        nixe_core::cmd::background(
                            move || crate::metadata::delete_view(&name),
                            move |r| match r {
                                Ok(()) => {
                                    if let Some(row) = weak.upgrade() {
                                        row.set_visible(false)
                                    }
                                }
                                Err(e) => window::status(&e.to_string()),
                            },
                        );
                    });
                    row.append(&delete);
                    body.append(&row);
                }
            }
            Err(e) => body.append(&widgets::label(&e.to_string(), "dim")),
        }
        dialog.present();
    });
}

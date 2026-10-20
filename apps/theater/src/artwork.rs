//! Bounded artwork workers and a disk budget; GTK only receives scaled image bytes.
use crate::servers::Client;
use gtk::{gdk, glib, prelude::*};
use nixe_media_model::{ItemId, MediaItem, MediaKind, ProviderKind, ServerId};
use nixe_ui::media::Artwork;
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    collections::HashMap,
    path::PathBuf,
    rc::{Rc, Weak},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Instant, SystemTime},
};
static CANCELLED: AtomicBool = AtomicBool::new(false);
struct Reply {
    tx: async_channel::Sender<Option<Arc<Vec<u8>>>>,
    active: Arc<AtomicBool>,
}
static PENDING: OnceLock<Mutex<HashMap<PathBuf, Vec<Reply>>>> = OnceLock::new();
thread_local! { static LOADERS: RefCell<Vec<Weak<dyn Fn()>>> = const { RefCell::new(Vec::new()) }; }
pub fn cancel() {
    CANCELLED.store(true, Ordering::Relaxed);
    if let Some(pending) = PENDING.get() {
        for (_, replies) in pending.lock().unwrap().drain() {
            for reply in replies {
                reply.active.store(false, Ordering::Relaxed);
                let _ = reply.tx.try_send(None);
            }
        }
    }
}
pub fn retry_visible() {
    LOADERS.with(|loaders| {
        loaders.borrow_mut().retain(|load| {
            if let Some(load) = load.upgrade() {
                load();
                true
            } else {
                false
            }
        });
    });
}
#[derive(Clone)]
struct Job {
    client: Option<Client>,
    server: ServerId,
    key: String,
    fallback: Vec<String>,
    width: u32,
    height: u32,
    group: Option<Group>,
}
#[derive(Clone)]
struct Group {
    id: Option<ItemId>,
    playlist: bool,
    cover: Option<String>,
    members: Vec<Job>,
}
static WORKERS: OnceLock<async_channel::Sender<Job>> = OnceLock::new();
static NETWORK_QUEUE: OnceLock<async_channel::Receiver<Job>> = OnceLock::new();
fn network_workers() -> &'static async_channel::Sender<Job> {
    WORKERS.get_or_init(|| {
        let (tx, rx) = async_channel::bounded::<Job>(128);
        let _ = NETWORK_QUEUE.set(rx.clone());
        for _ in 0..3 {
            let rx = rx.clone();
            std::thread::spawn(move || {
                loop {
                    let Ok(job) = rx.recv_blocking() else { break };
                    let path = path(&job);
                    let wanted = PENDING.get().and_then(|pending| pending.lock().ok()).is_some_and(|pending| {
                        pending.get(&path).is_some_and(|replies| replies.iter().any(|r| r.active.load(Ordering::Relaxed)))
                    });
                    let result = if wanted && !CANCELLED.load(Ordering::Relaxed) { get(&job).map(Arc::new) } else { None };
                    finish(&job, result);
                }
            });
        }
        tx
    })
}
fn finish(job: &Job, result: Option<Arc<Vec<u8>>>) {
    let replies = PENDING
        .get()
        .and_then(|pending| pending.lock().ok())
        .and_then(|mut pending| pending.remove(&path(job)))
        .unwrap_or_default();
    for reply in replies {
        if reply.active.load(Ordering::Relaxed) {
            let _ = reply.tx.try_send(result.clone());
        }
    }
}
fn cached(job: &Job) -> Option<Vec<u8>> {
    if let Some(group) = &job.group {
        if let Ok(bytes) = std::fs::read(path(job)) {
            return Some(bytes);
        }
        if let Some(cover) = &group.cover {
            let mut native = job.clone();
            native.key = cover.clone();
            native.group = None;
            if let Some(bytes) = cached(&native) {
                write_group(job, &bytes);
                return Some(bytes);
            }
        }
        return None;
    }
    std::iter::once(&job.key).chain(job.fallback.iter()).find_map(|key| {
        let mut job = job.clone();
        job.key = key.clone();
        std::fs::read(path(&job)).ok()
    })
}
fn workers() -> &'static async_channel::Sender<Job> {
    static READERS: OnceLock<async_channel::Sender<Job>> = OnceLock::new();
    READERS.get_or_init(|| {
        let (tx, rx) = async_channel::bounded::<Job>(64);
        for _ in 0..2 {
            let rx = rx.clone();
            std::thread::spawn(move || {
                while let Ok(job) = rx.recv_blocking() {
                    if CANCELLED.load(Ordering::Relaxed) {
                        finish(&job, None);
                        continue;
                    }
                    if let Some(bytes) = cached(&job) {
                        finish(&job, Some(Arc::new(bytes)));
                    } else {
                        enqueue_network(job);
                    }
                }
            });
        }
        tx
    })
}
fn enqueue_network(job: Job) {
    let network = network_workers();
    if let Err(error) = network.try_send(job) {
        let job = error.into_inner();
        if let Some(retired) = NETWORK_QUEUE.get().and_then(|q| q.try_recv().ok()) {
            finish(&retired, None);
        }
        if let Err(error) = network.try_send(job) {
            finish(&error.into_inner(), None);
        }
    }
}
fn path(job: &Job) -> PathBuf {
    let hash = Sha256::digest(format!("{:?}\0{}\0{}\0{}", job.server, job.key, job.width, job.height));
    // JPEG unless the picture has transparency (PNG); the format is read from the data.
    nixe_core::paths::cache_dir().join("artwork").join(format!("{hash:x}.img"))
}
fn get(job: &Job) -> Option<Vec<u8>> {
    if let Some(group) = &job.group {
        return group_image(job, group);
    }
    std::iter::once(&job.key).chain(job.fallback.iter()).find_map(|key| {
        let mut candidate = job.clone();
        candidate.key = key.clone();
        get_one(&candidate)
    })
}
fn get_one(job: &Job) -> Option<Vec<u8>> {
    let path = path(job);
    if let Ok(bytes) = std::fs::read(&path) {
        if let Ok(file) = std::fs::File::open(&path) {
            let _ = file.set_modified(SystemTime::now());
        }
        return Some(bytes);
    }
    let raw = job.client.as_ref()?.artwork(&job.key, job.width, job.height).ok()?;
    let loader = gtk::gdk_pixbuf::PixbufLoader::new();
    let (width, height) = (job.width as i32, job.height as i32);
    loader.connect_size_prepared(move |loader, w, h| {
        if w > 0 && h > 0 {
            let scale = (width as f64 / w as f64).min(height as f64 / h as f64).min(1.0);
            loader.set_size((w as f64 * scale).max(1.0) as i32, (h as f64 * scale).max(1.0) as i32);
        }
    });
    loader.write(&raw).ok()?;
    loader.close().ok()?;
    let pixbuf = loader.pixbuf()?;
    let bytes = if pixbuf.has_alpha() {
        pixbuf.save_to_bufferv("png", &[]).ok()?
    } else {
        pixbuf.save_to_bufferv("jpeg", &[("quality", "88")]).ok()?
    };
    std::fs::create_dir_all(path.parent()?).ok()?;
    let temp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    std::fs::write(&temp, &bytes).ok()?;
    std::fs::rename(&temp, &path).ok()?;
    if let Ok(cache) = nixe_media_cache::Cache::open(&nixe_core::paths::cache_dir().join("library.sqlite3")) {
        let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
        let _ = cache.index_artwork(
            &job.server,
            &format!("{}/{}/{}", job.key, job.width, job.height),
            &path,
            bytes.len() as u64,
            now,
        );
    }
    static LAST_TRIM: OnceLock<Mutex<Instant>> = OnceLock::new();
    if let Ok(mut last) = LAST_TRIM.get_or_init(|| Mutex::new(Instant::now() - std::time::Duration::from_secs(60))).try_lock()
        && last.elapsed().as_secs() >= 60 {
            *last = Instant::now();
            trim();
        }
    Some(bytes)
}
fn trim() {
    let dir = nixe_core::paths::cache_dir().join("artwork");
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            // `.png` files are from before artwork was kept as JPEG; they age out too.
            (m.is_file() && e.path().extension().is_some_and(|e| e == "img" || e == "png"))
                .then(|| (m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len(), e.path()))
        })
        .collect();
    let mut size: u64 = files.iter().map(|(_, n, _)| n).sum();
    if size <= 512 * 1024 * 1024 {
        return;
    }
    files.sort_by_key(|(time, _, _)| *time);
    for (_, bytes, path) in files {
        if size <= 512 * 1024 * 1024 {
            break;
        }
        if std::fs::remove_file(path).is_ok() {
            size = size.saturating_sub(bytes);
        }
    }
}
pub fn load(art: &Artwork, server: &ServerId, key: Option<&str>, width: u32, height: u32) {
    load_keys(art, server, key.map(|s| vec![s.to_owned()]).unwrap_or_default(), width, height);
}
pub fn load_poster(art: &Artwork, item: &MediaItem, width: u32, height: u32) {
    if item.kind == MediaKind::Collection {
        load_group(art, &item.title, Some(item.id.clone()), false, item.poster.clone(), &[], width, height);
    } else {
        load_keys(art, &item.id.server, nixe_media_model::tv::posters(item), width, height);
    }
}
pub fn load_playlist(art: &Artwork, list: &crate::playlist_store::Playlist, width: u32, height: u32) {
    let members: Vec<_> = list.entries.iter().take(4).map(|entry| entry.media.clone()).collect();
    load_group_named(art, &list.id, &list.title, list.origin.clone(), true, list.poster.clone(), &members, width, height);
}
#[allow(clippy::too_many_arguments)]
fn load_group(
    art: &Artwork,
    title: &str,
    id: Option<ItemId>,
    playlist: bool,
    cover: Option<String>,
    members: &[MediaItem],
    width: u32,
    height: u32,
) {
    let key = id.as_ref().map(|id| id.id.clone()).unwrap_or_else(|| title.into());
    load_group_named(art, &key, title, id, playlist, cover, members, width, height);
}
#[allow(clippy::too_many_arguments)]
fn load_group_named(
    art: &Artwork,
    name: &str,
    title: &str,
    id: Option<ItemId>,
    playlist: bool,
    cover: Option<String>,
    members: &[MediaItem],
    width: u32,
    height: u32,
) {
    let fallback = gtk::Box::new(gtk::Orientation::Vertical, 12);
    fallback.add_css_class("group-poster");
    fallback.set_valign(gtk::Align::Fill);
    fallback.set_halign(gtk::Align::Fill);
    fallback.set_can_target(false);
    let initials = title.split_whitespace().filter_map(|w| w.chars().next()).take(2).collect::<String>().to_uppercase();
    let monogram = gtk::Label::new(Some(&initials));
    monogram.add_css_class("group-poster-monogram");
    monogram.set_vexpand(true);
    monogram.set_valign(gtk::Align::End);
    fallback.append(&monogram);
    let kind = gtk::Label::new(Some(if playlist { "PLAYLIST" } else { "COLLECTION" }));
    kind.add_css_class("group-poster-kind");
    kind.set_vexpand(true);
    kind.set_valign(gtk::Align::Start);
    fallback.append(&kind);
    art.root.add_overlay(&fallback);
    let weak = fallback.downgrade();
    art.picture.connect_visible_notify(move |picture| {
        if let Some(fallback) = weak.upgrade() {
            fallback.set_visible(!picture.is_visible());
        }
    });
    let server = id
        .as_ref()
        .map(|id| id.server.clone())
        .unwrap_or_else(|| ServerId { provider: ProviderKind::Plex, id: format!("local-playlist:{name}") });
    let member_jobs = members
        .iter()
        .filter_map(|item| {
            let mut keys = nixe_media_model::tv::posters(item);
            if keys.is_empty() {
                return None;
            }
            Some(Job {
                client: crate::servers::client(&item.id.server),
                server: item.id.server.clone(),
                key: keys.remove(0),
                fallback: keys,
                width,
                height,
                group: None,
            })
        })
        .collect::<Vec<_>>();
    let fingerprint = member_jobs.iter().map(|j| format!("{:?}:{}", j.server, j.key)).collect::<Vec<_>>().join("|");
    let key = format!("group-v1:{name}:{cover:?}:{fingerprint}");
    load_spec(art, &server, vec![key], width, height, Some(Group { id, playlist, cover, members: member_jobs }));
}
fn load_keys(art: &Artwork, server: &ServerId, keys: Vec<String>, width: u32, height: u32) {
    load_spec(art, server, keys, width, height, None);
}
fn load_spec(art: &Artwork, server: &ServerId, mut keys: Vec<String>, width: u32, height: u32, group: Option<Group>) {
    art.set_texture(None);
    if keys.is_empty() {
        return;
    }
    let key = keys.remove(0);
    let root = art.root.downgrade();
    let picture = art.picture.downgrade();
    let placeholder = art.placeholder.downgrade();
    let active: Rc<RefCell<Option<Arc<AtomicBool>>>> = Rc::new(RefCell::new(None));
    let current = active.clone();
    art.root.connect_unmap(move |_| {
        if let Some(flag) = current.borrow_mut().take() {
            flag.store(false, Ordering::Relaxed);
        }
    });
    let server = server.clone();
    let load: Rc<dyn Fn()> = Rc::new(move || {
        if CANCELLED.load(Ordering::Relaxed) || active.borrow().is_some() {
            return;
        }
        let (Some(root), Some(picture), Some(placeholder)) = (root.upgrade(), picture.upgrade(), placeholder.upgrade()) else {
            return;
        };
        if !root.is_mapped() || picture.paintable().is_some() {
            return;
        }
        let flag = Arc::new(AtomicBool::new(true));
        *active.borrow_mut() = Some(flag.clone());
        let (tx, rx) = async_channel::bounded(1);
        let mut active_group = group.clone();
        if let Some(group) = &mut active_group {
            for member in &mut group.members {
                member.client = crate::servers::client(&member.server);
            }
        }
        let job = Job {
            client: crate::servers::client(&server),
            server: server.clone(),
            key: key.clone(),
            fallback: keys.clone(),
            width,
            height,
            group: active_group,
        };
        let offline = job.client.is_none() && job.group.as_ref().is_none_or(|g| g.members.iter().all(|m| m.client.is_none()));
        let retry_servers = std::iter::once(server.clone())
            .chain(job.group.iter().flat_map(|g| g.members.iter().map(|m| m.server.clone())))
            .collect::<Vec<_>>();
        let path = path(&job);
        let first = {
            let mut pending = PENDING.get_or_init(|| Mutex::new(HashMap::new())).lock().unwrap();
            let first = !pending.contains_key(&path);
            pending.entry(path.clone()).or_default().push(Reply { tx, active: flag.clone() });
            first
        };
        let (active, picture, placeholder) = (active.clone(), picture.downgrade(), placeholder.downgrade());
        glib::spawn_future_local(async move {
            if first && workers().send(job).await.is_err() {
                PENDING.get().unwrap().lock().unwrap().remove(&path);
            }
            let received = rx.recv().await;
            if CANCELLED.load(Ordering::Relaxed) || !flag.load(Ordering::Relaxed) {
                return;
            }
            active.borrow_mut().take();
            let (Some(picture), Some(placeholder)) = (picture.upgrade(), placeholder.upgrade()) else { return };
            if let Ok(Some(bytes)) = received {
                if let Ok(texture) = gdk::Texture::from_bytes(&glib::Bytes::from_owned((*bytes).clone())) {
                    picture.set_paintable(Some(&texture));
                    picture.set_visible(true);
                    placeholder.set_visible(false);
                }
            } else if offline && retry_servers.iter().any(|server| crate::servers::client(server).is_some()) {
                // A connection may become ready while the offline cache read is queued.
                retry_visible();
            }
        });
    });
    LOADERS.with(|loaders| loaders.borrow_mut().push(Rc::downgrade(&load)));
    let mapped = load.clone();
    art.root.connect_map(move |_| mapped());
    load();
}

fn group_image(job: &Job, group: &Group) -> Option<Vec<u8>> {
    if let Ok(bytes) = std::fs::read(path(job)) {
        return Some(bytes);
    }
    if let Some(cover) = &group.cover {
        let mut native = job.clone();
        native.group = None;
        native.key = cover.clone();
        native.fallback.clear();
        if let Some(bytes) = get_one(&native) {
            write_group(job, &bytes);
            return Some(bytes);
        }
    }
    let mut members = group.members.clone();
    if members.is_empty() {
        let id = group.id.as_ref()?;
        let client = job.client.as_ref()?;
        let page = if group.playlist { client.playlist_items(id, None) } else { client.child_page(id, "", None) }.ok()?;
        for item in page.items.into_iter().take(12) {
            let mut keys = nixe_media_model::tv::posters(&item);
            if keys.is_empty() {
                continue;
            }
            members.push(Job {
                client: Some(client.clone()),
                server: item.id.server,
                key: keys.remove(0),
                fallback: keys,
                width: job.width,
                height: job.height,
                group: None,
            });
            if members.len() == 4 {
                break;
            }
        }
    }
    let images = members
        .iter()
        .take(4)
        .filter_map(|member| {
            let bytes = get(member)?;
            let loader = gtk::gdk_pixbuf::PixbufLoader::new();
            loader.write(&bytes).ok()?;
            loader.close().ok()?;
            loader.pixbuf()
        })
        .collect::<Vec<_>>();
    if images.is_empty() {
        return None;
    }
    let bytes = compose(&images, job.width, job.height)?;
    write_group(job, &bytes);
    Some(bytes)
}
fn compose(images: &[gtk::gdk_pixbuf::Pixbuf], width: u32, height: u32) -> Option<Vec<u8>> {
    if images.is_empty() {
        return None;
    }
    let canvas = gtk::gdk_pixbuf::Pixbuf::new(gtk::gdk_pixbuf::Colorspace::Rgb, false, 8, width as i32, height as i32)?;
    canvas.fill(0);
    let columns = if images.len() > 1 { 2 } else { 1 };
    let rows = if images.len() > 2 { 2 } else { 1 };
    for index in 0..(columns * rows) {
        let image = &images[index % images.len()];
        let x = (index % columns) as i32 * canvas.width() / columns as i32;
        let y = (index / columns) as i32 * canvas.height() / rows as i32;
        let w = canvas.width() / columns as i32;
        let h = canvas.height() / rows as i32;
        let scale = (w as f64 / image.width() as f64).max(h as f64 / image.height() as f64);
        let resized = image.scale_simple(
            (image.width() as f64 * scale).ceil() as i32,
            (image.height() as f64 * scale).ceil() as i32,
            gtk::gdk_pixbuf::InterpType::Bilinear,
        )?;
        resized.copy_area((resized.width() - w) / 2, (resized.height() - h) / 2, w, h, &canvas, x, y);
    }
    let bytes = canvas.save_to_bufferv("png", &[]).ok()?;
    Some(bytes)
}
fn write_group(job: &Job, bytes: &[u8]) {
    let target = path(job);
    if let Some(parent) = target.parent() {
        let _ = std::fs::create_dir_all(parent);
        let _ = nixe_core::cmd::atomic_write(&target, bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn member_mosaics_have_poster_dimensions_and_distinct_tiles() {
        let red = gtk::gdk_pixbuf::Pixbuf::new(gtk::gdk_pixbuf::Colorspace::Rgb, false, 8, 12, 18).unwrap();
        red.fill(0xff0000ff);
        let blue = red.copy().unwrap();
        blue.fill(0x0000ffff);
        let bytes = compose(&[red, blue], 420, 630).unwrap();
        let loader = gtk::gdk_pixbuf::PixbufLoader::new();
        loader.write(&bytes).unwrap();
        loader.close().unwrap();
        let image = loader.pixbuf().unwrap();
        assert_eq!((image.width(), image.height()), (420, 630));
        let pixels = image.read_pixel_bytes();
        assert_eq!(&pixels.as_ref()[0..3], &[255, 0, 0]);
        assert_eq!(&pixels.as_ref()[210 * 3..210 * 3 + 3], &[0, 0, 255]);
    }
}

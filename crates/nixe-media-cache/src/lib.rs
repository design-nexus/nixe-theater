//! Worker-owned metadata snapshots. Never store tokens or resolved stream URLs.
use anyhow::{Context, Result, ensure};
use nixe_media_model::{ItemId, MediaItem, ServerId};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

pub struct Cache {
    db: Connection,
}

impl Cache {
    /// Open on a worker thread, not GTK's main thread.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        Self::from_connection(Connection::open(path)?)
    }

    fn from_connection(db: Connection) -> Result<Self> {
        db.busy_timeout(std::time::Duration::from_secs(3))?;
        let version: i64 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
        ensure!(version <= 3, "This cache needs a newer version of Theater");
        // Existing WAL databases need no schema writes on cache readers/artwork workers.
        if version == 3 {
            return Ok(Self { db });
        }
        db.execute_batch(
            "PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS media (
                server TEXT NOT NULL, library TEXT NOT NULL, item TEXT NOT NULL,
                metadata TEXT NOT NULL, PRIMARY KEY(server, library, item));
            CREATE TABLE IF NOT EXISTS artwork (
                server TEXT NOT NULL, artwork_key TEXT NOT NULL, local_path TEXT NOT NULL,
                accessed_at INTEGER NOT NULL, bytes INTEGER NOT NULL CHECK(bytes >= 0),
                PRIMARY KEY(server, artwork_key));
            CREATE TABLE IF NOT EXISTS playback_watch (
                server TEXT NOT NULL, library TEXT NOT NULL, item TEXT NOT NULL,
                metadata TEXT NOT NULL, revision INTEGER NOT NULL,
                PRIMARY KEY(server, item));
            CREATE TABLE IF NOT EXISTS library_snapshots (
                server TEXT NOT NULL, library TEXT NOT NULL, revision INTEGER NOT NULL,
                PRIMARY KEY(server, library));
            CREATE TABLE IF NOT EXISTS details (
                server TEXT NOT NULL, item TEXT NOT NULL, metadata TEXT NOT NULL,
                children TEXT NOT NULL, revision INTEGER NOT NULL,
                PRIMARY KEY(server, item));
            CREATE TABLE IF NOT EXISTS item_snapshots (
                server TEXT NOT NULL, item TEXT NOT NULL, revision INTEGER NOT NULL,
                PRIMARY KEY(server, item));
            PRAGMA user_version=3;",
        )?;
        Ok(Self { db })
    }

    /// Call only after fetching every page successfully. A failed refresh must
    /// leave the previous offline snapshot intact. This never touches media files.
    pub fn replace_library(&mut self, server: &ServerId, library: &str, items: &[MediaItem]) -> Result<()> {
        let started = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos();
        self.replace_library_since(server, library, items, i64::try_from(started)?)
    }

    /// Retain progress recorded after this network snapshot began.
    pub fn replace_library_since(&mut self, server: &ServerId, library: &str, items: &[MediaItem], started: i64) -> Result<()> {
        ensure!(
            items.iter().all(|i| &i.id.server == server && i.library_id == library),
            "Snapshot contains another server or library"
        );
        let key = serde_json::to_string(server)?;
        let tx = self.db.transaction()?;
        tx.execute("DELETE FROM media WHERE server=?1 AND library=?2", params![key, library])?;
        for item in items {
            tx.execute(
                "INSERT INTO media VALUES (?1, ?2, ?3, ?4)",
                params![key, library, item.id.id, serde_json::to_string(item)?],
            )?;
        }
        tx.execute(
            "DELETE FROM playback_watch WHERE server=?1 AND library=?2 AND revision<=?3
            AND EXISTS(SELECT 1 FROM media m WHERE m.server=playback_watch.server AND m.item=playback_watch.item)",
            params![key, library, started],
        )?;
        tx.execute(
            "INSERT INTO library_snapshots VALUES (?1,?2,?3)
            ON CONFLICT(server,library) DO UPDATE SET revision=MAX(revision,excluded.revision)",
            params![key, library, started],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn items(&self, server: &ServerId, library: &str) -> Result<Vec<MediaItem>> {
        let key = serde_json::to_string(server)?;
        let mut stmt = self.db.prepare(
            "SELECT COALESCE(w.metadata,m.metadata) FROM media m LEFT JOIN playback_watch w
            ON m.server=w.server AND m.item=w.item WHERE m.server=?1 AND m.library=?2
            UNION ALL SELECT w.metadata FROM playback_watch w WHERE w.server=?1 AND w.library=?2
            AND NOT EXISTS(SELECT 1 FROM media m WHERE m.server=w.server AND m.item=w.item)",
        )?;
        let rows = stmt.query_map(params![key, library], |row| row.get::<_, String>(0))?;
        rows.map(|row| serde_json::from_str(&row?).context("Invalid cached metadata")).collect()
    }

    pub fn all_items(&self, server: &ServerId) -> Result<Vec<MediaItem>> {
        let mut stmt = self.db.prepare(
            "SELECT COALESCE(w.metadata,m.metadata) FROM media m LEFT JOIN playback_watch w
            ON m.server=w.server AND m.item=w.item WHERE m.server=?1
            UNION ALL SELECT w.metadata FROM playback_watch w WHERE w.server=?1
            AND NOT EXISTS(SELECT 1 FROM media m WHERE m.server=w.server AND m.item=w.item)",
        )?;
        let rows = stmt.query_map([serde_json::to_string(server)?], |row| row.get::<_, String>(0))?;
        rows.map(|row| serde_json::from_str(&row?).context("Invalid cached metadata")).collect()
    }

    pub fn playback_since(&self, server: &ServerId, started: i64) -> Result<Vec<MediaItem>> {
        let mut stmt = self.db.prepare("SELECT metadata FROM playback_watch WHERE server=?1 AND revision>?2")?;
        let rows = stmt.query_map(params![serde_json::to_string(server)?, started], |r| r.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    /// Local playback state is an overlay, never a cross-provider write queue.
    /// Older worker completions cannot replace a newer session's progress.
    pub fn record_playback(&self, item: &MediaItem, revision: i64) -> Result<()> {
        self.db.execute(
            "INSERT INTO playback_watch SELECT ?1, ?2, ?3, ?4, ?5
            WHERE NOT EXISTS(SELECT 1 FROM library_snapshots s JOIN media m ON s.server=m.server AND s.library=m.library
                WHERE s.server=?1 AND s.library=?2 AND m.item=?3 AND s.revision>=?5)
            AND NOT EXISTS(SELECT 1 FROM item_snapshots WHERE server=?1 AND item=?3 AND revision>=?5)
            ON CONFLICT(server,item) DO UPDATE SET library=excluded.library,
            metadata=excluded.metadata, revision=excluded.revision
            WHERE excluded.revision>=playback_watch.revision",
            params![serde_json::to_string(&item.id.server)?, item.library_id, item.id.id, serde_json::to_string(item)?, revision],
        )?;
        Ok(())
    }

    /// Store only complete child snapshots, including successfully fetched empty lists.
    pub fn save_details(&mut self, item: &MediaItem, children: &[MediaItem], revision: i64) -> Result<()> {
        ensure!(children.iter().all(|child| child.id.server == item.id.server), "Children contain another server");
        let tx = self.db.transaction()?;
        tx.execute(
            "INSERT INTO details VALUES (?1,?2,?3,?4,?5)
            ON CONFLICT(server,item) DO UPDATE SET metadata=excluded.metadata,
            children=excluded.children, revision=excluded.revision WHERE excluded.revision>=details.revision",
            params![
                serde_json::to_string(&item.id.server)?,
                item.id.id,
                serde_json::to_string(item)?,
                serde_json::to_string(children)?,
                revision
            ],
        )?;
        let server = serde_json::to_string(&item.id.server)?;
        for media in std::iter::once(item).chain(children.iter()) {
            tx.execute(
                "UPDATE playback_watch SET metadata=?4, revision=?3 WHERE server=?1 AND item=?2 AND revision<=?3",
                params![server, media.id.id, revision, serde_json::to_string(media)?],
            )?;
            tx.execute(
                "INSERT INTO item_snapshots VALUES (?1,?2,?3)
                ON CONFLICT(server,item) DO UPDATE SET revision=MAX(revision,excluded.revision)",
                params![server, media.id.id, revision],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Read compact rating overlays in one pass, without loading child pages or watches.
    pub fn detail_ratings(&self) -> Result<Vec<MediaItem>> {
        let mut statement = self.db.prepare("SELECT metadata FROM details")?;
        let rows = statement.query_map([], |r| r.get::<_, String>(0))?;
        rows.filter_map(|row| match row {
            Ok(body) => match serde_json::from_str::<MediaItem>(&body) {
                Ok(item) if !item.metadata.ratings.is_empty() => Some(Ok(item)),
                Ok(_) => None,
                Err(e) => Some(Err(e.into())),
            },
            Err(e) => Some(Err(e.into())),
        })
        .collect()
    }
    pub fn details(&self, id: &ItemId) -> Result<Option<(MediaItem, Vec<MediaItem>)>> {
        let server = serde_json::to_string(&id.server)?;
        let row: Option<(String, String)> = self
            .db
            .query_row("SELECT metadata,children FROM details WHERE server=?1 AND item=?2", params![server, id.id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?;
        let Some((metadata, children)) = row else { return Ok(None) };
        let mut item: MediaItem = serde_json::from_str(&metadata)?;
        let mut children: Vec<MediaItem> = serde_json::from_str(&children)?;
        let mut stmt = self.db.prepare("SELECT metadata FROM playback_watch WHERE server=?1")?;
        let rows = stmt.query_map([server], |r| r.get::<_, String>(0))?;
        let mut watches = std::collections::HashMap::new();
        for row in rows {
            let played: MediaItem = serde_json::from_str(&row?)?;
            watches.insert(played.id, played.watch);
        }
        for media in std::iter::once(&mut item).chain(children.iter_mut()) {
            if let Some(watch) = watches.get(&media.id) {
                media.watch = watch.clone();
            }
        }
        Ok(Some((item, children)))
    }

    /// Keys are provider-relative, credential-free artwork identifiers, not URLs.
    pub fn index_artwork(&self, server: &ServerId, key: &str, path: &Path, bytes: u64, accessed_at: i64) -> Result<()> {
        ensure!(!key.contains('?') && !key.contains("://"), "Artwork keys must not contain URLs or credentials");
        let bytes = i64::try_from(bytes)?;
        self.db.execute(
            "INSERT INTO artwork VALUES (?1, ?2, ?3, ?4, ?5)
            ON CONFLICT(server, artwork_key) DO UPDATE SET local_path=excluded.local_path,
            accessed_at=excluded.accessed_at, bytes=excluded.bytes",
            params![serde_json::to_string(server)?, key, path.to_string_lossy(), accessed_at, bytes],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nixe_media_model::*;
    fn item(server: &ServerId, title: &str) -> MediaItem {
        MediaItem {
            id: ItemId { server: server.clone(), id: "1".into() },
            library_id: "movies".into(),
            kind: MediaKind::Movie,
            title: title.into(),
            tv: Default::default(),
            metadata: Default::default(),
            summary: String::new(),
            year: None,
            external_ids: ExternalIds::default(),
            parent: None,
            series: None,
            season_number: None,
            episode_number: None,
            duration_ms: None,
            added_at: None,
            poster: None,
            backdrop: None,
            versions: vec![],
            watch: WatchState::default(),
        }
    }
    #[test]
    fn details_keep_complete_children_separate_from_library_snapshots() -> Result<()> {
        let mut cache = Cache::from_connection(Connection::open_in_memory()?)?;
        let plex = ServerId { provider: ProviderKind::Plex, id: "server".into() };
        let jelly = ServerId { provider: ProviderKind::Jellyfin, id: "server".into() };
        let parent = item(&plex, "Series");
        let mut episode = item(&plex, "Episode");
        episode.id.id = "2".into();
        cache.save_details(&parent, &[episode.clone()], 10)?;
        cache.save_details(&parent, &[], 5)?;
        assert!(cache.details(&ItemId { server: jelly.clone(), id: parent.id.id.clone() })?.is_none());
        assert!(cache.save_details(&parent, &[item(&jelly, "Wrong server")], 20).is_err());
        cache.replace_library_since(&plex, "movies", std::slice::from_ref(&parent), 15)?;
        episode.watch.position_ms = 20_000;
        cache.record_playback(&episode, 20)?;
        cache.replace_library_since(&plex, "movies", std::slice::from_ref(&parent), 25)?;
        let (_, children) = cache.details(&parent.id)?.unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].watch.position_ms, 20_000);
        let mut fresh = episode.clone();
        fresh.watch.position_ms = 40_000;
        cache.save_details(&parent, &[fresh], 30)?;
        cache.record_playback(&episode, 20)?;
        assert_eq!(cache.details(&parent.id)?.unwrap().1[0].watch.position_ms, 40_000);
        cache.save_details(&parent, &[], 35)?;
        assert!(cache.details(&parent.id)?.unwrap().1.is_empty());
        Ok(())
    }
    #[test]
    fn playback_survives_inflight_snapshots_and_rejects_old_reports() -> Result<()> {
        let mut cache = Cache::from_connection(Connection::open_in_memory()?)?;
        let server = ServerId { provider: ProviderKind::Plex, id: "server".into() };
        let original = item(&server, "Movie");
        cache.replace_library_since(&server, "movies", std::slice::from_ref(&original), 5)?;
        let mut playing = original.clone();
        playing.watch.position_ms = 42_000;
        cache.record_playback(&playing, 20)?;
        cache.record_playback(&original, 10)?;
        cache.replace_library_since(&server, "movies", std::slice::from_ref(&original), 15)?;
        assert_eq!(cache.all_items(&server)?[0].watch.position_ms, 42_000);
        assert_eq!(cache.items(&server, "movies")?[0].watch.position_ms, 42_000);
        // A later authoritative snapshot retires the local overlay.
        cache.replace_library_since(&server, "movies", &[original], 25)?;
        cache.record_playback(&playing, 20)?;
        assert_eq!(cache.all_items(&server)?[0].watch.position_ms, 0);
        playing.id.id = "episode".into();
        cache.record_playback(&playing, 30)?;
        assert_eq!(cache.all_items(&server)?.len(), 2);
        Ok(())
    }
    #[test]
    fn isolates_servers_and_rolls_back_incomplete_snapshots() -> Result<()> {
        let mut cache = Cache::from_connection(Connection::open_in_memory()?)?;
        let plex = ServerId { provider: ProviderKind::Plex, id: "server".into() };
        let jelly = ServerId { provider: ProviderKind::Jellyfin, id: "server".into() };
        cache.replace_library(&plex, "movies", &[item(&plex, "Plex")])?;
        cache.replace_library(&jelly, "movies", &[item(&jelly, "Jellyfin")])?;
        // A duplicate key fails after the old records were deleted inside the transaction.
        assert!(cache.replace_library(&plex, "movies", &[item(&plex, "New"), item(&plex, "Duplicate")]).is_err());
        assert_eq!(cache.items(&plex, "movies")?[0].title, "Plex");
        assert_eq!(cache.items(&jelly, "movies")?[0].title, "Jellyfin");
        assert!(cache.replace_library(&plex, "movies", &[item(&jelly, "Wrong server")]).is_err());
        assert_eq!(Reconciliation::default(), Reconciliation::Disabled);
        Ok(())
    }
}

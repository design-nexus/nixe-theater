//! Durable user-created lists live under XDG data, never the disposable cache.
use nixe_media_model::{ItemId, MediaItem, ProviderError};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
type Result<T> = std::result::Result<T, ProviderError>;
fn failure() -> ProviderError {
    ProviderError::Failed("Playlists could not be saved or loaded".into())
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub media: MediaItem,
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Playlist {
    pub id: String,
    pub title: String,
    pub origin: Option<ItemId>,
    pub entries: Vec<Entry>,
    #[serde(default)]
    pub poster: Option<String>,
    #[serde(default)]
    pub count: Option<u64>,
}
impl Playlist {
    pub fn new(title: String) -> Self {
        Self { id: uuid::Uuid::new_v4().to_string(), title, origin: None, entries: vec![], poster: None, count: None }
    }
    pub fn add(&mut self, item: MediaItem) {
        self.entries.push(Entry { id: uuid::Uuid::new_v4().to_string(), media: item });
    }
    pub fn move_entry(&mut self, id: &str, delta: isize) {
        if let Some(index) = self.entries.iter().position(|e| e.id == id)
            && let Some(next) = index.checked_add_signed(delta).filter(|&n| n < self.entries.len()) {
                self.entries.swap(index, next);
            }
    }
}
pub struct Store(Connection);
impl Store {
    pub fn open() -> Result<Self> {
        let dir = nixe_core::paths::data_dir();
        std::fs::create_dir_all(&dir).map_err(|_| failure())?;
        Self::init(Connection::open(dir.join("playlists.sqlite3")).map_err(|_| failure())?)
    }
    fn init(db: Connection) -> Result<Self> {
        db.busy_timeout(std::time::Duration::from_secs(2)).map_err(|_| failure())?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS playlists(id TEXT PRIMARY KEY, body TEXT NOT NULL);",
        )
        .map_err(|_| failure())?;
        Ok(Self(db))
    }
    pub fn list(&self) -> Result<Vec<Playlist>> {
        let mut statement = self.0.prepare("SELECT body FROM playlists ORDER BY id").map_err(|_| failure())?;
        let rows = statement.query_map([], |r| r.get::<_, String>(0)).map_err(|_| failure())?;
        rows.map(|row| serde_json::from_str(&row.map_err(|_| failure())?).map_err(|_| failure())).collect()
    }
    pub fn load(&self, id: &str) -> Result<Playlist> {
        let body: String =
            self.0.query_row("SELECT body FROM playlists WHERE id=?1", [id], |r| r.get(0)).map_err(|_| failure())?;
        serde_json::from_str(&body).map_err(|_| failure())
    }
    pub fn save(&self, list: &Playlist) -> Result<()> {
        let body = serde_json::to_string(list).map_err(|_| failure())?;
        self.0
            .execute(
                "INSERT INTO playlists(id,body) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET body=excluded.body",
                params![list.id, body],
            )
            .map_err(|_| failure())?;
        Ok(())
    }
    pub fn edit(&mut self, id: &str, edit: impl FnOnce(&mut Playlist)) -> Result<()> {
        let tx = self.0.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(|_| failure())?;
        let body: String = tx.query_row("SELECT body FROM playlists WHERE id=?1", [id], |r| r.get(0)).map_err(|_| failure())?;
        let mut list: Playlist = serde_json::from_str(&body).map_err(|_| failure())?;
        if list.origin.is_some() {
            return Err(ProviderError::Unsupported);
        }
        edit(&mut list);
        let body = serde_json::to_string(&list).map_err(|_| failure())?;
        tx.execute("UPDATE playlists SET body=?2 WHERE id=?1", params![id, body]).map_err(|_| failure())?;
        tx.commit().map_err(|_| failure())
    }
    pub fn delete_local(&self, id: &str) -> Result<()> {
        let list = self.load(id)?;
        if list.origin.is_some() {
            return Err(ProviderError::Unsupported);
        }
        self.0.execute("DELETE FROM playlists WHERE id=?1", [id]).map_err(|_| failure())?;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn durable_lists_preserve_duplicates_order_and_remote_read_only() {
        let mut store = Store::init(Connection::open_in_memory().unwrap()).unwrap();
        let mut list = Playlist::new("Local".into());
        // Reuse a serialized fixture shape without depending on GTK or a server.
        let media:MediaItem=serde_json::from_value(serde_json::json!({"id":{"server":{"provider":"plex","id":"server"},"id":"1"},"library_id":"1","kind":"movie","title":"Movie","summary":"","year":null,"external_ids":{"tmdb":null,"imdb":null,"tvdb":null},"parent":null,"series":null,"season_number":null,"episode_number":null,"duration_ms":null,"added_at":null,"poster":null,"backdrop":null,"versions":[],"watch":{"position_ms":0,"watched":false,"updated_at":null}})).unwrap();
        list.add(media.clone());
        list.add(media.clone());
        store.save(&list).unwrap();
        let last = list.entries[1].id.clone();
        store.edit(&list.id, |list| list.move_entry(&last, -1)).unwrap();
        let loaded = store.load(&list.id).unwrap();
        assert_eq!(loaded.entries.len(), 2);
        assert_eq!(loaded.entries[0].id, last);
        list.origin = Some(media.id);
        store.save(&list).unwrap();
        assert!(store.delete_local(&list.id).is_err());
        assert!(store.edit(&list.id, |list| list.title = "bad".into()).is_err());
    }
}

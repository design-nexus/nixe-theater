//! Prepared pages are safe to show before credentials or network requests complete.
use crate::catalog::View;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
fn path(root: &Path, key: &str) -> PathBuf {
    root.join(format!("{:x}.json", Sha256::digest(key)))
}
pub fn load(root: &Path, key: &str, generation: u64) -> Option<View> {
    let file = path(root, key);
    if std::fs::metadata(&file).ok()?.len() > 32 * 1024 * 1024 {
        return None;
    }
    let mut view: View = serde_json::from_slice(&std::fs::read(file).ok()?).ok()?;
    view.generation = generation;
    Some(view)
}
pub fn save(root: &Path, key: &str, view: &View) {
    if std::fs::create_dir_all(root).is_err() {
        return;
    }
    if let Ok(data) = serde_json::to_vec(view)
        && data.len() <= 32 * 1024 * 1024 {
            let _ = nixe_core::cmd::atomic_write(&path(root, key), data);
        }
    prune(root, KEPT);
}
/// Snapshots kept: one per page, sort, filter and server set seen, so the newest few
/// dozen cover what's in use.
const KEPT: usize = 48;
/// Remove all but the `keep` most recently written snapshots.
fn prune(root: &Path, keep: usize) {
    let mut files: Vec<_> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    if files.len() <= keep {
        return;
    }
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    for (_, path) in files.into_iter().skip(keep) {
        let _ = std::fs::remove_file(path);
    }
}
pub fn root() -> PathBuf {
    nixe_core::paths::cache_dir().join("views")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snapshots_are_scoped_and_retagged_without_network() {
        let root = std::env::temp_dir().join(format!("theater-view-{}", uuid::Uuid::new_v4()));
        let view = View { generation: 1, page: "home".into(), items: vec![], home: Some(Default::default()) };
        save(&root, "server-a/home", &view);
        assert_eq!(load(&root, "server-a/home", 42).unwrap().generation, 42);
        assert!(load(&root, "server-b/home", 42).is_none());
        assert!(load(&root, "server-a/movies", 42).is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn keeps_only_the_newest_snapshots() {
        let root = std::env::temp_dir().join(format!("theater-view-{}", uuid::Uuid::new_v4()));
        let view = View { generation: 1, page: "home".into(), items: vec![], home: Some(Default::default()) };
        for i in 0..5 {
            save(&root, &format!("k{i}"), &view);
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        prune(&root, 2);
        assert!(load(&root, "k4", 1).is_some() && load(&root, "k3", 1).is_some());
        assert!(load(&root, "k0", 1).is_none());
        let _ = std::fs::remove_dir_all(root);
    }
}

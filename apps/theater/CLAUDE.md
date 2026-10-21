# Theater — notes for working on this app

- GTK4 (gtk4-rs 0.11, `v4_18`) + Rust, no libadwaita, built to `STYLE.md`. Providers are
  `crates/nixe-plex` and `crates/nixe-jellyfin` over `crates/nixe-media-model`; caches are
  `crates/nixe-media-cache`; libmpv and the GL renderer are `crates/nixe-playback` (shared
  with Media Player); posters and cards are `nixe-ui::media`.
- `plex.rs`, `jellyfin.rs`: each provider's state (client, `servers::Catalog`, generation)
  lives on the GTK thread; refreshes run on a thread and send events back over a bounded
  channel, checked against the generation. Each library is fetched on its own: one that
  fails (other than sign-in or offline) is reported (`Skipped`) and keeps its cached titles.
  `servers::MAX_TITLES` is a safety stop per kind.
- `sync.rs`: incremental sync. `start()` syncs incrementally, `refresh()` fully. A library
  with `SyncMarks` (cache table `library_sync`: last sync, last full sync, the server's
  counts per kind) asks for titles changed since (Plex `updatedAt>>`/`lastViewedAt>>`,
  Jellyfin `minDateLastSaved`/`…ForUser`), lays them over `Cache::snapshot_items`, and
  falls back to a full fetch when the counts show a removal or a day has passed.
- Plex re-discovery: the account token from signing in is kept (`credentials::*_account`);
  when the saved address is offline or its token is refused, `nixe_plex::rediscover`
  finds the server by machine id and the new address replaces the old one.
- `servers.rs`: routing by `ServerId`, so a credential is never used on another server.
  `Catalog` keeps an id → position index (`position()` checks it; `items` is also set
  directly). Watch overlays carry revisions (`servers::revision`) so a late server reply
  never overwrites newer local progress.
- `catalog.rs`: the worker that prepares grids, Home/Discover and search (latest-only
  mailbox, 250 ms coalescing). `library::schedule` hands it `servers::items()` only when
  the catalog is dirty; watch progress marks it dirty at most every 30 s
  (`WATCH_REFRESH_SECS`).
- `view_cache.rs`: prepared page snapshots, shown before live data; the newest 48 are kept.
- `artwork.rs`: two disk readers, three network workers, JPEG (PNG with alpha) under
  `~/.cache/nixe-theater/artwork/*.img`, 512 MiB LRU trim.
- `playback.rs`: Theater's policy over `nixe-playback`: Direct Play/conversion, multipart,
  resume, progress reports (a worker thread; the final report holds the app ≤ 2 s), early
  EOF reopened once, and on completion the playlist queue or `library::up_next` (the
  `window::show_up_next` countdown card).
- `reconcile.rs`: explicit cross-server watch-state copying; matching runs on a worker.
- `control.rs`: `nixe_core::control` for Nixe Assistant (`theater.*`). Ids are
  `serde_json` of `ItemId`. New tools also go in the Assistant's
  `abilities/nixe-apps/{main.py,plugin.toml}` and `assistant-policy`.
- Credentials: `secret-tool` through stdin on workers with an 8 s deadline; never logged.
- Checks: `cargo test -p nixe-theater -p nixe-plex -p nixe-jellyfin -p nixe-media-model
  -p nixe-media-cache`, and clippy `--all-targets -- -D warnings` on the same.
- Not yet checked against live servers: conversion, bitmap subtitle timing, multipart
  transitions, reconciliation, up-next, the early-EOF reopen, incremental sync (the
  filters and counts are fixture-tested only) and Plex re-discovery.

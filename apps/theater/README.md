# Theater

Your Plex and Jellyfin movies and shows in one GTK 4 window: browse them together,
play them, and pick up where you left off.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/design-nexus/nixe/main/install.sh | bash -s -- theater
```

This installs mpv, FFmpeg and libsecret if they're missing, builds with Cargo, and installs
`nixe-theater` to `~/.local/bin` with a launcher entry. To remove it, run the same line with
`uninstall.sh` in place of `install.sh`.

## Connecting

- **Plex:** sign in through Plex's web login; Theater finds your account's servers and
  prefers a local connection. An address and token can be entered by hand instead.
- **Jellyfin:** the server's address and your account.
- One server of each kind can be connected at once. Tokens are kept in the desktop keyring
  (Secret Service), never in settings or caches. Disconnect (in Settings, two clicks)
  removes the token.
- Cached titles stay browsable when a server is offline. A library that fails to refresh
  keeps its cached titles while the others update.

## Browsing

- **Home:** a backdrop hero with Play or Resume, then Continue Watching, Recently Added
  Movies and Recently Added TV (a season that arrived together shows as one card).
- **Discover:** unwatched movies, shows to start, older movies to rediscover, highly rated,
  short movies, unwatched 4K, recent releases and genre rows from your own libraries.
- **Movies, TV shows, Collections:** poster grids with sorting (title, date, ratings,
  runtime, play count, quality, shuffle and more), filters (watch state, genre, year,
  runtime, rating, certification, source and library, format, languages, edition, people,
  studio, collection, TV status) and saved views. Poster size is Small, Medium or Large.
- **Search** across both servers.
- Titles on both servers appear once when their TMDB, IMDb or TVDB ids match; each keeps
  its own editions, versions and watch state. Titles are never matched by name alone.
- **Details:** ratings, cast and crew with filmographies, related titles, trailers, the
  next unwatched episode, and source and codec information. Mark a title watched or
  unwatched (a show or season: all its episodes).
- Optional TMDB and MDBList tokens (Settings → Metadata) add people, recommendations and
  IMDb, Rotten Tomatoes and Metacritic ratings.

## Playing

- Direct Play, or conversion to 1080p when a file can't play directly (Automatic tries
  Direct Play, then conversion, then other copies of the same edition).
- Resume a few seconds before where you stopped; progress goes back to the server.
- Audio and subtitle tracks (Jellyfin text subtitles, or burned-in bitmap subtitles),
  speed, chapters and full screen.
- When an episode ends, the next one plays after a 10-second countdown that can be skipped
  or stopped (Settings → Theater window → Play the next episode).
- If a stream drops partway, it's reopened where it stopped.
- **Playlists:** your server playlists (read-only) and Theater's own lists, which can mix
  both servers, repeat entries and be reordered.
- Local video files open with Ctrl+O, from the command line, or from the file manager.

## Watch state

Each server keeps its own watch state. Settings → Servers → Watch-state reconciliation can
show titles whose state differs between the servers, and copy one server's state to the
other for the titles you choose.

## Usage

```
nixe-theater [--section home|discover|movies|tv|collections|playlists|settings] [--toggle] [FILE|URI]
```

| Key | Action |
| --- | --- |
| Ctrl+O | Open a video file |
| Space | Play or pause |
| ← → | Seek 5 seconds |
| F | Full screen |
| M | Mute |
| Ctrl+F | Search |
| Alt+← (or a two-finger swipe right) | Back |
| Esc | Clear search, close settings, leave full screen |
| Ctrl+B | Collapse the sidebar |
| Ctrl+, | Settings |
| Ctrl+Shift+P | Command palette |
| F1 | Keyboard shortcuts |
| Ctrl+Q | Close |

Nixe Assistant works Theater over the session bus: `theater.search`,
`theater.continue_watching`, `theater.now_playing`, `theater.play`, `theater.control` and
`theater.mark_watched`.

## Files

- Settings: `~/.config/nixe-theater/settings.toml`
- Playlists, personal ratings and saved views: `~/.local/share/nixe-theater/`
- Library, metadata and artwork caches (artwork up to 512 MiB): `~/.cache/nixe-theater/`

API references: [Plex](https://developer.plex.tv/pms/), [Jellyfin](https://api.jellyfin.org/),
[libmpv](https://mpv.io/manual/stable/).

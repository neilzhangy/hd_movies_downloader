# hd-movies 3.0

`hd-movies` is a Rust service intended to run in the same TrueNAS/FreeNAS jail as Transmission. It polls movie feeds, remembers releases in SQLite, queues new magnet/torrent URLs through Transmission RPC, and tidies completed jobs into a consistent local library layout.

Subtitle searching and downloading are gone. The organizer only keeps subtitle files that already arrived with a torrent.

```text
src/
  app.rs           service lifecycle and queue orchestration
  cli.rs           command-line and environment configuration
  feed.rs          TPB fetch, parser, title normalization, and size extraction
  filter.rs        size/year/4K/IMDb eligibility rules and rating lookup
  db.rs            SQLite state and legacy migration
  transmission.rs  unauthenticated Transmission RPC client
  organizer.rs     local completed-download normalization
  models.rs        shared data types
  main.rs          thin executable entry point
```

See [DESIGN.md](DESIGN.md) for the operational design and [CHANGELOG.md](CHANGELOG.md) for the change history.

## What it keeps and removes

For each managed, completed torrent, the service selects the largest `.mkv`, `.mp4`, or `.avi` file at least 500 MiB by default. It also keeps existing `.srt`, `.ass`, `.ssa`, `.sub`, and `.vtt` files. Everything else in that managed download folder is discarded after a successful move.

For a release called `Example Movie 2026 2160p`, the final layout is:

```text
<library>/Example Movie 2026 2160p/
  Example Movie 2026 2160p.mkv
  Example Movie 2026 2160p.en.srt
```

No subtitle site is contacted, no subtitle archive is downloaded, and no `web_data` file is created.

## Download filter

Before a TPB candidate is recorded in SQLite or sent to Transmission, all of these conditions must pass:

- Its advertised torrent size is strictly greater than 500 MiB.
- Its normalized release name contains this calendar year or the previous calendar year.
- Its normalized release name contains `4K` or `2160p`.
- An exact IMDb movie-title and year match has an IMDb score strictly greater than 6.0.

The title lookup uses IMDb's public suggestion endpoint to avoid guessing an IMDb ID, then retrieves the score by that ID from a public metadata endpoint. An ambiguous title, a missing rating, or a lookup failure is rejected rather than queued. `--year` can replace the default two-year window when needed.

Eligible TPB variants are then grouped by their exact IMDb movie ID. Only one torrent is retained for each movie: known dead swarms lose to live ones, then Dolby Vision (`DV`, `DoVi`, or `Dolby Vision`) is preferred, followed by source tier (`REMUX`, BluRay, WEB-DL, WEBRip), HDR, codec, seeders, leechers, and size. This means a Dolby Vision 4K variant is chosen ahead of an otherwise better non-Dolby variant, while a reported zero-seeder torrent is never preferred over a live swarm. The selected row stores its canonical title/year key, IMDb ID, and IMDb rating, so later scans do not queue a different TPB filename for the same film.

## Build and install

Build on the target FreeBSD version and architecture when possible:

```sh
pkg install rust
cd /path/to/hd
cargo build --release --locked
install -d -m 755 /var/db/hd-movies
install -m 755 target/release/hd-movies /usr/local/sbin/hd-movies
```

The runnable release binary is `target/release/hd-movies`. SQLite is bundled and HTTPS uses Rustls, so the deployed binary does not need Python, `transmissionrpc`, OpenSSL, or a system SQLite library.

For a Linux development host, this checkout also retains the already-provisioned FreeBSD 13.1 amd64 sysroot and linker support in `.freebsd13-build/`. That directory is intentionally Git-ignored, so repeated cross-builds do not download or discard the FreeBSD base archive. With Zig available on `PATH`, build the deployable TrueNAS CORE 13 binary with:

```sh
./scripts/build-freebsd13.sh
```

It produces `target/x86_64-unknown-freebsd/release/hd-movies`. The local cache is specific to this checkout path; if the project is moved, re-create or update its two local linker configuration files before using it.

## Configuration

Create `/usr/local/etc/hd-movies.env` in the Transmission jail:

```sh
HD_MOVIES_DB=/var/db/hd-movies/movies.db
HD_MOVIES_INTERVAL_SECONDS=21600

# Transmission is local to this jail. The legacy port remains the default.
HD_MOVIES_TRANSMISSION_IP=127.0.0.1
HD_MOVIES_TRANSMISSION_PORT=9999

# Optional: route only TPB, IMDb, and rating-metadata requests through this proxy.
# HTTP(S) proxies and socks5:// URLs are supported. Transmission stays direct.
# HD_MOVIES_PROXY=socks5://127.0.0.1:1080

# Both paths must exist inside this jail and must be separate.
HD_MOVIES_DOWNLOAD_DIR=/mnt/downloads/movies
HD_MOVIES_LIBRARY_DIR=/mnt/media/movies

# Optional; defaults to 500.
# HD_MOVIES_MINIMUM_MOVIE_SIZE_MIB=500

# Download eligibility (both comparisons are strict: >, not >=).
# HD_MOVIES_MINIMUM_TORRENT_SIZE_MIB=500
# HD_MOVIES_MINIMUM_IMDB_SCORE=6

# Optional comma-separated replacement feed list.
# HD_MOVIES_SOURCE='https://tpb.party/top/207,https://tpb.party/browse/207/1/7/0'
```

`HD_MOVIES_PROXY` is used exclusively for the service's remote TPB, IMDb, and rating-metadata HTTP requests. It does not configure Transmission RPC, tracker requests, peer traffic, or the actual torrent download; those remain direct under Transmission's own configuration. The proxy URL must include its scheme, for example `http://127.0.0.1:7890` or `socks5://127.0.0.1:1080`.

The Transmission client uses the standard unauthenticated endpoint `http://IP:PORT/transmission/rpc` directly, even when a proxy is configured. Verify it before enabling the service:

```sh
/usr/local/sbin/hd-movies --check-transmission
```

This calls only Transmission's read-only `session-get` method and creates neither a database nor a torrent. A successful response prints the configured download directory. If the Transmission web UI is reachable at the same IP and port, this is normally the correct RPC configuration.

`HD_MOVIES_DOWNLOAD_DIR` must be the same local parent directory Transmission uses. New torrents receive a normalized child directory beneath it, which makes cleanup safe. `HD_MOVIES_LIBRARY_DIR` must be elsewhere on the filesystem; it is the destination for finalized movie folders.

## First migration

To retain the Python scanner's history, copy its `movies.db` into `/var/db/hd-movies/movies.db`. Version 3.0 automatically adds its state columns to the legacy `MOVIES(name, url)` table and marks existing rows as already queued, preventing historical releases from being added again. The current schema also records a best-effort canonical title/year key for old rows; this prevents a later selected TPB variant of the same recognizable movie from being queued again. Newly selected rows additionally retain their exact IMDb ID and rating.

If no old database is available, establish a baseline once:

```sh
/usr/local/sbin/hd-movies --first-run
```

This stores current eligible releases as `baseline` and performs neither torrent submission nor completed-file cleanup. Stop the old Python job before enabling v3.0.

## Run as a service

Install the supplied rc.d wrapper and enable it:

```sh
install -m 755 packaging/freebsd/hd_movies /usr/local/etc/rc.d/hd_movies
sysrc hd_movies_enable=YES
service hd_movies start
service hd_movies status
```

The service scans immediately at startup and then every six hours by default. Its output is written to `/var/log/hd-movies.log`. Use `HD_MOVIES_INTERVAL_SECONDS` in the environment file to change the interval.

For one interactive cycle:

```sh
/usr/local/sbin/hd-movies --once
```

Other useful commands:

```sh
# Scan and update SQLite without RPC submission or file organization.
/usr/local/sbin/hd-movies --once --no-transmission

# Print status, first-seen time, attempts, queue time, last error, IMDb ID/rating, title, and URL.
/usr/local/sbin/hd-movies --print-db

# Export pending rows in the old alternating title/URL text layout.
/usr/local/sbin/hd-movies --once --no-transmission --queue-file /var/db/hd-movies/pending.txt
```

Run `hd-movies --help` for all flags. `--source` may be repeated, and `--year` overrides the default current/previous-year filter. The release-name resolution requirement is fixed at `4K` or `2160p`; adjust the size and rating thresholds with their dedicated flags or environment variables.

## Safety rules for completed downloads

Organization is enabled only when `HD_MOVIES_LIBRARY_DIR` or `--library-dir` is set. The service only removes a completed source directory when all of these are true:

1. Transmission reports it complete (`seeding`, or a fully complete stopped torrent).
2. Its download directory is a direct child of the configured download root.
3. A qualifying movie was moved successfully, followed by any existing subtitle files.
4. Transmission accepted removal of the torrent record.

The destination folder is never overwritten. A collision leaves the original torrent and files untouched for manual review. A cross-filesystem move uses one short-lived `.part` file for an atomic final rename; normal same-filesystem moves create no temporary files.

## Testing

```sh
cargo test
```

The live parser, IMDb-rating, and TPB/IMDb/SQLite deduplication checks are deliberately opt-in because they require external services:

```sh
cargo test -- --ignored
```

If TPB or IMDb requires a proxy from the build host, use the same explicit setting as the service without exposing it in the test output:

```sh
HD_MOVIES_PROXY=socks5://127.0.0.1:1080 cargo test -- --ignored
```

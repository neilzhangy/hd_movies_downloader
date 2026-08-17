use std::path::PathBuf;

use clap::{ArgAction, Parser};

#[derive(Debug, Parser)]
#[command(
    name = "hd-movies",
    version,
    about = "Scan movie feeds, remember releases in SQLite, and enqueue new torrents in Transmission"
)]
pub struct Cli {
    /// SQLite database used as the durable release and queue state.
    #[arg(long, env = "HD_MOVIES_DB", default_value = "movies.db")]
    pub db: PathBuf,

    /// Feed URL to scan. Repeat the option for multiple feeds. Defaults to the legacy feeds.
    #[arg(long = "source", env = "HD_MOVIES_SOURCE", value_delimiter = ',')]
    pub sources: Vec<String>,

    /// Record matching releases as the initial baseline, without queueing them.
    #[arg(long, conflicts_with = "print_db")]
    pub first_run: bool,

    /// List releases already stored in SQLite, then exit without fetching feeds.
    #[arg(short = 'p', long, conflicts_with = "first_run")]
    pub print_db: bool,

    /// Validate the read-only Transmission RPC session-get call, then exit.
    #[arg(long, conflicts_with_all = ["first_run", "print_db", "once", "no_transmission"])]
    pub check_transmission: bool,

    /// Scan once and exit. Without this option the program runs as a polling service.
    #[arg(long)]
    pub once: bool,

    /// Seconds between scans while running as a service.
    #[arg(long, env = "HD_MOVIES_INTERVAL_SECONDS", default_value_t = 21_600)]
    pub interval_seconds: u64,

    /// IP address of Transmission. The default is the local jail.
    #[arg(long, env = "HD_MOVIES_TRANSMISSION_IP", default_value = "127.0.0.1")]
    pub transmission_ip: String,

    /// RPC port of Transmission.
    #[arg(long, env = "HD_MOVIES_TRANSMISSION_PORT", default_value_t = 9999)]
    pub transmission_port: u16,

    /// Existing Transmission download directory on this jail. If omitted, the configured session
    /// download directory is used. New releases receive a child directory beneath it.
    #[arg(long, env = "HD_MOVIES_DOWNLOAD_DIR")]
    pub download_dir: Option<PathBuf>,

    /// Library directory on this jail for normalized completed movie folders and files. Supplying
    /// this enables completed-download organization.
    #[arg(long, env = "HD_MOVIES_LIBRARY_DIR")]
    pub library_dir: Option<PathBuf>,

    /// Minimum video size eligible for completed-download organization.
    #[arg(long, env = "HD_MOVIES_MINIMUM_MOVIE_SIZE_MIB", default_value_t = 500)]
    pub minimum_movie_size_mib: u64,

    /// Minimum advertised torrent size required before a movie is considered for download.
    /// The torrent must be strictly larger than this value.
    #[arg(
        long,
        env = "HD_MOVIES_MINIMUM_TORRENT_SIZE_MIB",
        default_value_t = 500
    )]
    pub minimum_torrent_size_mib: u64,

    /// IMDb rating threshold. A movie must have a score strictly greater than this value.
    #[arg(long, env = "HD_MOVIES_MINIMUM_IMDB_SCORE", default_value_t = 6.0)]
    pub minimum_imdb_score: f64,

    /// Do not contact Transmission; retain all new releases as pending in SQLite.
    #[arg(long)]
    pub no_transmission: bool,

    /// Optional compatibility export of pending items as alternating title and torrent URL lines.
    /// SQLite remains the authoritative queue; no queue file is created unless this is specified.
    #[arg(long, env = "HD_MOVIES_QUEUE_FILE", value_name = "FILE")]
    pub queue_file: Option<PathBuf>,

    /// Accept a specific release year. Repeat for more than one. Defaults to this year and last year.
    #[arg(long = "year", value_delimiter = ',')]
    pub years: Vec<i32>,

    /// Accept invalid HTTPS certificates when fetching feeds.
    #[arg(long)]
    pub insecure_tls: bool,

    /// Show detailed progress and recoverable feed or queue errors.
    #[arg(short, long, action = ArgAction::SetTrue)]
    pub verbose: bool,
}

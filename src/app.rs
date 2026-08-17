use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use rusqlite::Connection;

use crate::cli::Cli;
use crate::db::{
    export_queue, mark_queued, open_database, pending_releases, print_database, record_queue_error,
    record_releases,
};
use crate::feed::{effective_sources, normalise_name, scan_sources};
use crate::filter::{FilterConfig, MovieFilter};
use crate::http::build_http_client;
use crate::models::Release;
use crate::organizer::organize_completed;
use crate::transmission::{transmission_endpoint, TransmissionClient};

pub fn run(cli: Cli) -> Result<()> {
    if cli.check_transmission {
        return check_transmission(&cli);
    }
    if cli.print_db {
        let database = open_database(&cli.db)?;
        print_database(&database)?;
        return Ok(());
    }

    // Baseline setup is intentionally a one-shot operation even if --once was omitted.
    if cli.once || cli.first_run {
        return scan_once(&cli);
    }
    if cli.interval_seconds == 0 {
        bail!("--interval-seconds must be greater than zero in service mode");
    }

    println!(
        "hd-movies 3.0 service started; scanning every {} second(s)",
        cli.interval_seconds
    );
    loop {
        if let Err(error) = scan_once(&cli) {
            eprintln!("warning: scan failed: {error:#}");
        }
        thread::sleep(Duration::from_secs(cli.interval_seconds));
    }
}

fn check_transmission(cli: &Cli) -> Result<()> {
    let endpoint = transmission_endpoint(&cli.transmission_ip, cli.transmission_port);
    let mut transmission = TransmissionClient::new(build_http_client(false)?, endpoint.clone());
    let download_dir = transmission.session_download_dir()?;
    println!(
        "Transmission RPC is reachable at {endpoint}; download directory: {}",
        download_dir.display()
    );
    Ok(())
}

fn scan_once(cli: &Cli) -> Result<()> {
    let mut transmission_error = None;
    let mut transport = if cli.no_transmission || cli.first_run {
        None
    } else {
        match prepare_transmission(cli) {
            Ok((mut transmission, download_root)) => {
                if let Some(library_dir) = &cli.library_dir {
                    match organize_completed(
                        &mut transmission,
                        &download_root,
                        library_dir,
                        cli.minimum_movie_size_mib,
                        cli.verbose,
                    ) {
                        Ok(organized) if organized > 0 || cli.verbose => {
                            println!("organized {organized} completed torrent(s)");
                        }
                        Ok(_) => {}
                        Err(error) => {
                            eprintln!("warning: could not organize completed downloads: {error:#}")
                        }
                    }
                }
                Some((transmission, download_root))
            }
            Err(error) => {
                eprintln!("warning: Transmission is unavailable: {error:#}");
                transmission_error = Some(error);
                None
            }
        }
    };

    let http_client = build_http_client(cli.insecure_tls)?;
    let sources = effective_sources(&cli.sources);
    let candidates = scan_sources(&http_client, &sources, cli.verbose)?;
    let filter = MovieFilter::new(
        FilterConfig::new(
            &cli.years,
            cli.minimum_torrent_size_mib,
            cli.minimum_imdb_score,
        )?,
        http_client.clone(),
    );
    let filter_outcome = filter.filter(&candidates, cli.verbose);
    let releases = filter_outcome.releases;
    println!(
        "scanned {} torrent candidate(s); {} passed filters ({} basic rejection(s), {} IMDb-score rejection(s), {} IMDb lookup failure(s))",
        candidates.len(),
        releases.len(),
        filter_outcome.basic_rejections,
        filter_outcome.rating_rejections,
        filter_outcome.lookup_failures,
    );
    let mut database = open_database(&cli.db)?;

    if cli.first_run {
        let added = record_releases(&mut database, &releases, "baseline")?;
        export_queue(cli.queue_file.as_deref(), &[])?;
        println!(
            "initial baseline complete: recorded {} new filtered release(s); no torrents were queued",
            added
        );
        return Ok(());
    }

    let added = record_releases(&mut database, &releases, "pending")?;
    let pending_before_delivery = pending_releases(&database)?;
    export_queue(cli.queue_file.as_deref(), &pending_before_delivery)?;
    println!(
        "recorded {} new filtered release(s); {} pending queue item(s)",
        added,
        pending_before_delivery.len()
    );

    if cli.no_transmission {
        println!("Transmission was disabled; pending releases remain in SQLite");
        return Ok(());
    }
    let (mut transmission, download_root) = transport.take().ok_or_else(|| {
        transmission_error
            .take()
            .unwrap_or_else(|| anyhow!("Transmission setup did not complete"))
    })?;
    enqueue_pending(
        &mut transmission,
        &database,
        &pending_before_delivery,
        &download_root,
        cli.verbose,
    )?;

    let remaining = pending_releases(&database)?;
    export_queue(cli.queue_file.as_deref(), &remaining)?;
    println!("{} pending queue item(s) remain", remaining.len());
    Ok(())
}

fn prepare_transmission(cli: &Cli) -> Result<(TransmissionClient, PathBuf)> {
    let endpoint = transmission_endpoint(&cli.transmission_ip, cli.transmission_port);
    let mut transmission = TransmissionClient::new(build_http_client(false)?, endpoint);
    let download_root = match &cli.download_dir {
        Some(path) => path.clone(),
        None => transmission.session_download_dir()?,
    };
    Ok((transmission, download_root))
}

fn enqueue_pending(
    transmission: &mut TransmissionClient,
    database: &Connection,
    pending: &[Release],
    download_root: &Path,
    verbose: bool,
) -> Result<()> {
    if !download_root.is_dir() {
        bail!(
            "Transmission download directory is not available on this jail: {}",
            download_root.display()
        );
    }

    let mut queued = 0;
    for release in pending {
        let download_dir = download_root.join(safe_file_name(&release.name));
        let directory_existed = download_dir.exists();
        let queue_result = (|| -> Result<()> {
            fs::create_dir_all(&download_dir)
                .with_context(|| format!("create release directory {}", download_dir.display()))?;
            transmission.add_torrent(&release.url, &download_dir)
        })();
        match queue_result {
            Ok(()) => {
                mark_queued(database, &release.name)?;
                queued += 1;
                if verbose {
                    eprintln!("queued {}", release.name);
                }
            }
            Err(error) => {
                if !directory_existed {
                    let _ = fs::remove_dir(&download_dir);
                }
                record_queue_error(database, &release.name, &error.to_string())?;
                eprintln!("warning: could not queue {}: {error:#}", release.name);
            }
        }
    }
    println!("queued {queued} torrent(s) in Transmission");
    Ok(())
}

fn safe_file_name(name: &str) -> String {
    let value = normalise_name(name);
    if value.is_empty() {
        "untitled movie".to_owned()
    } else {
        value.chars().take(180).collect()
    }
}

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};

use crate::feed::normalise_name;
use crate::transmission::{CompletedTorrent, TransmissionClient};

const VIDEO_EXTENSIONS: [&str; 3] = ["mkv", "mp4", "avi"];
const SUBTITLE_EXTENSIONS: [&str; 5] = ["srt", "ass", "ssa", "sub", "vtt"];

#[derive(Debug)]
struct MediaSelection {
    movie: PathBuf,
    subtitles: Vec<PathBuf>,
}

#[derive(Debug)]
struct FileMove {
    source: PathBuf,
    destination: PathBuf,
}

pub fn organize_completed(
    transmission: &mut TransmissionClient,
    download_root: &Path,
    library_dir: &Path,
    minimum_movie_size_mib: u64,
    verbose: bool,
) -> Result<usize> {
    if !download_root.is_dir() {
        bail!(
            "Transmission download directory is not available on this jail: {}",
            download_root.display()
        );
    }
    fs::create_dir_all(library_dir)
        .with_context(|| format!("create library directory {}", library_dir.display()))?;
    let canonical_download_root = fs::canonicalize(download_root)
        .with_context(|| format!("resolve download directory {}", download_root.display()))?;
    let canonical_library_dir = fs::canonicalize(library_dir)
        .with_context(|| format!("resolve library directory {}", library_dir.display()))?;
    if canonical_library_dir.starts_with(&canonical_download_root)
        || canonical_download_root.starts_with(&canonical_library_dir)
    {
        bail!(
            "library directory {} must be separate from managed download directory {}",
            library_dir.display(),
            download_root.display()
        );
    }

    let minimum_size = minimum_movie_size_mib.saturating_mul(1024 * 1024);
    let mut organized = 0;
    for torrent in transmission.completed_torrents()? {
        let job_dir = match managed_job_directory(&torrent.download_dir, &canonical_download_root) {
            Ok(job_dir) => job_dir,
            Err(error) => {
                if verbose {
                    eprintln!("skipping completed torrent {}: {error:#}", torrent.name);
                }
                continue;
            }
        };
        match organize_one_completed_torrent(
            transmission,
            &torrent,
            &job_dir,
            &canonical_library_dir,
            minimum_size,
        ) {
            Ok(true) => {
                organized += 1;
                if verbose {
                    eprintln!("organized completed torrent {}", torrent.name);
                }
            }
            Ok(false) => {
                if verbose {
                    eprintln!(
                        "no qualifying movie found for completed torrent {}",
                        torrent.name
                    );
                }
            }
            Err(error) => eprintln!(
                "warning: could not organize completed torrent {}: {error:#}",
                torrent.name
            ),
        }
    }
    Ok(organized)
}

fn managed_job_directory(download_dir: &Path, canonical_download_root: &Path) -> Result<PathBuf> {
    let job_dir = fs::canonicalize(download_dir)
        .with_context(|| format!("resolve completed directory {}", download_dir.display()))?;
    if job_dir == canonical_download_root || job_dir.parent() != Some(canonical_download_root) {
        bail!(
            "{} is not a direct child of the managed download directory {}",
            download_dir.display(),
            canonical_download_root.display()
        );
    }
    Ok(job_dir)
}

fn organize_one_completed_torrent(
    transmission: &mut TransmissionClient,
    torrent: &CompletedTorrent,
    job_dir: &Path,
    library_dir: &Path,
    minimum_size: u64,
) -> Result<bool> {
    let Some(selection) = select_media_files(job_dir, minimum_size)? else {
        return Ok(false);
    };
    let title_source = job_dir
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or(&torrent.name);
    let title = safe_file_name(title_source);
    let destination_dir = library_dir.join(&title);
    if destination_dir.exists() {
        bail!(
            "destination folder already exists and was left untouched: {}",
            destination_dir.display()
        );
    }

    fs::create_dir_all(&destination_dir)
        .with_context(|| format!("create destination folder {}", destination_dir.display()))?;
    let moves = build_file_moves(&selection, &destination_dir, &title)?;
    if let Err(error) = execute_file_moves(&moves) {
        let _ = fs::remove_dir_all(&destination_dir);
        return Err(error).context("move selected media files");
    }

    if let Err(error) = transmission.remove_torrent(torrent.id) {
        rollback_file_moves(&moves);
        let _ = fs::remove_dir_all(&destination_dir);
        return Err(error).context("remove organized torrent from Transmission");
    }
    fs::remove_dir_all(job_dir)
        .with_context(|| format!("remove original completed directory {}", job_dir.display()))?;
    Ok(true)
}

fn select_media_files(job_dir: &Path, minimum_size: u64) -> Result<Option<MediaSelection>> {
    let mut movies = Vec::new();
    let mut subtitles = Vec::new();
    collect_media_files(job_dir, minimum_size, &mut movies, &mut subtitles)?;
    let Some(movie) = movies.into_iter().max_by_key(|path| {
        fs::metadata(path)
            .map(|metadata| metadata.len())
            .unwrap_or_default()
    }) else {
        return Ok(None);
    };
    subtitles.sort();
    Ok(Some(MediaSelection { movie, subtitles }))
}

fn collect_media_files(
    directory: &Path,
    minimum_size: u64,
    movies: &mut Vec<PathBuf>,
    subtitles: &mut Vec<PathBuf>,
) -> Result<()> {
    for entry in fs::read_dir(directory)
        .with_context(|| format!("read completed directory {}", directory.display()))?
    {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let path = entry.path();
        if file_type.is_dir() {
            collect_media_files(&path, minimum_size, movies, subtitles)?;
        } else if file_type.is_file() {
            let extension = extension_of(&path);
            if is_extension(&extension, &VIDEO_EXTENSIONS)
                && entry.metadata()?.len() >= minimum_size
            {
                movies.push(path);
            } else if is_extension(&extension, &SUBTITLE_EXTENSIONS) {
                subtitles.push(path);
            }
        }
    }
    Ok(())
}

fn build_file_moves(
    selection: &MediaSelection,
    destination_dir: &Path,
    title: &str,
) -> Result<Vec<FileMove>> {
    let movie_extension = extension_of(&selection.movie);
    if movie_extension.is_empty() {
        bail!(
            "selected movie has no usable extension: {}",
            selection.movie.display()
        );
    }
    let movie_destination = destination_dir.join(format!("{title}.{movie_extension}"));
    let mut moves = vec![FileMove {
        source: selection.movie.clone(),
        destination: movie_destination.clone(),
    }];
    let mut used_destinations = HashSet::new();
    used_destinations.insert(movie_destination);

    for (index, source) in selection.subtitles.iter().enumerate() {
        let extension = extension_of(source);
        if extension.is_empty() {
            continue;
        }
        let label = subtitle_label(source, title, index);
        let mut suffix = 1;
        let destination = loop {
            let suffix_text = if suffix == 1 {
                label.clone()
            } else {
                format!("{label} {suffix}")
            };
            let candidate = destination_dir.join(format!("{title}.{suffix_text}.{extension}"));
            if !used_destinations.contains(&candidate) {
                break candidate;
            }
            suffix += 1;
        };
        used_destinations.insert(destination.clone());
        moves.push(FileMove {
            source: source.clone(),
            destination,
        });
    }
    Ok(moves)
}

fn subtitle_label(source: &Path, title: &str, index: usize) -> String {
    let source_stem = source
        .file_stem()
        .and_then(OsStr::to_str)
        .map(normalise_name)
        .unwrap_or_default();
    let source_words: Vec<_> = source_stem.split_whitespace().collect();
    let title_words: Vec<_> = title.split_whitespace().collect();
    let matching_prefix = source_words
        .iter()
        .zip(&title_words)
        .take_while(|(left, right)| left.eq_ignore_ascii_case(right))
        .count();
    let label = source_words[matching_prefix..].join(" ");
    if label.is_empty() {
        if index == 0 {
            "subtitle".to_owned()
        } else {
            format!("subtitle {}", index + 1)
        }
    } else {
        label
    }
}

fn extension_of(path: &Path) -> String {
    path.extension()
        .and_then(OsStr::to_str)
        .map(str::to_ascii_lowercase)
        .unwrap_or_default()
}

fn is_extension(extension: &str, expected: &[&str]) -> bool {
    expected
        .iter()
        .any(|known| extension.eq_ignore_ascii_case(known))
}

fn execute_file_moves(moves: &[FileMove]) -> Result<()> {
    let mut moved = Vec::new();
    for file_move in moves {
        if let Err(error) = move_across_filesystems(&file_move.source, &file_move.destination) {
            rollback_file_moves(&moved);
            return Err(error).with_context(|| {
                format!(
                    "move {} to {}",
                    file_move.source.display(),
                    file_move.destination.display()
                )
            });
        }
        moved.push(FileMove {
            source: file_move.source.clone(),
            destination: file_move.destination.clone(),
        });
    }
    Ok(())
}

fn rollback_file_moves(moves: &[FileMove]) {
    for file_move in moves.iter().rev() {
        if file_move.destination.exists() && !file_move.source.exists() {
            if let Err(error) = move_across_filesystems(&file_move.destination, &file_move.source) {
                eprintln!(
                    "warning: rollback failed for {}: {error}",
                    file_move.source.display()
                );
            }
        }
    }
}

fn move_across_filesystems(source: &Path, destination: &Path) -> Result<()> {
    match fs::rename(source, destination) {
        Ok(()) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {}
        Err(error) => {
            return Err(error).with_context(|| {
                format!("rename {} to {}", source.display(), destination.display())
            });
        }
    }

    let parent = destination
        .parent()
        .ok_or_else(|| anyhow!("destination has no parent: {}", destination.display()))?;
    let file_name = destination
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or("movie");
    let temporary = parent.join(format!(
        ".{file_name}.hd-movies-{}.part",
        std::process::id()
    ));
    let copied = (|| -> Result<()> {
        fs::copy(source, &temporary)
            .with_context(|| format!("copy {} to {}", source.display(), temporary.display()))?;
        fs::File::open(&temporary)?.sync_all()?;
        fs::rename(&temporary, destination)
            .with_context(|| format!("finalize copied file at {}", destination.display()))?;
        Ok(())
    })();
    if let Err(error) = copied {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    fs::remove_file(source).with_context(|| format!("remove moved source {}", source.display()))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn keeps_movie_and_subtitles_with_uniform_names() {
        let directory = tempdir().unwrap();
        let job_dir = directory.path().join("The.Movie.2026.1080p");
        let library_dir = directory.path().join("library");
        fs::create_dir_all(&job_dir).unwrap();
        fs::create_dir_all(&library_dir).unwrap();
        fs::write(job_dir.join("feature.mkv"), b"movie").unwrap();
        fs::write(job_dir.join("The.Movie.2026.1080p.en.srt"), b"subtitle").unwrap();
        fs::write(job_dir.join("notes.txt"), b"discard").unwrap();

        let selection = select_media_files(&job_dir, 1).unwrap().unwrap();
        let title = safe_file_name(job_dir.file_name().unwrap().to_str().unwrap());
        let destination = library_dir.join(&title);
        fs::create_dir_all(&destination).unwrap();
        let moves = build_file_moves(&selection, &destination, &title).unwrap();
        execute_file_moves(&moves).unwrap();

        assert!(destination.join("The Movie 2026 1080p.mkv").is_file());
        assert!(destination.join("The Movie 2026 1080p.en.srt").is_file());
        assert!(job_dir.join("notes.txt").is_file());
    }
}

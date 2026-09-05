use crate::config::Config;
use crate::logging;
use crate::manifest::{Entry, Manifest, hash_file, manifest_path};
use crate::scan::{Ignore, LocalFile, cartridge_directories, scan};
use crate::webdav::Dav;
use anyhow::{Context, Result};
use futures::stream::{self, StreamExt};
use indicatif::{ProgressBar, ProgressStyle};
use std::io::{Cursor, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use zip::CompressionMethod;
use zip::write::{SimpleFileOptions, ZipWriter};

const CHUNK_BYTES: u64 = 24 * 1024 * 1024;
const CHUNK_FILES: usize = 400;
const INLINE_PUT_FILES: usize = 8;
const INLINE_PUT_BYTES: u64 = 2 * 1024 * 1024;
const COMPRESSION_LEVEL: i64 = 1;

pub struct Ctx {
    pub config: Config,
    pub dav: Dav,
    pub ignore: Ignore,
    pub manifest_path: PathBuf,
    pub jobs: usize,
}

impl Ctx {
    pub fn new(config: Config, jobs: usize) -> Result<Ctx> {
        let dav = Dav::new(&config)?;
        let ignore = Ignore::load(&config);
        let manifest_path = manifest_path(&config);
        Ok(Ctx { config, dav, ignore, manifest_path, jobs })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PushOptions {
    pub full: bool,
    pub dry_run: bool,
    pub show_progress: bool,
}

#[derive(Debug, Default)]
pub struct Stats {
    pub uploaded: usize,
    pub deleted: usize,
    pub bytes: u64,
    pub elapsed: Duration,
}

pub async fn push(ctx: &Ctx, options: PushOptions) -> Result<Stats> {
    let started = Instant::now();
    let mut manifest = if options.full { Manifest::default() } else { Manifest::load(&ctx.manifest_path) };
    let files = scan(&ctx.config, &ctx.ignore)?;

    let changed = select_changed(&files, &manifest);
    let removed = if options.full { Vec::new() } else { select_removed(&files, &manifest) };

    if changed.is_empty() && removed.is_empty() {
        logging::ok(format!("{} already up to date", ctx.config.code_version));
        return Ok(Stats { elapsed: started.elapsed(), ..Stats::default() });
    }

    let bytes: u64 = changed.iter().map(|file| file.size).sum();
    logging::info(format!(
        "{} file(s) to upload ({}), {} to delete",
        changed.len(),
        human_bytes(bytes),
        removed.len()
    ));

    if options.dry_run {
        for file in changed.iter().take(50) {
            crate::out!("  + {}", file.relative);
        }
        for path in removed.iter().take(50) {
            crate::out!("  - {path}");
        }
        return Ok(Stats {
            uploaded: changed.len(),
            deleted: removed.len(),
            bytes,
            elapsed: started.elapsed(),
        });
    }

    ctx.dav.wait_until_ready(Some(Duration::from_secs(600))).await?;
    ctx.dav.mkcol(ctx.dav.base_url()).await?;

    if options.full {
        clear_remote_cartridges(ctx).await?;
    }

    let progress = build_progress(changed.len(), options.show_progress);
    let outcome = upload_files(ctx, changed, progress.as_ref()).await;
    if let Some(bar) = progress {
        bar.finish_and_clear();
    }

    let (recorded, failure) = match outcome {
        Ok(recorded) => (recorded, None),
        Err(error) => (Vec::new(), Some(error)),
    };
    let uploaded = recorded.len();
    for (relative, entry) in recorded {
        manifest.record(relative, entry);
    }

    let mut deleted = 0;
    if failure.is_none() && !removed.is_empty() {
        deleted = delete_paths(ctx, &removed).await?;
        for path in &removed {
            manifest.forget(path);
        }
    }

    manifest.save(&ctx.manifest_path)?;

    if let Some(error) = failure {
        return Err(error);
    }

    let stats = Stats { uploaded, deleted, bytes, elapsed: started.elapsed() };
    logging::ok(format!(
        "{} uploaded, {} deleted, {} in {:.1}s",
        stats.uploaded,
        stats.deleted,
        human_bytes(stats.bytes),
        stats.elapsed.as_secs_f64()
    ));
    Ok(stats)
}

pub fn select_changed(files: &[LocalFile], manifest: &Manifest) -> Vec<LocalFile> {
    files
        .iter()
        .filter(|file| !manifest.is_unchanged(&file.relative, file.size, file.modified_millis))
        .filter(|file| match hash_file(&file.absolute) {
            Ok(hash) => !manifest.matches_hash(&file.relative, hash),
            Err(_) => true,
        })
        .cloned()
        .collect()
}

pub fn select_removed(files: &[LocalFile], manifest: &Manifest) -> Vec<String> {
    let present: std::collections::HashSet<&str> =
        files.iter().map(|file| file.relative.as_str()).collect();
    manifest
        .files
        .keys()
        .filter(|key| !present.contains(key.as_str()))
        .cloned()
        .collect()
}

pub async fn upload_files(
    ctx: &Ctx,
    files: Vec<LocalFile>,
    progress: Option<&ProgressBar>,
) -> Result<Vec<(String, Entry)>> {
    if files.is_empty() {
        return Ok(Vec::new());
    }

    let total_bytes: u64 = files.iter().map(|file| file.size).sum();
    if files.len() <= INLINE_PUT_FILES && total_bytes <= INLINE_PUT_BYTES {
        return upload_individually(ctx, files, progress).await;
    }

    let chunks = split_into_chunks(files);
    let results = stream::iter(chunks.into_iter().enumerate())
        .map(|(index, chunk)| async move { upload_chunk(ctx, chunk, index).await })
        .buffer_unordered(ctx.jobs)
        .collect::<Vec<_>>()
        .await;

    let mut recorded = Vec::new();
    let mut failure = None;
    for result in results {
        match result {
            Ok(entries) => {
                if let Some(bar) = progress {
                    bar.inc(entries.len() as u64);
                }
                recorded.extend(entries);
            }
            Err(error) => failure = Some(error),
        }
    }

    match failure {
        Some(error) if recorded.is_empty() => Err(error),
        Some(error) => {
            logging::error(format!("{error:#}"));
            Ok(recorded)
        }
        None => Ok(recorded),
    }
}

pub async fn delete_paths(ctx: &Ctx, paths: &[String]) -> Result<usize> {
    let roots = outermost(paths);
    let deleted = stream::iter(roots.iter())
        .map(|path| async move {
            match ctx.dav.delete(path).await {
                Ok(true) => {
                    logging::removal(path);
                    1
                }
                Ok(false) => 0,
                Err(error) => {
                    logging::error(format!("{error:#}"));
                    0
                }
            }
        })
        .buffer_unordered(ctx.jobs)
        .collect::<Vec<_>>()
        .await;
    Ok(deleted.into_iter().sum())
}

fn outermost(paths: &[String]) -> Vec<String> {
    let mut sorted: Vec<String> = paths.to_vec();
    sorted.sort();

    let mut roots: Vec<String> = Vec::with_capacity(sorted.len());
    for path in sorted {
        let nested = roots.last().is_some_and(|root| path.starts_with(&format!("{root}/")));
        if !nested {
            roots.push(path);
        }
    }
    roots
}

async fn upload_individually(
    ctx: &Ctx,
    files: Vec<LocalFile>,
    progress: Option<&ProgressBar>,
) -> Result<Vec<(String, Entry)>> {
    let mut recorded = Vec::new();
    for file in files {
        let body = std::fs::read(&file.absolute)
            .with_context(|| format!("cannot read {}", file.absolute.display()))?;
        let hash = hash_file(&file.absolute)?;

        if let Some((parent, _)) = file.relative.rsplit_once('/') {
            ctx.dav.ensure_directory(parent).await?;
        }
        ctx.dav.put(&file.relative, body).await?;
        if let Some(bar) = progress {
            bar.inc(1);
        }
        recorded.push((
            file.relative,
            Entry { hash, size: file.size, modified_millis: file.modified_millis },
        ));
    }
    Ok(recorded)
}

async fn upload_chunk(ctx: &Ctx, chunk: Vec<LocalFile>, index: usize) -> Result<Vec<(String, Entry)>> {
    let (archive, recorded) = tokio::task::spawn_blocking(move || build_archive(chunk))
        .await
        .context("the archive task panicked")??;

    let name = format!("prost-{}-{index}.zip", std::process::id());
    ctx.dav.put(&name, archive).await?;
    ctx.dav.unzip(&name).await?;
    if let Err(error) = ctx.dav.delete(&name).await {
        logging::warn(format!("could not remove the temporary archive {name}: {error:#}"));
    }
    Ok(recorded)
}

fn build_archive(files: Vec<LocalFile>) -> Result<(Vec<u8>, Vec<(String, Entry)>)> {
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .compression_level(Some(COMPRESSION_LEVEL))
        .large_file(false);

    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let mut recorded = Vec::with_capacity(files.len());

    for file in files {
        let contents = std::fs::read(&file.absolute)
            .with_context(|| format!("cannot read {}", file.absolute.display()))?;
        let hash = xxhash_rust::xxh3::xxh3_64(&contents);

        writer
            .start_file(&file.relative, options)
            .with_context(|| format!("cannot add {} to the archive", file.relative))?;
        writer
            .write_all(&contents)
            .with_context(|| format!("cannot write {} into the archive", file.relative))?;

        recorded.push((
            file.relative,
            Entry { hash, size: file.size, modified_millis: file.modified_millis },
        ));
    }

    let archive = writer.finish().context("cannot close the archive")?.into_inner();
    Ok((archive, recorded))
}

fn split_into_chunks(files: Vec<LocalFile>) -> Vec<Vec<LocalFile>> {
    let mut chunks = Vec::new();
    let mut current = Vec::new();
    let mut current_bytes = 0_u64;

    for file in files {
        current_bytes += file.size;
        current.push(file);
        if current_bytes >= CHUNK_BYTES || current.len() >= CHUNK_FILES {
            chunks.push(std::mem::take(&mut current));
            current_bytes = 0;
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

async fn clear_remote_cartridges(ctx: &Ctx) -> Result<()> {
    let names: Vec<String> = cartridge_directories(&ctx.config)?
        .iter()
        .filter_map(|path| path.file_name().map(|name| name.to_string_lossy().into_owned()))
        .collect();

    logging::info(format!("clearing {} cartridge folder(s) on the sandbox", names.len()));
    delete_paths(ctx, &names).await?;
    Ok(())
}

fn build_progress(total: usize, enabled: bool) -> Option<ProgressBar> {
    if !enabled {
        return None;
    }
    let bar = ProgressBar::new(total as u64);
    if let Ok(style) = ProgressStyle::with_template("  {bar:32} {pos}/{len} files {elapsed_precise}") {
        bar.set_style(style.progress_chars("=> "));
    }
    Some(bar)
}

pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_of(size: u64) -> LocalFile {
        LocalFile {
            relative: format!("cartridge/file-{size}.js"),
            absolute: PathBuf::from("."),
            size,
            modified_millis: 0,
        }
    }

    #[test]
    fn deleting_a_folder_does_not_also_delete_its_contents() {
        let paths = vec![
            "app/cartridge/tmp/a.js".to_string(),
            "app/cartridge/tmp".to_string(),
            "app/cartridge/tmp/deep/b.js".to_string(),
            "app/cartridge/tmpx/c.js".to_string(),
            "other/file.js".to_string(),
        ];
        assert_eq!(
            outermost(&paths),
            vec!["app/cartridge/tmp", "app/cartridge/tmpx/c.js", "other/file.js"]
        );
    }

    #[test]
    fn formats_sizes_for_humans() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn closes_a_chunk_when_the_byte_budget_is_reached() {
        let chunks = split_into_chunks(vec![file_of(CHUNK_BYTES), file_of(10), file_of(20)]);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 1);
        assert_eq!(chunks[1].len(), 2);
    }

    #[test]
    fn closes_a_chunk_when_the_file_budget_is_reached() {
        let files: Vec<LocalFile> = (0..CHUNK_FILES + 3).map(|index| file_of(index as u64)).collect();
        let chunks = split_into_chunks(files);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), CHUNK_FILES);
    }
}

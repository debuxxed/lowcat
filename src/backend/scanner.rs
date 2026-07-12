use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io;
use std::path::Path;
use std::time::UNIX_EPOCH;

use lofty::config::ParseOptions;
use lofty::file::AudioFile;
use lofty::flac::FlacFile;
use lofty::ogg::{OpusFile, VorbisComments};

use crate::db::FileScanRecord;
use crate::model::{Category, supported_audio_extension};

#[derive(Default)]
pub(super) struct CategoryScan {
    pub(super) records: Vec<FileScanRecord>,
    pub(super) files_seen: usize,
    pub(super) dirs_seen: usize,
    pub(super) skipped_seen: usize,
    pub(super) reused_seen: usize,
    pub(super) tags_read: usize,
}

pub(super) fn scan_category_folder(
    category: Category,
    folder: &Path,
    fingerprints: &BTreeMap<String, (u64, i64)>,
) -> io::Result<CategoryScan> {
    let mut scan = CategoryScan::default();
    scan_category_dir(category, folder, fingerprints, &mut scan)?;
    Ok(scan)
}

fn scan_category_dir(
    category: Category,
    folder: &Path,
    fingerprints: &BTreeMap<String, (u64, i64)>,
    scan: &mut CategoryScan,
) -> io::Result<()> {
    for entry in fs::read_dir(folder)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let path = entry.path();

        if file_type.is_dir() {
            scan.dirs_seen += 1;
            scan_category_dir(category, &path, fingerprints, scan)?;
            continue;
        }

        if !file_type.is_file() {
            continue;
        }
        scan.files_seen += 1;

        if !is_library_file(&path) {
            scan.skipped_seen += 1;
            continue;
        }

        if let Ok(record) = read_scan_record_cached(category, &path, fingerprints, scan) {
            scan.records.push(record);
        }
    }

    Ok(())
}

fn canonical_category_key(category: Category, key: &str) -> Option<&'static str> {
    let key = crate::model::canonical_tag_key(key)?;
    category.tag_keys().contains(&key).then_some(key)
}

pub(super) fn is_library_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .and_then(supported_audio_extension)
        .is_some()
}

fn read_scan_record_cached(
    category: Category,
    path: &Path,
    fingerprints: &BTreeMap<String, (u64, i64)>,
    scan: &mut CategoryScan,
) -> io::Result<FileScanRecord> {
    let metadata = fs::metadata(path)?;
    let size = metadata.len();
    let modified = modified_secs(&metadata);
    let path_key = path.to_string_lossy().to_string();
    let unchanged = fingerprints
        .get(&path_key)
        .is_some_and(|(cached_size, cached_modified)| {
            *cached_size == size && *cached_modified == modified
        });

    let tags = if unchanged {
        scan.reused_seen += 1;
        BTreeMap::new()
    } else {
        scan.tags_read += 1;
        read_tags(category, path).unwrap_or_default()
    };

    Ok(FileScanRecord {
        path: path.to_path_buf(),
        stem: file_stem(path),
        extension: extension(path).unwrap_or_default(),
        size,
        modified,
        tags,
    })
}

fn modified_secs(metadata: &fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

pub(super) fn file_stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default()
        .to_string()
}

fn read_tags(category: Category, path: &Path) -> io::Result<BTreeMap<String, Vec<String>>> {
    let mut tags = BTreeMap::new();
    match extension(path).as_deref() {
        Some("opus") => {
            let mut file = File::open(path)?;
            let opus = OpusFile::read_from(&mut file, ParseOptions::new()).map_err(lofty_error)?;
            collect_vorbis_tags(category, opus.vorbis_comments(), &mut tags);
        }
        Some("flac") => {
            let mut file = File::open(path)?;
            let flac = FlacFile::read_from(&mut file, ParseOptions::new()).map_err(lofty_error)?;
            if let Some(comments) = flac.vorbis_comments() {
                collect_vorbis_tags(category, comments, &mut tags);
            }
        }
        _ => {}
    }
    Ok(tags)
}

fn collect_vorbis_tags(
    category: Category,
    comments: &VorbisComments,
    tags: &mut BTreeMap<String, Vec<String>>,
) {
    for (key, value) in comments.items() {
        let Some(key) = canonical_category_key(category, key) else {
            continue;
        };
        let values = tags.entry(key.to_string()).or_default();
        push_unique(values, value);
    }
}

fn push_unique(values: &mut Vec<String>, value: &str) {
    if !values.iter().any(|existing| existing == value) {
        values.push(value.to_string());
    }
}

pub(super) fn extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_lowercase())
}

fn lofty_error(error: lofty::error::LoftyError) -> io::Error {
    io::Error::other(error)
}

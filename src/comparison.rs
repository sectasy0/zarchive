//! Compares archive entry trees and the metadata available from inspection.

use crate::inspection::{self, EntryInfo, Integrity, Report};
use sha2::{Digest, Sha256, Sha512};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{BufReader, Read},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum DifferenceKind {
    OnlyInA,
    OnlyInB,
    Common,
    ContentChanged,
    MetadataChanged,
    EntryTypeChanged,
    Moved,
    Renamed,
    MovedAndRenamed,
    DirectoryOnlyInA,
    DirectoryOnlyInB,
    EmptyDirectoryOnlyInA,
    EmptyDirectoryOnlyInB,
}

impl DifferenceKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::OnlyInA => "Only in archive A",
            Self::OnlyInB => "Only in archive B",
            Self::Common => "Common file",
            Self::ContentChanged => "Content changed",
            Self::MetadataChanged => "Metadata changed",
            Self::EntryTypeChanged => "File type changed",
            Self::Moved => "Moved",
            Self::Renamed => "Renamed",
            Self::MovedAndRenamed => "Moved and renamed",
            Self::DirectoryOnlyInA => "Directory only in archive A",
            Self::DirectoryOnlyInB => "Directory only in archive B",
            Self::EmptyDirectoryOnlyInA => "Empty directory only in archive A",
            Self::EmptyDirectoryOnlyInB => "Empty directory only in archive B",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MetadataDifference {
    pub key: &'static str,
    pub value_a: Option<String>,
    pub value_b: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ContentAnalysis {
    pub sha256_a: Option<String>,
    pub sha256_b: Option<String>,
    pub sha512_a: Option<String>,
    pub sha512_b: Option<String>,
    pub blake3_a: Option<String>,
    pub blake3_b: Option<String>,
    pub binary_equal: Option<bool>,
    pub semantic_equal: Option<bool>,
    pub semantic_format: Option<String>,
    pub semantic_error: Option<String>,
    pub semantic_limit_reached: bool,
    pub semantic_diff: Option<String>,
    pub semantic_diff_truncated: bool,
    pub text_diff: Option<String>,
    pub text_diff_limit_reached: bool,
    pub text_diff_truncated: bool,
    pub image_metadata_a: Option<String>,
    pub image_metadata_b: Option<String>,
    pub first_difference: Option<u64>,
    pub error_a: Option<String>,
    pub error_b: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct ComparisonItem {
    pub kind: DifferenceKind,
    pub scope: String,
    pub path_a: Option<String>,
    pub path_b: Option<String>,
    pub metadata: Vec<MetadataDifference>,
    pub content: Option<ContentAnalysis>,
}

type EntryMap<'a> = BTreeMap<String, Vec<&'a EntryInfo>>;

fn normalized_path(path: &str) -> String {
    let mut path = path.replace('\\', "/");
    while let Some(relative) = path.strip_prefix("./") {
        path = relative.to_owned();
    }
    path.trim_matches('/').to_owned()
}

fn entry_map(entries: &[EntryInfo]) -> EntryMap<'_> {
    let mut map = BTreeMap::new();
    for entry in entries {
        map.entry(normalized_path(&entry.name))
            .or_insert_with(Vec::new)
            .push(entry);
    }
    map
}

fn add_metadata<T: ToString + PartialEq>(
    changes: &mut Vec<MetadataDifference>,
    key: &'static str,
    value_a: Option<T>,
    value_b: Option<T>,
) {
    if value_a != value_b {
        changes.push(MetadataDifference {
            key,
            value_a: value_a.map(|value| value.to_string()),
            value_b: value_b.map(|value| value.to_string()),
        });
    }
}

fn metadata_differences(a: &EntryInfo, b: &EntryInfo) -> Vec<MetadataDifference> {
    let mut changes = Vec::new();
    add_metadata(&mut changes, "File size", Some(a.size), Some(b.size));
    add_metadata(
        &mut changes,
        "Compressed size",
        a.compressed_size,
        b.compressed_size,
    );
    add_metadata(
        &mut changes,
        "Compression method",
        Some(&a.method),
        Some(&b.method),
    );
    add_metadata(
        &mut changes,
        "Modification time",
        a.modified.as_ref(),
        b.modified.as_ref(),
    );
    add_metadata(
        &mut changes,
        "Permissions and attributes",
        a.attributes.as_ref(),
        b.attributes.as_ref(),
    );
    add_metadata(&mut changes, "Owner ID", a.owner_id, b.owner_id);
    add_metadata(&mut changes, "Group ID", a.group_id, b.group_id);
    add_metadata(
        &mut changes,
        "File comment",
        a.comment.as_ref(),
        b.comment.as_ref(),
    );
    add_metadata(
        &mut changes,
        "Extra fields",
        a.extra_fields.as_ref(),
        b.extra_fields.as_ref(),
    );
    add_metadata(
        &mut changes,
        "Link target",
        a.link_target.as_ref(),
        b.link_target.as_ref(),
    );
    add_metadata(&mut changes, "Stored CRC-32", a.stored_crc, b.stored_crc);
    add_metadata(
        &mut changes,
        "Computed CRC-32",
        a.computed_crc,
        b.computed_crc,
    );
    add_metadata(
        &mut changes,
        "Symbolic or hard link",
        Some(a.link),
        Some(b.link),
    );
    add_metadata(
        &mut changes,
        "Entry integrity",
        a.error.as_ref(),
        b.error.as_ref(),
    );
    changes
}

fn item(
    kind: DifferenceKind,
    scope: &str,
    path_a: Option<String>,
    path_b: Option<String>,
    metadata: Vec<MetadataDifference>,
    content: Option<ContentAnalysis>,
) -> ComparisonItem {
    ComparisonItem {
        kind,
        scope: scope.to_owned(),
        path_a,
        path_b,
        metadata,
        content,
    }
}

fn content_analysis(a: &EntryInfo, b: &EntryInfo) -> ContentAnalysis {
    ContentAnalysis {
        sha256_a: a.sha256.clone(),
        sha256_b: b.sha256.clone(),
        ..ContentAnalysis::default()
    }
}

const MAX_SEMANTIC_COMPARE_BYTES: usize = 8 * 1024 * 1024;
const MAX_TEXT_DIFF_BYTES: usize = 2 * 1024 * 1024;
const MAX_TEXT_DIFF_OUTPUT: usize = 64 * 1024;

fn semantic_format(name_a: &str, name_b: &str) -> Option<&'static str> {
    let extension_a = std::path::Path::new(name_a)
        .extension()?
        .to_string_lossy()
        .to_ascii_lowercase();
    let extension_b = std::path::Path::new(name_b)
        .extension()?
        .to_string_lossy()
        .to_ascii_lowercase();
    if extension_a != extension_b {
        return None;
    }
    match extension_a.as_str() {
        "json" => Some("JSON"),
        "xml" => Some("XML"),
        "yaml" | "yml" => Some("YAML"),
        "toml" => Some("TOML"),
        _ => None,
    }
}

fn canonical_xml(source: &str) -> anyhow::Result<String> {
    let document = roxmltree::Document::parse(source)?;
    let mut canonical = String::new();
    for node in document.root().children() {
        canonical_xml_node(node, &mut canonical)?;
    }
    Ok(canonical)
}

fn canonical_xml_node(node: roxmltree::Node<'_, '_>, canonical: &mut String) -> anyhow::Result<()> {
    match node.node_type() {
        roxmltree::NodeType::Element => {
            let tag = node.tag_name();
            let name = expanded_xml_name(tag.namespace(), tag.name());
            canonical.push_str("S:");
            canonical.push_str(&serde_json::to_string(&name)?);
            let mut attributes: Vec<_> = node
                .attributes()
                .map(|attribute| {
                    (
                        expanded_xml_name(attribute.namespace(), attribute.name()),
                        attribute.value(),
                    )
                })
                .collect();
            attributes.sort();
            for (key, value) in attributes {
                canonical.push(':');
                canonical.push_str(&serde_json::to_string(&key)?);
                canonical.push('=');
                canonical.push_str(&serde_json::to_string(value)?);
            }
            canonical.push('\n');
            for child in node.children() {
                canonical_xml_node(child, canonical)?;
            }
            canonical.push_str("E:");
            canonical.push_str(&serde_json::to_string(&name)?);
            canonical.push('\n');
        }
        roxmltree::NodeType::Text => {
            if let Some(text) = node.text() {
                canonical.push_str("T:");
                canonical.push_str(&serde_json::to_string(text)?);
                canonical.push('\n');
            }
        }
        _ => {}
    }
    Ok(())
}

fn expanded_xml_name(namespace: Option<&str>, local_name: &str) -> String {
    namespace.map_or_else(
        || local_name.to_owned(),
        |namespace| format!("{{{namespace}}}{local_name}"),
    )
}

fn normalized_structured(format: &str, bytes: &[u8]) -> anyhow::Result<String> {
    Ok(match format {
        "JSON" => canonical_json(&serde_json::from_slice::<serde_json::Value>(bytes)?)?,
        "TOML" => {
            toml::to_string_pretty(&toml::from_str::<toml::Value>(std::str::from_utf8(bytes)?)?)?
        }
        "YAML" => serde_yaml::to_string(&serde_yaml::from_slice::<serde_yaml::Value>(bytes)?)?,
        "XML" => canonical_xml(std::str::from_utf8(bytes)?)?,
        _ => anyhow::bail!("Unsupported semantic format"),
    })
}

fn canonical_json(value: &serde_json::Value) -> anyhow::Result<String> {
    Ok(match value {
        serde_json::Value::Object(object) => {
            let sorted: BTreeMap<_, _> = object.iter().collect();
            let fields = sorted
                .into_iter()
                .map(|(key, value)| {
                    Ok(format!(
                        "{}:{}",
                        serde_json::to_string(key)?,
                        canonical_json(value)?
                    ))
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            format!("{{{}}}", fields.join(","))
        }
        serde_json::Value::Array(items) => {
            let items = items
                .iter()
                .map(canonical_json)
                .collect::<anyhow::Result<Vec<_>>>()?;
            format!("[{}]", items.join(","))
        }
        value => serde_json::to_string(value)?,
    })
}

fn structured_equal(format: &str, bytes_a: &[u8], bytes_b: &[u8]) -> anyhow::Result<bool> {
    Ok(match format {
        "JSON" => {
            serde_json::from_slice::<serde_json::Value>(bytes_a)?
                == serde_json::from_slice::<serde_json::Value>(bytes_b)?
        }
        "TOML" => {
            toml::from_str::<toml::Value>(std::str::from_utf8(bytes_a)?)?
                == toml::from_str::<toml::Value>(std::str::from_utf8(bytes_b)?)?
        }
        "YAML" => {
            serde_yaml::from_slice::<serde_yaml::Value>(bytes_a)?
                == serde_yaml::from_slice::<serde_yaml::Value>(bytes_b)?
        }
        "XML" => {
            canonical_xml(std::str::from_utf8(bytes_a)?)?
                == canonical_xml(std::str::from_utf8(bytes_b)?)?
        }
        _ => anyhow::bail!("Unsupported semantic format"),
    })
}

/// Hashes and byte-compares optional extracted files, then checks text semantics when supported.
pub(crate) fn analyze_readers(
    mut reader_a: Option<&mut dyn Read>,
    name_a: Option<&str>,
    mut reader_b: Option<&mut dyn Read>,
    name_b: Option<&str>,
) -> anyhow::Result<ContentAnalysis> {
    let paired = reader_a.is_some() && reader_b.is_some();
    let mut sha256_a = Sha256::new();
    let mut sha256_b = Sha256::new();
    let mut sha512_a = Sha512::new();
    let mut sha512_b = Sha512::new();
    let mut blake3_a = blake3::Hasher::new();
    let mut blake3_b = blake3::Hasher::new();
    let mut bytes_a = Some(Vec::new());
    let mut bytes_b = Some(Vec::new());
    let mut buffer_a = [0; 64 * 1024];
    let mut buffer_b = [0; 64 * 1024];
    let mut offset = 0_u64;
    let mut length_a = 0_u64;
    let mut length_b = 0_u64;
    let mut first_difference = None;
    let mut binary_equal = true;

    loop {
        crate::task_queue::checkpoint()?;
        let count_a = match reader_a.as_deref_mut() {
            Some(reader) => reader.read(&mut buffer_a)?,
            None => 0,
        };
        let count_b = match reader_b.as_deref_mut() {
            Some(reader) => reader.read(&mut buffer_b)?,
            None => 0,
        };
        if count_a == 0 && count_b == 0 {
            break;
        }
        if paired {
            let common = count_a.min(count_b);
            if let Some(index) = buffer_a[..common]
                .iter()
                .zip(&buffer_b[..common])
                .position(|(a, b)| a != b)
            {
                binary_equal = false;
                first_difference.get_or_insert(offset + index as u64);
            } else if count_a != count_b {
                binary_equal = false;
                first_difference.get_or_insert(offset + common as u64);
            }
        }
        if count_a > 0 {
            length_a = length_a.saturating_add(count_a as u64);
            let chunk = &buffer_a[..count_a];
            sha256_a.update(chunk);
            sha512_a.update(chunk);
            blake3_a.update(chunk);
            if let Some(bytes) = &mut bytes_a {
                if bytes.len().saturating_add(count_a) <= MAX_SEMANTIC_COMPARE_BYTES {
                    bytes.extend_from_slice(chunk);
                } else {
                    bytes_a = None;
                }
            }
        }
        if count_b > 0 {
            length_b = length_b.saturating_add(count_b as u64);
            let chunk = &buffer_b[..count_b];
            sha256_b.update(chunk);
            sha512_b.update(chunk);
            blake3_b.update(chunk);
            if let Some(bytes) = &mut bytes_b {
                if bytes.len().saturating_add(count_b) <= MAX_SEMANTIC_COMPARE_BYTES {
                    bytes.extend_from_slice(chunk);
                } else {
                    bytes_b = None;
                }
            }
        }
        offset += count_a.max(count_b) as u64;
    }

    let mut result = ContentAnalysis {
        sha256_a: reader_a
            .is_some()
            .then(|| format!("{:x}", sha256_a.finalize())),
        sha256_b: reader_b
            .is_some()
            .then(|| format!("{:x}", sha256_b.finalize())),
        sha512_a: reader_a
            .is_some()
            .then(|| format!("{:x}", sha512_a.finalize())),
        sha512_b: reader_b
            .is_some()
            .then(|| format!("{:x}", sha512_b.finalize())),
        blake3_a: reader_a
            .is_some()
            .then(|| blake3_a.finalize().to_hex().to_string()),
        blake3_b: reader_b
            .is_some()
            .then(|| blake3_b.finalize().to_hex().to_string()),
        binary_equal: paired.then_some(binary_equal),
        first_difference,
        ..ContentAnalysis::default()
    };
    if let (Some(name_a), Some(name_b)) = (name_a, name_b) {
        if let Some(format) = semantic_format(name_a, name_b) {
            result.semantic_format = Some(format.to_owned());
            if length_a.max(length_b) > MAX_SEMANTIC_COMPARE_BYTES as u64 {
                result.semantic_limit_reached = true;
            } else if let (Some(bytes_a), Some(bytes_b)) = (&bytes_a, &bytes_b) {
                match structured_equal(format, bytes_a, bytes_b) {
                    Ok(equal) => {
                        result.semantic_equal = Some(equal);
                        if !equal {
                            let normalized_a = normalized_structured(format, bytes_a)?;
                            let normalized_b = normalized_structured(format, bytes_b)?;
                            let diff = similar::TextDiff::from_lines(&normalized_a, &normalized_b)
                                .unified_diff()
                                .context_radius(3)
                                .to_string();
                            result.semantic_diff_truncated =
                                diff.chars().count() > MAX_TEXT_DIFF_OUTPUT;
                            result.semantic_diff =
                                Some(diff.chars().take(MAX_TEXT_DIFF_OUTPUT).collect());
                        }
                    }
                    Err(error) => result.semantic_error = Some(format!("{error:#}")),
                }
            }
        }
        if result.binary_equal == Some(false) {
            if length_a.max(length_b) > MAX_TEXT_DIFF_BYTES as u64 {
                result.text_diff_limit_reached = true;
            } else if let (Some(bytes_a), Some(bytes_b)) = (&bytes_a, &bytes_b) {
                if let (Ok(text_a), Ok(text_b)) =
                    (std::str::from_utf8(bytes_a), std::str::from_utf8(bytes_b))
                {
                    let diff = similar::TextDiff::from_lines(text_a, text_b)
                        .unified_diff()
                        .context_radius(3)
                        .to_string();
                    result.text_diff_truncated = diff.chars().count() > MAX_TEXT_DIFF_OUTPUT;
                    result.text_diff = Some(diff.chars().take(MAX_TEXT_DIFF_OUTPUT).collect());
                }
            }
        }
    }
    Ok(result)
}

fn parent_directories(path: &str, directories: &mut BTreeSet<String>) {
    let mut parent = std::path::Path::new(path).parent();
    while let Some(directory) = parent {
        let value = directory.to_string_lossy().replace('\\', "/");
        if value.is_empty() || value == "." {
            break;
        }
        directories.insert(value.clone());
        parent = directory.parent();
    }
}

fn directory_set(entries: &[EntryInfo]) -> BTreeSet<String> {
    let mut directories = BTreeSet::new();
    for entry in entries {
        let path = normalized_path(&entry.name);
        if entry.directory {
            directories.insert(path.clone());
        }
        parent_directories(&path, &mut directories);
    }
    directories.remove("");
    directories
}

fn is_empty_directory(path: &str, entries: &[EntryInfo]) -> bool {
    let prefix = format!("{}/", path.trim_end_matches('/'));
    !entries.iter().any(|entry| {
        let candidate = normalized_path(&entry.name);
        candidate != path && candidate.starts_with(&prefix)
    })
}

fn moved_kind(path_a: &str, path_b: &str) -> DifferenceKind {
    let name_a = std::path::Path::new(path_a).file_name();
    let name_b = std::path::Path::new(path_b).file_name();
    let parent_a = std::path::Path::new(path_a).parent();
    let parent_b = std::path::Path::new(path_b).parent();
    match (name_a == name_b, parent_a == parent_b) {
        (true, false) => DifferenceKind::Moved,
        (false, true) => DifferenceKind::Renamed,
        _ => DifferenceKind::MovedAndRenamed,
    }
}

/// Compares paths, content hashes, directory hierarchy, and exposed entry metadata.
pub(crate) fn compare_entries(
    scope: &str,
    entries_a: &[EntryInfo],
    entries_b: &[EntryInfo],
) -> Vec<ComparisonItem> {
    let map_a = entry_map(entries_a);
    let map_b = entry_map(entries_b);
    let all_paths: BTreeSet<_> = map_a.keys().chain(map_b.keys()).cloned().collect();
    let mut items = Vec::new();
    let mut unmatched_a = Vec::new();
    let mut unmatched_b = Vec::new();

    for path in all_paths {
        let a = map_a.get(&path).map(Vec::as_slice).unwrap_or_default();
        let b = map_b.get(&path).map(Vec::as_slice).unwrap_or_default();
        let common_count = a.len().min(b.len());
        for (entry_a, entry_b) in a.iter().zip(b.iter()).take(common_count) {
            let type_changed = entry_a.directory != entry_b.directory;
            let kind = if type_changed {
                DifferenceKind::EntryTypeChanged
            } else if !entry_a.directory
                && entry_a.sha256.is_some()
                && entry_b.sha256.is_some()
                && entry_a.sha256 != entry_b.sha256
            {
                DifferenceKind::ContentChanged
            } else if metadata_differences(entry_a, entry_b).is_empty() {
                DifferenceKind::Common
            } else {
                DifferenceKind::MetadataChanged
            };
            items.push(item(
                kind,
                scope,
                Some(path.clone()),
                Some(path.clone()),
                metadata_differences(entry_a, entry_b),
                (!entry_a.directory && !entry_b.directory)
                    .then(|| content_analysis(entry_a, entry_b)),
            ));
        }
        unmatched_a.extend(
            a.iter()
                .skip(common_count)
                .map(|entry| (path.clone(), *entry)),
        );
        unmatched_b.extend(
            b.iter()
                .skip(common_count)
                .map(|entry| (path.clone(), *entry)),
        );
    }

    let mut a_by_hash: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    let mut b_by_hash: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (index, (_, entry)) in unmatched_a.iter().enumerate() {
        if !entry.directory {
            if let Some(hash) = entry.sha256.as_deref() {
                a_by_hash.entry(hash).or_default().push(index);
            }
        }
    }
    for (index, (_, entry)) in unmatched_b.iter().enumerate() {
        if !entry.directory {
            if let Some(hash) = entry.sha256.as_deref() {
                b_by_hash.entry(hash).or_default().push(index);
            }
        }
    }
    let mut paired_a = BTreeSet::new();
    let mut paired_b = BTreeSet::new();
    for (hash, indices_a) in &a_by_hash {
        let Some(indices_b) = b_by_hash.get(hash) else {
            continue;
        };
        for (&index_a, &index_b) in indices_a.iter().zip(indices_b) {
            let (path_a, entry_a) = &unmatched_a[index_a];
            let (path_b, entry_b) = &unmatched_b[index_b];
            paired_a.insert(index_a);
            paired_b.insert(index_b);
            items.push(item(
                moved_kind(path_a, path_b),
                scope,
                Some(path_a.clone()),
                Some(path_b.clone()),
                metadata_differences(entry_a, entry_b),
                Some(content_analysis(entry_a, entry_b)),
            ));
        }
    }

    let mut a_by_name: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    let mut b_by_name: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (index, (_, entry)) in unmatched_a.iter().enumerate() {
        if paired_a.contains(&index) {
            continue;
        }
        if let Some(name) = std::path::Path::new(&entry.name)
            .file_name()
            .and_then(|name| name.to_str())
        {
            a_by_name.entry(name).or_default().push(index);
        }
    }
    for (index, (_, entry)) in unmatched_b.iter().enumerate() {
        if paired_b.contains(&index) {
            continue;
        }
        if let Some(name) = std::path::Path::new(&entry.name)
            .file_name()
            .and_then(|name| name.to_str())
        {
            b_by_name.entry(name).or_default().push(index);
        }
    }
    for (name, indices_a) in &a_by_name {
        let Some(indices_b) = b_by_name.get(name) else {
            continue;
        };
        if indices_a.len() != 1 || indices_b.len() != 1 {
            continue;
        }
        let index_a = indices_a[0];
        let index_b = indices_b[0];
        let (path_a, entry_a) = &unmatched_a[index_a];
        let (path_b, entry_b) = &unmatched_b[index_b];
        if path_a == path_b {
            continue;
        }
        paired_a.insert(index_a);
        paired_b.insert(index_b);
        items.push(item(
            DifferenceKind::Moved,
            scope,
            Some(path_a.clone()),
            Some(path_b.clone()),
            metadata_differences(entry_a, entry_b),
            Some(content_analysis(entry_a, entry_b)),
        ));
    }

    for (index, (path, entry)) in unmatched_a.into_iter().enumerate() {
        if paired_a.contains(&index) {
            continue;
        }
        if entry.directory {
            let empty = is_empty_directory(&path, entries_a);
            items.push(item(
                if empty {
                    DifferenceKind::EmptyDirectoryOnlyInA
                } else {
                    DifferenceKind::DirectoryOnlyInA
                },
                scope,
                Some(path),
                None,
                Vec::new(),
                None,
            ));
        } else {
            items.push(item(
                DifferenceKind::OnlyInA,
                scope,
                Some(path),
                None,
                Vec::new(),
                None,
            ));
        }
    }
    for (index, (path, entry)) in unmatched_b.into_iter().enumerate() {
        if paired_b.contains(&index) {
            continue;
        }
        if entry.directory {
            let empty = is_empty_directory(&path, entries_b);
            items.push(item(
                if empty {
                    DifferenceKind::EmptyDirectoryOnlyInB
                } else {
                    DifferenceKind::DirectoryOnlyInB
                },
                scope,
                None,
                Some(path),
                Vec::new(),
                None,
            ));
        } else {
            items.push(item(
                DifferenceKind::OnlyInB,
                scope,
                None,
                Some(path),
                Vec::new(),
                None,
            ));
        }
    }

    let directories_a = directory_set(entries_a);
    let directories_b = directory_set(entries_b);
    for path in directories_a.difference(&directories_b) {
        if !entries_a
            .iter()
            .any(|entry| entry.directory && normalized_path(&entry.name) == *path)
        {
            items.push(item(
                DifferenceKind::DirectoryOnlyInA,
                scope,
                Some(path.clone()),
                None,
                Vec::new(),
                None,
            ));
        }
    }
    for path in directories_b.difference(&directories_a) {
        if !entries_b
            .iter()
            .any(|entry| entry.directory && normalized_path(&entry.name) == *path)
        {
            items.push(item(
                DifferenceKind::DirectoryOnlyInB,
                scope,
                None,
                Some(path.clone()),
                Vec::new(),
                None,
            ));
        }
    }
    items.sort_by(|a, b| {
        a.scope
            .cmp(&b.scope)
            .then_with(|| a.path_a.cmp(&b.path_a))
            .then_with(|| a.path_b.cmp(&b.path_b))
            .then_with(|| a.kind.label().cmp(b.kind.label()))
    });
    items
}

#[derive(Clone, Debug)]
pub(crate) struct ComparisonReport {
    pub archive_a: String,
    pub archive_b: String,
    pub integrity_a: Integrity,
    pub integrity_b: Integrity,
    pub archive_metadata: Vec<MetadataDifference>,
    pub items: Vec<ComparisonItem>,
    pub complete: bool,
    pub nested_archives_analyzed: usize,
    pub errors: Vec<String>,
}

impl ComparisonReport {
    pub(crate) fn json(&self) -> serde_json::Value {
        let mut summary = BTreeMap::<String, usize>::new();
        for item in &self.items {
            *summary.entry(format!("{:?}", item.kind)).or_default() += 1;
        }
        serde_json::json!({
            "archive_a": self.archive_a,
            "archive_b": self.archive_b,
            "integrity": {
                "a": format!("{:?}", self.integrity_a),
                "b": format!("{:?}", self.integrity_b),
            },
            "archive_metadata_changes": self.archive_metadata.iter().map(|change| serde_json::json!({
                "field": change.key, "a": change.value_a, "b": change.value_b,
            })).collect::<Vec<_>>(),
            "summary": summary,
            "complete": self.complete,
            "nested_archives_analyzed": self.nested_archives_analyzed,
            "errors": self.errors,
            "differences": self.items.iter().map(|item| serde_json::json!({
                "kind": format!("{:?}", item.kind),
                "scope": item.scope,
                "path_a": item.path_a,
                "path_b": item.path_b,
                "metadata_changes": item.metadata.iter().map(|change| serde_json::json!({
                    "field": change.key, "a": change.value_a, "b": change.value_b,
                })).collect::<Vec<_>>(),
                "content": item.content.as_ref().map(|content| serde_json::json!({
                    "sha256_a": content.sha256_a, "sha256_b": content.sha256_b,
                    "sha512_a": content.sha512_a, "sha512_b": content.sha512_b,
                    "blake3_a": content.blake3_a, "blake3_b": content.blake3_b,
                    "binary_equal": content.binary_equal,
                    "first_difference": content.first_difference,
                    "semantic_format": content.semantic_format,
                    "semantic_equal": content.semantic_equal,
                    "semantic_error": content.semantic_error,
                    "semantic_limit_reached": content.semantic_limit_reached,
                    "semantic_diff": content.semantic_diff,
                    "semantic_diff_truncated": content.semantic_diff_truncated,
                    "text_diff": content.text_diff,
                    "text_diff_limit_reached": content.text_diff_limit_reached,
                    "text_diff_truncated": content.text_diff_truncated,
                    "image_metadata_a": content.image_metadata_a,
                    "image_metadata_b": content.image_metadata_b,
                    "error_a": content.error_a, "error_b": content.error_b,
                })),
            })).collect::<Vec<_>>(),
            "limits": {
                "max_nested_depth": MAX_NESTING_DEPTH,
                "max_nested_archives": MAX_NESTED_ARCHIVES,
                "max_semantic_file_bytes": MAX_SEMANTIC_COMPARE_BYTES,
                "max_text_diff_bytes": MAX_TEXT_DIFF_BYTES,
            },
        })
    }
}

const MAX_NESTING_DEPTH: usize = 4;
const MAX_NESTED_ARCHIVES: usize = 128;

#[derive(Default)]
struct ComparisonState {
    complete: bool,
    nested_archives_analyzed: usize,
    errors: Vec<String>,
}

fn archive_report_is_complete(report: &Report) -> bool {
    !report.limited
        && report.error.is_none()
        && report.detected.is_some()
        && !matches!(
            report.integrity,
            Integrity::Incomplete | Integrity::PasswordRequired
        )
}

fn archive_metadata_differences(a: &Report, b: &Report) -> Vec<MetadataDifference> {
    let mut changes = Vec::new();
    add_metadata(
        &mut changes,
        "Archive size",
        Some(a.archive_size),
        Some(b.archive_size),
    );
    add_metadata(
        &mut changes,
        "Format",
        Some(&a.signature),
        Some(&b.signature),
    );
    add_metadata(
        &mut changes,
        "Creation time",
        a.created.as_ref(),
        b.created.as_ref(),
    );
    add_metadata(
        &mut changes,
        "Modification time",
        a.modified.as_ref(),
        b.modified.as_ref(),
    );
    add_metadata(
        &mut changes,
        "Uncompressed size",
        Some(a.unpacked_size()),
        Some(b.unpacked_size()),
    );
    add_metadata(
        &mut changes,
        "Archive integrity",
        Some(format!("{:?}", a.integrity)),
        Some(format!("{:?}", b.integrity)),
    );
    add_metadata(
        &mut changes,
        "Archive SHA-256",
        a.archive_sha256.as_ref(),
        b.archive_sha256.as_ref(),
    );
    changes
}

/// Audits and compares two archives, including supported nested archive pairs.
pub(crate) fn compare_archives(
    path_a: &Path,
    path_b: &Path,
    cancel: &AtomicBool,
) -> anyhow::Result<ComparisonReport> {
    let report_a = inspection::inspect(path_a, None, inspection::InspectionMode::FullAudit, cancel);
    let report_b = inspection::inspect(path_b, None, inspection::InspectionMode::FullAudit, cancel);
    let mut state = ComparisonState {
        complete: archive_report_is_complete(&report_a) && archive_report_is_complete(&report_b),
        ..ComparisonState::default()
    };
    for (path, report) in [(path_a, &report_a), (path_b, &report_b)] {
        if let Some(error) = &report.error {
            state.errors.push(format!("{}: {error}", path.display()));
        }
    }
    let archive_metadata = archive_metadata_differences(&report_a, &report_b);
    let mut items = compare_report_contents(
        Some(&report_a),
        Some(path_a),
        Some(&report_b),
        Some(path_b),
        "",
        0,
        cancel,
        &mut state,
    )?;
    if !archive_metadata.is_empty() {
        items.push(item(
            DifferenceKind::MetadataChanged,
            "Archive metadata",
            Some(archive_label(path_a)),
            Some(archive_label(path_b)),
            archive_metadata.clone(),
            None,
        ));
    }
    Ok(ComparisonReport {
        archive_a: path_a.display().to_string(),
        archive_b: path_b.display().to_string(),
        integrity_a: report_a.integrity,
        integrity_b: report_b.integrity,
        archive_metadata,
        items,
        complete: state.complete,
        nested_archives_analyzed: state.nested_archives_analyzed,
        errors: state.errors,
    })
}

fn archive_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn entry_for<'a>(report: Option<&'a Report>, path: Option<&str>) -> Option<&'a EntryInfo> {
    let path = path?;
    report?
        .entries
        .iter()
        .find(|entry| normalized_path(&entry.name) == path)
}

fn nested_report(path: &Path, cancel: &AtomicBool, state: &mut ComparisonState) -> Option<Report> {
    if !crate::archive::is_supported_archive(path) {
        return None;
    }
    let report = inspection::inspect(path, None, inspection::InspectionMode::FullAudit, cancel);
    if !archive_report_is_complete(&report) {
        state.complete = false;
    }
    if let Some(error) = &report.error {
        state.errors.push(format!("{}: {error}", path.display()));
    }
    Some(report)
}

fn compare_report_contents(
    report_a: Option<&Report>,
    source_a: Option<&Path>,
    report_b: Option<&Report>,
    source_b: Option<&Path>,
    scope: &str,
    depth: usize,
    cancel: &AtomicBool,
    state: &mut ComparisonState,
) -> anyhow::Result<Vec<ComparisonItem>> {
    let entries_a = report_a.map_or(&[][..], |report| report.entries.as_slice());
    let entries_b = report_b.map_or(&[][..], |report| report.entries.as_slice());
    let mut items = compare_entries(scope, entries_a, entries_b);
    let mut nested_items = Vec::new();

    for item in &mut items {
        crate::task_queue::checkpoint()?;
        let entry_a = entry_for(report_a, item.path_a.as_deref());
        let entry_b = entry_for(report_b, item.path_b.as_deref());
        if entry_a.is_none_or(|entry| entry.directory)
            && entry_b.is_none_or(|entry| entry.directory)
        {
            continue;
        }

        let mut content = item.content.take().unwrap_or_default();
        if let Some(entry) = entry_a {
            content.sha256_a = entry.sha256.clone();
        }
        if let Some(entry) = entry_b {
            content.sha256_b = entry.sha256.clone();
        }
        let mut file_a = None;
        let mut file_b = None;
        if let (Some(source), Some(entry)) = (source_a, entry_a.filter(|entry| !entry.link)) {
            match crate::archive::extract_entry_with_password(source, &entry.name, None) {
                Ok(file) => file_a = Some(file),
                Err(error) => {
                    content.error_a = Some(format!("{error:#}"));
                    state.complete = false;
                }
            }
        }
        if let (Some(source), Some(entry)) = (source_b, entry_b.filter(|entry| !entry.link)) {
            match crate::archive::extract_entry_with_password(source, &entry.name, None) {
                Ok(file) => file_b = Some(file),
                Err(error) => {
                    content.error_b = Some(format!("{error:#}"));
                    state.complete = false;
                }
            }
        }
        if file_a.is_some() || file_b.is_some() {
            let analysis = analyze_readers(
                file_a
                    .as_mut()
                    .map(|file| file.as_file_mut() as &mut dyn Read),
                item.path_a.as_deref(),
                file_b
                    .as_mut()
                    .map(|file| file.as_file_mut() as &mut dyn Read),
                item.path_b.as_deref(),
            );
            match analysis {
                Ok(mut analysis) => {
                    state.complete &= !analysis.semantic_limit_reached
                        && !analysis.text_diff_limit_reached
                        && analysis.semantic_error.is_none();
                    if content.error_a.is_some() {
                        analysis.error_a = content.error_a.take();
                        analysis.sha256_a = entry_a.and_then(|entry| entry.sha256.clone());
                    }
                    if content.error_b.is_some() {
                        analysis.error_b = content.error_b.take();
                        analysis.sha256_b = entry_b.and_then(|entry| entry.sha256.clone());
                    }
                    content = analysis;
                }
                Err(error) => {
                    if cancel.load(Ordering::Relaxed) {
                        return Err(error);
                    }
                    content.error_a = Some(format!("{error:#}"));
                    state.complete = false;
                }
            }
        }
        if let Some(file) = &file_a {
            content.image_metadata_a = image_metadata(file.path());
        }
        if let Some(file) = &file_b {
            content.image_metadata_b = image_metadata(file.path());
        }
        if content.image_metadata_a != content.image_metadata_b
            && (content.image_metadata_a.is_some() || content.image_metadata_b.is_some())
        {
            item.metadata.push(MetadataDifference {
                key: "Image metadata",
                value_a: content.image_metadata_a.clone(),
                value_b: content.image_metadata_b.clone(),
            });
        }
        if content.binary_equal == Some(false) && item.kind == DifferenceKind::Common {
            item.kind = DifferenceKind::ContentChanged;
        }
        item.content = Some(content);

        let nested_a = file_a
            .as_ref()
            .is_some_and(|file| crate::archive::is_supported_archive(file.path()));
        let nested_b = file_b
            .as_ref()
            .is_some_and(|file| crate::archive::is_supported_archive(file.path()));
        if !nested_a && !nested_b {
            continue;
        }
        if depth >= MAX_NESTING_DEPTH || state.nested_archives_analyzed >= MAX_NESTED_ARCHIVES {
            state.complete = false;
            if let Some(content) = &mut item.content {
                let message = "Nested archive comparison limit reached".to_owned();
                if nested_a {
                    content.error_a = Some(message.clone());
                }
                if nested_b {
                    content.error_b = Some(message);
                }
            }
            continue;
        }
        state.nested_archives_analyzed += 1;
        let child_a = file_a
            .as_ref()
            .filter(|_| nested_a)
            .and_then(|file| nested_report(file.path(), cancel, state));
        let child_b = file_b
            .as_ref()
            .filter(|_| nested_b)
            .and_then(|file| nested_report(file.path(), cancel, state));
        let child_scope = format!(
            "{}!{} -> {}",
            scope,
            item.path_a.as_deref().unwrap_or("-"),
            item.path_b.as_deref().unwrap_or("-")
        );
        nested_items.extend(compare_report_contents(
            child_a.as_ref(),
            file_a.as_ref().filter(|_| nested_a).map(|file| file.path()),
            child_b.as_ref(),
            file_b.as_ref().filter(|_| nested_b).map(|file| file.path()),
            &child_scope,
            depth + 1,
            cancel,
            state,
        )?);
    }

    items.extend(nested_items);
    items.sort_by(|a, b| {
        a.scope
            .cmp(&b.scope)
            .then_with(|| a.path_a.cmp(&b.path_a))
            .then_with(|| a.path_b.cmp(&b.path_b))
            .then_with(|| a.kind.label().cmp(b.kind.label()))
    });
    Ok(items)
}

fn image_metadata(path: &Path) -> Option<String> {
    let mut values = Vec::new();
    let reader = image::ImageReader::new(BufReader::new(File::open(path).ok()?))
        .with_guessed_format()
        .ok()?;
    let format = reader.format().map(|format| format!("{format:?}"));
    if let Ok((width, height)) = reader.into_dimensions() {
        values.push(format!("{width}x{height}"));
    }
    if let Some(format) = format {
        values.push(format);
    }
    if let Ok(file) = File::open(path) {
        let mut file = BufReader::new(file);
        if let Ok(exif) = exif::Reader::new().read_from_container(&mut file) {
            let mut fields: Vec<_> = exif
                .fields()
                .take(64)
                .map(|field| {
                    format!(
                        "{:?}: {}",
                        field.tag,
                        field.display_value().with_unit(&exif)
                    )
                })
                .collect();
            fields.sort();
            values.extend(fields);
        }
    }
    (!values.is_empty()).then(|| values.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        for (name, bytes) in entries {
            zip.start_file(*name, options).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    fn write_zip(path: &Path, entries: &[(&str, &[u8])], empty_directory: bool) {
        let mut zip = zip::ZipWriter::new(File::create(path).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        for (name, bytes) in entries {
            zip.start_file(*name, options).unwrap();
            zip.write_all(bytes).unwrap();
        }
        if empty_directory {
            zip.add_directory("empty/", options).unwrap();
        }
        zip.finish().unwrap();
    }

    fn compare_bytes(name: &str, a: &[u8], b: &[u8]) -> ContentAnalysis {
        let mut reader_a = std::io::Cursor::new(a);
        let mut reader_b = std::io::Cursor::new(b);
        analyze_readers(
            Some(&mut reader_a),
            Some(name),
            Some(&mut reader_b),
            Some(name),
        )
        .unwrap()
    }

    fn entry(name: &str, directory: bool, sha256: Option<&str>) -> EntryInfo {
        EntryInfo {
            name: name.into(),
            directory,
            size: 4,
            compressed_size: Some(4),
            method: "Stored".into(),
            modified: None,
            attributes: None,
            owner_id: None,
            group_id: None,
            comment: None,
            extra_fields: None,
            link_target: None,
            stored_crc: Some(1),
            computed_crc: Some(1),
            sha256: sha256.map(str::to_owned),
            encrypted: false,
            link: false,
            error: None,
        }
    }

    #[test]
    fn compares_common_unique_moved_renamed_and_empty_directory_entries() {
        let entries_a = vec![
            entry("docs/common.txt", false, Some("same")),
            entry("docs/changed.json", false, Some("old")),
            entry("old/name.txt", false, Some("move")),
            entry("old/revised.txt", false, Some("old-content")),
            entry("only-a.txt", false, Some("a")),
            entry("empty/", true, None),
        ];
        let entries_b = vec![
            entry("docs\\common.txt", false, Some("same")),
            entry("docs/changed.json", false, Some("new")),
            entry("new/name.txt", false, Some("move")),
            entry("new/revised.txt", false, Some("new-content")),
            entry("only-b.txt", false, Some("b")),
        ];
        let result = compare_entries("root.zip", &entries_a, &entries_b);
        let kinds: BTreeSet<_> = result.iter().map(|item| item.kind).collect();
        assert!(kinds.contains(&DifferenceKind::Common));
        assert!(kinds.contains(&DifferenceKind::ContentChanged));
        assert!(kinds.contains(&DifferenceKind::Moved));
        assert!(result.iter().any(|item| {
            item.kind == DifferenceKind::Moved
                && item.path_a.as_deref() == Some("old/revised.txt")
                && item.path_b.as_deref() == Some("new/revised.txt")
        }));
        assert!(kinds.contains(&DifferenceKind::OnlyInA));
        assert!(kinds.contains(&DifferenceKind::OnlyInB));
        assert!(kinds.contains(&DifferenceKind::EmptyDirectoryOnlyInA));
        assert!(
            result
                .iter()
                .any(|item| item.kind == DifferenceKind::DirectoryOnlyInA)
        );
    }

    #[test]
    fn classifies_renames_and_reports_available_metadata_changes() {
        let entries_a = vec![entry("before.txt", false, Some("same"))];
        let mut renamed = entry("after.txt", false, Some("same"));
        renamed.compressed_size = Some(2);
        renamed.modified = Some("2026-01-01".into());
        let result = compare_entries("root.zip", &entries_a, &[renamed]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].kind, DifferenceKind::Renamed);
        assert!(
            result[0]
                .metadata
                .iter()
                .any(|item| item.key == "Compressed size")
        );
        assert!(
            result[0]
                .metadata
                .iter()
                .any(|item| item.key == "Modification time")
        );
    }

    #[test]
    fn compares_exact_bytes_and_calculates_sha512_and_blake3() {
        let equal = compare_bytes("data.bin", b"same bytes", b"same bytes");
        assert_eq!(equal.binary_equal, Some(true));
        assert_eq!(equal.first_difference, None);
        assert_eq!(equal.sha512_a, equal.sha512_b);
        assert_eq!(equal.blake3_a, equal.blake3_b);

        let different = compare_bytes("data.bin", b"abc", b"axc");
        assert_eq!(different.binary_equal, Some(false));
        assert_eq!(different.first_difference, Some(1));
    }

    #[test]
    fn recognizes_semantically_equal_structured_text() {
        for (name, a, b) in [
            (
                "data.json",
                br#"{"a":1,"b":2}"#.as_slice(),
                br#"{"b":2,"a":1}"#.as_slice(),
            ),
            ("data.yaml", b"a: 1\nb: 2\n", b"b: 2\na: 1\n"),
            ("data.toml", b"a = 1\nb = 2\n", b"b = 2\na = 1\n"),
            (
                "data.xml",
                b"<root z=\"2\" a=\"1\"><child/></root>",
                b"<root a=\"1\" z=\"2\"><child></child></root>",
            ),
            (
                "namespaced.xml",
                b"<x:root xmlns:x=\"urn:example\"><x:item/></x:root>",
                b"<y:root xmlns:y=\"urn:example\"><y:item></y:item></y:root>",
            ),
        ] {
            let result = compare_bytes(name, a, b);
            assert_eq!(result.binary_equal, Some(false), "{name}");
            assert_eq!(result.semantic_equal, Some(true), "{name}");
            assert!(result.text_diff.is_some(), "{name}");
        }
    }

    #[test]
    fn structured_diff_ignores_json_key_order_and_shows_value_changes() {
        let equal = compare_bytes("data.json", br#"{"a":1,"b":2}"#, br#"{"b":2,"a":1}"#);
        assert_eq!(equal.semantic_equal, Some(true));
        assert_eq!(equal.semantic_diff, None);

        let changed = compare_bytes("data.json", br#"{"a":1}"#, br#"{"a":2}"#);
        assert_eq!(changed.semantic_equal, Some(false));
        assert!(
            changed
                .semantic_diff
                .as_deref()
                .is_some_and(|diff| diff.contains("1") && diff.contains("2"))
        );
    }

    #[test]
    fn reports_semantic_and_text_limits_instead_of_claiming_equal() {
        let large_structured = vec![b' '; MAX_SEMANTIC_COMPARE_BYTES + 1];
        let semantic = compare_bytes("large.json", &large_structured, &large_structured);
        assert!(semantic.semantic_limit_reached);
        assert_eq!(semantic.semantic_equal, None);

        let large_text = vec![b'a'; MAX_TEXT_DIFF_BYTES + 1];
        let changed_text = vec![b'b'; MAX_TEXT_DIFF_BYTES + 1];
        let text = compare_bytes("large.txt", &large_text, &changed_text);
        assert!(text.text_diff_limit_reached);
        assert_eq!(text.binary_equal, Some(false));
        assert_eq!(text.first_difference, Some(0));
    }

    #[test]
    fn compares_real_archives_and_recurses_into_nested_archives() {
        let root = tempfile::tempdir().unwrap();
        let path_a = root.path().join("a.zip");
        let path_b = root.path().join("b.zip");
        let nested_a = zip_bytes(&[("inside.txt", b"old content")]);
        let nested_b = zip_bytes(&[("inside.txt", b"new content")]);
        write_zip(
            &path_a,
            &[
                ("docs/shared.json", br#"{"a":1,"b":2}"#),
                ("old/name.txt", b"same bytes"),
                ("only-a.txt", b"a"),
                ("nested.zip", &nested_a),
            ],
            true,
        );
        write_zip(
            &path_b,
            &[
                ("docs/shared.json", br#"{"b":2,"a":1}"#),
                ("new/name.txt", b"same bytes"),
                ("only-b.txt", b"b"),
                ("nested.zip", &nested_b),
            ],
            false,
        );

        let report = compare_archives(&path_a, &path_b, &AtomicBool::new(false)).unwrap();
        assert!(report.complete, "{:?}", report.errors);
        assert_eq!(report.nested_archives_analyzed, 1);
        assert!(report.items.iter().any(|item| {
            item.scope.is_empty()
                && item.path_a.as_deref() == Some("docs/shared.json")
                && item.content.as_ref().is_some_and(|content| {
                    content.binary_equal == Some(false)
                        && content.semantic_equal == Some(true)
                        && content.sha512_a.is_some()
                        && content.blake3_a.is_some()
                })
        }));
        assert!(report.items.iter().any(|item| {
            item.scope.is_empty()
                && item.kind == DifferenceKind::Moved
                && item.path_a.as_deref() == Some("old/name.txt")
                && item.path_b.as_deref() == Some("new/name.txt")
        }));
        assert!(report.items.iter().any(|item| {
            item.scope.starts_with("!nested.zip")
                && item.kind == DifferenceKind::ContentChanged
                && item.path_a.as_deref() == Some("inside.txt")
        }));
        assert!(report.items.iter().any(|item| {
            item.kind == DifferenceKind::EmptyDirectoryOnlyInA
                && item.path_a.as_deref() == Some("empty")
        }));
        assert_eq!(report.json()["complete"], true);
    }
}

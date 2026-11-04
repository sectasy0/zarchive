//! Validates PAR2 metadata before accessing protected files or allocating repair rows.

use super::*;
use md5::{Digest, Md5};
use std::io::{self, Read, Seek, SeekFrom};

const MAX_METADATA: u64 = 16 * 1024 * 1024;
const MAX_SLICE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_SOURCE_BLOCKS: u64 = 8192;
const MAX_REPAIR_BYTES: u64 = 128 * 1024 * 1024;
const MAX_MISSING_BLOCKS: usize = 256;

pub(super) fn limits() -> serde_json::Value {
    serde_json::json!({ "metadata_bytes": MAX_METADATA, "slice_bytes": MAX_SLICE_BYTES,
        "source_blocks": MAX_SOURCE_BLOCKS, "missing_blocks": MAX_MISSING_BLOCKS, "repair_bytes": MAX_REPAIR_BYTES,
        "flat_filenames_only": true })
}

struct MetadataReader {
    file: File,
    remaining: u64,
}

impl Read for MetadataReader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        crate::task_queue::checkpoint()?;
        if bytes.is_empty() {
            return Ok(0);
        }
        if self.remaining == 0 {
            return Err(io::Error::other("PAR2 metadata safety limit reached"));
        }
        let length = self.remaining.min(bytes.len() as u64) as usize;
        let count = self.file.read(&mut bytes[..length])?;
        self.remaining -= count as u64;
        Ok(count)
    }
}

impl Seek for MetadataReader {
    fn seek(&mut self, offset: SeekFrom) -> io::Result<u64> {
        crate::task_queue::checkpoint()?;
        self.file.seek(offset)
    }
}

fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name.starts_with('-')
        && !name
            .chars()
            .any(|character| character.is_control() || matches!(character, '/' | '\\' | ':'))
        && !name.ends_with(['.', ' '])
        && name != "."
        && name != ".."
        && !{
            let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
            matches!(
                stem.as_str(),
                "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
            ) || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && matches!(stem.as_bytes()[3], b'1'..=b'9'))
        }
}

fn metadata_packets(path: &Path) -> Result<([u8; 16], Vec<u8>)> {
    let metadata = std::fs::symlink_metadata(path)?;
    anyhow::ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() <= MAX_SET_BYTES,
        "PAR2 requires bounded regular files without symlinks"
    );
    let mut reader = MetadataReader {
        file: File::open(path)?,
        remaining: MAX_METADATA,
    };
    let mut packets = Vec::new();
    let mut set_id = None;
    let mut count = 0;
    while reader.stream_position()? < metadata.len() {
        let start = reader.stream_position()?;
        let mut header = [0u8; 64];
        reader.read_exact(&mut header)?;
        let length = u64::from_le_bytes(header[8..16].try_into()?);
        anyhow::ensure!(
            header[..8] == *b"PAR2\0PKT"
                && length >= 64
                && length % 4 == 0
                && length <= metadata.len() - start,
            "Invalid or truncated PAR2 packet"
        );
        let id: [u8; 16] = header[32..48].try_into()?;
        anyhow::ensure!(
            set_id.get_or_insert(id) == &id,
            "PAR2 contains conflicting recovery sets"
        );
        count += 1;
        anyhow::ensure!(count <= 20_000, "PAR2 packet safety limit reached");
        if matches!(
            &header[48..64],
            b"PAR 2.0\0Main\0\0\0\0" | b"PAR 2.0\0FileDesc" | b"PAR 2.0\0IFSC\0\0\0\0"
        ) {
            anyhow::ensure!(
                length <= MAX_METADATA - packets.len() as u64,
                "PAR2 metadata safety limit reached"
            );
            let mut body = vec![0; (length - 64) as usize];
            reader.read_exact(&mut body)?;
            let mut hash = Md5::new();
            hash.update(&header[32..]);
            hash.update(&body);
            anyhow::ensure!(
                hash.finalize()[..] == header[16..32],
                "PAR2 metadata checksum mismatch"
            );
            if &header[48..64] == b"PAR 2.0\0Main\0\0\0\0" {
                anyhow::ensure!(
                    Md5::digest(&body)[..] == id,
                    "PAR2 main packet set ID mismatch"
                );
            }
            packets.extend_from_slice(&header);
            packets.extend_from_slice(&body);
        } else {
            reader.seek(SeekFrom::Start(start + length))?;
        }
    }
    Ok((set_id.context("No PAR2 packets found")?, packets))
}

fn parse(paths: &[PathBuf]) -> Result<rust_par2::Par2FileSet> {
    let mut packets = Vec::new();
    let mut set_id = None;
    let mut identities = std::collections::BTreeMap::new();
    for path in paths {
        let (id, metadata) = metadata_packets(path)?;
        anyhow::ensure!(
            *set_id.get_or_insert(id) == id,
            "Conflicting PAR2 recovery sets"
        );
        let mut offset = 0;
        while offset < metadata.len() {
            let length = u64::from_le_bytes(metadata[offset + 8..offset + 16].try_into()?) as usize;
            let packet = &metadata[offset..offset + length];
            let kind: [u8; 16] = packet[48..64].try_into()?;
            let file_id = if &kind == b"PAR 2.0\0Main\0\0\0\0" {
                [0; 16]
            } else {
                packet
                    .get(64..80)
                    .context("Truncated PAR2 metadata")?
                    .try_into()?
            };
            let hash: [u8; 16] = packet[16..32].try_into()?;
            if let Some(previous) = identities.insert((kind, file_id), hash) {
                anyhow::ensure!(previous == hash, "Conflicting PAR2 file metadata");
            } else {
                anyhow::ensure!(
                    packets.len() as u64 + length as u64 <= MAX_METADATA,
                    "PAR2 metadata safety limit reached"
                );
                packets.extend_from_slice(packet);
            }
            offset += length;
        }
    }
    let length = packets.len() as u64;
    let set = rust_par2::parse_par2_reader(&mut io::Cursor::new(packets), length)?;
    validate(&set)?;
    Ok(set)
}

fn validate(set: &rust_par2::Par2FileSet) -> Result<()> {
    anyhow::ensure!(
        set.slice_size > 0 && set.slice_size <= MAX_SLICE_BYTES && set.slice_size % 4 == 0,
        "PAR2 slice size exceeds the safety limit"
    );
    anyhow::ensure!(
        !set.file_order.is_empty()
            && set.file_order.len() <= MAX_SET_FILES
            && set.files.len() == set.file_order.len(),
        "PAR2 file count exceeds the safety limit"
    );
    let mut names = std::collections::BTreeSet::new();
    let mut ids = std::collections::BTreeSet::new();
    let mut total = 0u64;
    let mut blocks = 0u64;
    for id in &set.file_order {
        let file = set
            .files
            .get(id)
            .context("PAR2 protected file metadata is incomplete")?;
        anyhow::ensure!(
            ids.insert(*id)
                && safe_name(&file.filename)
                && names.insert(file.filename.to_lowercase()),
            "PAR2 contains unsafe or conflicting filenames"
        );
        anyhow::ensure!(
            !file.filename.to_ascii_lowercase().ends_with(".par2"),
            "PAR2 metadata cannot overwrite recovery volumes"
        );
        total = total
            .checked_add(file.size)
            .context("PAR2 file size exceeds the safety limit")?;
        blocks = blocks
            .checked_add(file.size.div_ceil(set.slice_size))
            .context("PAR2 block count exceeds the safety limit")?;
        anyhow::ensure!(
            total <= MAX_SET_BYTES && blocks <= MAX_SOURCE_BLOCKS,
            "PAR2 repair exceeds the safety limit"
        );
        anyhow::ensure!(
            file.slices.len() as u64 == file.size.div_ceil(set.slice_size),
            "PAR2 slice checksums are incomplete"
        );
    }
    Ok(())
}

fn key(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    let stem = name.strip_suffix(".par2")?;
    Some(
        stem.rsplit_once(".vol")
            .filter(|(_, suffix)| {
                suffix.split_once('+').is_some_and(|(first, second)| {
                    !first.is_empty()
                        && !second.is_empty()
                        && first
                            .bytes()
                            .chain(second.bytes())
                            .all(|byte| byte.is_ascii_digit())
                })
            })
            .map_or(stem, |(base, _)| base)
            .to_owned(),
    )
}

pub(super) fn find(path: &Path) -> Option<PathBuf> {
    if key(path).is_some() {
        return Some(path.to_owned());
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path.file_name()?.to_str()?;
    let stem = path.file_stem()?.to_str()?;
    [
        parent.join(format!("{name}.par2")),
        parent.join(format!("{stem}.par2")),
    ]
    .into_iter()
    .find(|candidate| candidate.is_file())
}

fn copy_private(source: &Path, destination: &Path, total: &mut u64) -> Result<()> {
    let metadata = std::fs::symlink_metadata(source)?;
    anyhow::ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "PAR2 requires regular files without symlinks"
    );
    *total = total
        .checked_add(metadata.len())
        .context("PAR2 input size limit reached")?;
    anyhow::ensure!(*total <= MAX_SET_BYTES, "PAR2 input size limit reached");
    let mut input = File::open(source)?;
    let mut output = File::options()
        .create_new(true)
        .write(true)
        .open(destination)?;
    let copied =
        crate::task_queue::copy(&mut input.by_ref().take(metadata.len() + 1), &mut output)?;
    anyhow::ensure!(
        copied == metadata.len(),
        "PAR2 source changed during staging"
    );
    Ok(())
}

fn expired(started: Instant) -> Result<()> {
    crate::task_queue::checkpoint()?;
    anyhow::ensure!(
        started.elapsed().as_secs() < MAX_ENGINE_SECONDS,
        "PAR2 time safety limit reached"
    );
    Ok(())
}

fn missing_blocks(
    set: &rust_par2::Par2FileSet,
    map: &rust_par2::BlockMap,
    directory: &Path,
) -> Result<Vec<usize>> {
    let mut missing = Vec::new();
    let mut buffer = vec![0; set.slice_size as usize];
    for index in 0..map.total_blocks as usize {
        crate::task_queue::checkpoint()?;
        let location = map
            .global_to_file(index)
            .context("Invalid PAR2 block map")?;
        let mut file = File::open(directory.join(&location.file.filename))?;
        file.seek(SeekFrom::Start(location.offset))?;
        file.read_exact(&mut buffer[..location.len])?;
        if !set.files[&location.file.file_id].slice_matches(
            location.local_index as usize,
            &buffer[..location.len],
            set.slice_size,
        ) {
            missing.push(index);
        }
    }
    Ok(missing)
}

pub(super) fn reconstruct(source: &Path, selected: &Path) -> Result<Reconstruction> {
    let started = Instant::now();
    let family = key(selected).context("Invalid PAR2 filename")?;
    let parent = selected.parent().unwrap_or_else(|| Path::new("."));
    let directory = tempfile::tempdir()?;
    let mut total = 0;
    let mut volumes = Vec::new();
    for (count, entry) in std::fs::read_dir(parent)?.enumerate() {
        expired(started)?;
        anyhow::ensure!(count < 20_000, "PAR2 directory safety limit reached");
        let path = entry?.path();
        if key(&path).as_ref() != Some(&family) {
            continue;
        }
        anyhow::ensure!(
            volumes.len() < MAX_SET_FILES,
            "PAR2 volume count safety limit reached"
        );
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .context("Invalid PAR2 volume name")?;
        anyhow::ensure!(safe_name(name), "Unsafe PAR2 volume name");
        let copy = directory.path().join(name);
        copy_private(&path, &copy, &mut total)?;
        volumes.push(copy);
    }
    volumes.sort();
    let selected_copy = directory
        .path()
        .join(selected.file_name().context("Invalid PAR2 source")?);
    let set = parse(&volumes)?;
    let mut names = std::collections::BTreeSet::new();
    for volume in &volumes {
        names.insert(volume.file_name().unwrap().to_string_lossy().to_lowercase());
    }
    let mut before = std::collections::BTreeMap::new();
    let mut archives = Vec::new();
    for id in &set.file_order {
        expired(started)?;
        let file = &set.files[id];
        anyhow::ensure!(
            names.insert(file.filename.to_lowercase()),
            "PAR2 filename collides with recovery metadata"
        );
        let original = parent.join(&file.filename);
        let copy = directory.path().join(&file.filename);
        match std::fs::symlink_metadata(&original) {
            Ok(_) => {
                copy_private(&original, &copy, &mut total)?;
                before.insert(file.filename.clone(), digest(&copy)?);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                File::options().write(true).create_new(true).open(&copy)?;
            }
            Err(error) => return Err(error.into()),
        }
        File::options()
            .write(true)
            .open(&copy)?
            .set_len(file.size)?;
        archives.push(copy);
    }
    let map = rust_par2::BlockMap::new(&set);
    let missing = missing_blocks(&set, &map, directory.path())?;
    let mut limited = false;
    if !missing.is_empty() {
        anyhow::ensure!(
            missing.len() <= MAX_MISSING_BLOCKS
                && (missing.len() as u64 * set.slice_size * 3) <= MAX_REPAIR_BYTES,
            "PAR2 repair memory safety limit reached"
        );
        let mut rows =
            rust_par2::recovery::RecoveryReader::new(set.recovery_set_id, set.slice_size)
                .with_budget(rust_par2::recovery::RecoveryBudget::rows(missing.len()))
                .with_max_volume_bytes(MAX_SET_BYTES);
        for volume in &volumes {
            expired(started)?;
            let scan = rows.read_volume(MetadataReader {
                file: File::open(volume)?,
                remaining: MAX_SET_BYTES,
            });
            limited |= !scan.read_to_end();
        }
        expired(started)?;
        if rows.kept() >= missing.len() {
            let mut repair = rust_par2::Repair::new_owning(
                rows.into_blocks(),
                set.slice_size as usize,
                map.total_blocks as usize,
            )?;
            let mut buffer = vec![0; set.slice_size as usize];
            for index in 0..map.total_blocks as usize {
                expired(started)?;
                if missing.binary_search(&index).is_ok() {
                    continue;
                }
                let location = map
                    .global_to_file(index)
                    .context("Invalid PAR2 block map")?;
                let mut file = File::open(directory.path().join(&location.file.filename))?;
                file.seek(SeekFrom::Start(location.offset))?;
                file.read_exact(&mut buffer[..location.len])?;
                repair.feed(index, &buffer[..location.len])?;
            }
            for (index, bytes) in repair.solve(&missing)? {
                expired(started)?;
                let location = map
                    .global_to_file(index)
                    .context("Invalid PAR2 block map")?;
                let mut file = File::options()
                    .write(true)
                    .open(directory.path().join(&location.file.filename))?;
                file.seek(SeekFrom::Start(location.offset))?;
                std::io::Write::write_all(&mut file, &bytes[..location.len])?;
            }
        }
    }
    let mut files = Vec::new();
    let mut changed_files = Vec::new();
    for id in &set.file_order {
        expired(started)?;
        let file = &set.files[id];
        let copy = directory.path().join(&file.filename);
        let mut input = File::open(&copy)?;
        let mut hash = Md5::new();
        let mut buffer = vec![0; 65536];
        loop {
            expired(started)?;
            let count = input.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
        }
        let verified = hash.finalize()[..] == file.hash;
        if verified && before.get(&file.filename) != Some(&digest(&copy)?) {
            changed_files.push(file.filename.clone());
        }
        files.push(ReconstructedFile {
            name: file.filename.clone(),
            size: file.size,
            path: copy,
            verified,
        });
    }
    let all_verified = files.iter().all(|file| file.verified);
    let archive = files
        .iter()
        .filter(|file| file.verified)
        .find(|file| file.path.file_name() == source.file_name() && key(source).is_none())
        .or_else(|| {
            files.iter().find(|file| {
                file.verified && crate::archive::ResolvedFormat::from_path(&file.path).is_some()
            })
        })
        .map(|file| file.path.clone());
    Ok(Reconstruction {
        status: if all_verified {
            if changed_files.is_empty() {
                Status::NoChange
            } else {
                Status::Reconstructed
            }
        } else if limited {
            Status::Limited
        } else {
            Status::Failed
        },
        archive,
        method: "PAR2 Reed-Solomon with MD5 verification",
        exit_code: None,
        changed_files,
        files,
        workspace: Some(Workspace {
            directory,
            selected: selected_copy,
            archives,
            recovery_volumes: volumes,
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(id: [u8; 16], kind: &[u8; 16], body: &[u8]) -> Vec<u8> {
        let mut data = id.to_vec();
        data.extend_from_slice(kind);
        data.extend_from_slice(body);
        while data.len() % 4 != 0 {
            data.push(0);
        }
        let mut packet = b"PAR2\0PKT".to_vec();
        packet.extend_from_slice(&(32 + data.len() as u64).to_le_bytes());
        packet.extend_from_slice(&Md5::digest(&data));
        packet.extend(data);
        packet
    }

    fn fixture(directory: &Path, name: &str, payload: &[u8], parity: bool) -> PathBuf {
        assert!(payload.len() <= 4096);
        let full_hash = Md5::digest(payload);
        let mut identity = full_hash.to_vec();
        identity.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        identity.extend_from_slice(name.as_bytes());
        let file_id: [u8; 16] = Md5::digest(&identity).into();
        let mut main = 4096u64.to_le_bytes().to_vec();
        main.extend_from_slice(&1u32.to_le_bytes());
        main.extend(file_id);
        let set_id: [u8; 16] = Md5::digest(&main).into();
        let mut bytes = packet(set_id, b"PAR 2.0\0Main\0\0\0\0", &main);
        let mut description = file_id.to_vec();
        description.extend_from_slice(&full_hash);
        description.extend_from_slice(&full_hash);
        description.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        description.extend_from_slice(name.as_bytes());
        bytes.extend(packet(set_id, b"PAR 2.0\0FileDesc", &description));
        let mut padded = payload.to_vec();
        padded.resize(4096, 0);
        let mut checksums = file_id.to_vec();
        checksums.extend_from_slice(&Md5::digest(&padded));
        checksums.extend_from_slice(&crc32fast::hash(&padded).to_le_bytes());
        bytes.extend(packet(set_id, b"PAR 2.0\0IFSC\0\0\0\0", &checksums));
        if parity {
            let mut recovery = 0u32.to_le_bytes().to_vec();
            recovery.extend(padded);
            bytes.extend(packet(set_id, b"PAR 2.0\0RecvSlic", &recovery));
        }
        let path = directory.join("set.par2");
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn par2_reconstructs_missing_and_corrupt_files_verifies_md5_and_preserves_originals() {
        for missing in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let payload = b"PAR2 reconstruction protects arbitrary files";
            let source = fixture(root.path(), "notes.txt", payload, true);
            let protected = root.path().join("notes.txt");
            if !missing {
                std::fs::write(&protected, b"damaged bytes").unwrap();
            }
            let parity_before = std::fs::read(&source).unwrap();
            let result = super::super::reconstruct(&source).unwrap();
            assert_eq!(result.status, Status::Reconstructed);
            assert!(result.files[0].verified);
            assert_eq!(std::fs::read(&result.files[0].path).unwrap(), payload);
            assert_eq!(result.json()["protected_files"][0]["md5_verified"], true);
            let report = crate::recovery::recover_with_parity(&source, None, false, true).unwrap();
            let output = root.path().join("recovered.zip");
            report.export_zip(&output).unwrap();
            let mut archive = zip::ZipArchive::new(File::open(output).unwrap()).unwrap();
            let mut recovered = Vec::new();
            archive
                .by_name("recovered/00001-notes.txt")
                .unwrap()
                .read_to_end(&mut recovered)
                .unwrap();
            assert_eq!(recovered, payload);
            assert_eq!(std::fs::read(&source).unwrap(), parity_before);
            if missing {
                assert!(!protected.exists());
            } else {
                assert_eq!(std::fs::read(protected).unwrap(), b"damaged bytes");
            }
        }
    }

    #[test]
    fn par2_insufficient_or_corrupt_recovery_is_not_exported_as_verified_data() {
        for corrupt in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let source = fixture(root.path(), "notes.txt", b"protected bytes", corrupt);
            if corrupt {
                let mut bytes = std::fs::read(&source).unwrap();
                *bytes.last_mut().unwrap() ^= 1;
                std::fs::write(&source, bytes).unwrap();
            }
            let result = super::super::reconstruct(&source).unwrap();
            assert_eq!(result.status, Status::Failed);
            assert!(!result.files[0].verified);
            let report = crate::recovery::recover_with_parity(&source, None, false, true).unwrap();
            let output = root.path().join("recovered.zip");
            report.export_zip(&output).unwrap();
            let archive = zip::ZipArchive::new(File::open(output).unwrap()).unwrap();
            assert_eq!(archive.len(), 1);
        }
    }

    #[test]
    fn par2_merges_metadata_with_recovery_only_volumes_and_rejects_conflicting_descriptions() {
        let root = tempfile::tempdir().unwrap();
        let payload = b"protected by a separate PAR2 recovery volume";
        let source = fixture(root.path(), "notes.txt", payload, true);
        let mut bytes = std::fs::read(&source).unwrap();
        let offset = bytes
            .windows(16)
            .position(|window| window == b"PAR 2.0\0RecvSlic")
            .unwrap()
            - 48;
        let recovery = bytes.split_off(offset);
        std::fs::write(&source, &bytes).unwrap();
        let volume = root.path().join("set.vol00+01.par2");
        std::fs::write(&volume, recovery).unwrap();
        let result = super::super::reconstruct(&volume).unwrap();
        assert_eq!(result.status, Status::Reconstructed);
        assert!(result.files[0].verified);
        assert_eq!(std::fs::read(&result.files[0].path).unwrap(), payload);
        let mut conflicting = bytes;
        let description = conflicting
            .windows(16)
            .position(|window| window == b"PAR 2.0\0FileDesc")
            .unwrap()
            - 48;
        conflicting[description + 80] ^= 1;
        let length = u64::from_le_bytes(
            conflicting[description + 8..description + 16]
                .try_into()
                .unwrap(),
        ) as usize;
        let hash = Md5::digest(&conflicting[description + 32..description + length]);
        conflicting[description + 16..description + 32].copy_from_slice(&hash);
        std::fs::write(volume, conflicting).unwrap();
        assert!(super::super::reconstruct(&source).is_err());
    }

    #[test]
    fn par2_rejects_mixed_sets_and_symlinked_protected_files() {
        let root = tempfile::tempdir().unwrap();
        let source = fixture(root.path(), "notes.txt", b"protected bytes", true);
        let other = tempfile::tempdir().unwrap();
        let other_source = fixture(other.path(), "other.txt", b"other bytes", true);
        let volume = root.path().join("set.vol00+01.par2");
        std::fs::copy(other_source, &volume).unwrap();
        assert!(super::super::reconstruct(&source).is_err());
        std::fs::remove_file(volume).unwrap();
        #[cfg(unix)]
        {
            let outside = other.path().join("outside.txt");
            std::fs::write(&outside, b"outside original").unwrap();
            std::os::unix::fs::symlink(&outside, root.path().join("notes.txt")).unwrap();
            assert!(super::super::reconstruct(&source).is_err());
            assert_eq!(std::fs::read(outside).unwrap(), b"outside original");
        }
    }

    fn set(name: &str) -> rust_par2::Par2FileSet {
        let id = [1u8; 16];
        rust_par2::Par2FileSet {
            recovery_set_id: [2; 16],
            slice_size: 4,
            file_order: vec![id],
            files: std::collections::HashMap::from([(
                id,
                rust_par2::Par2File {
                    file_id: id,
                    hash: [0; 16],
                    hash_16k: [0; 16],
                    size: 4,
                    filename: name.to_owned(),
                    slices: vec![rust_par2::SliceChecksum {
                        md5: [0; 16],
                        crc32: 0,
                    }],
                },
            )]),
            recovery_block_count: 1,
            creator: None,
        }
    }

    #[test]
    fn par2_metadata_rejects_escaping_names_devices_and_excessive_sizes() {
        validate(&set("archive.zip.001")).unwrap();
        for name in [
            "../outside",
            "/outside",
            "folder/file",
            "folder\\file",
            "C:stream",
            "NUL.txt",
            "con",
            "file. ",
            "-switch",
            "file.par2",
        ] {
            assert!(validate(&set(name)).is_err(), "{name}");
        }
        let mut oversized = set("archive.zip");
        oversized.slice_size = u64::MAX;
        assert!(validate(&oversized).is_err());
        let mut incomplete = set("archive.zip");
        incomplete.files.get_mut(&[1; 16]).unwrap().slices.clear();
        assert!(validate(&incomplete).is_err());
    }
}

//! Reconstructs bounded private copies through native and optional external engines.

mod par2;
mod create;
mod split_zip;
pub(crate) use create::create_par2;
pub(crate) use split_zip::{
    is_split as is_split_zip, layout as split_zip_layout, reconstruct as reconstruct_split_zip,
};

use anyhow::{Context, Result};
use std::{
    fs::File,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const MAX_SET_FILES: usize = 128;
const MAX_SET_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_WORKSPACE_BYTES: u64 = 3 * MAX_SET_BYTES;
const MAX_ENGINE_SECONDS: u64 = 120;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    Reconstructed,
    StructureOnly,
    NoChange,
    ToolMissing,
    Failed,
    Limited,
    Merged,
    ZipToolMissing,
    IncompleteVolumes,
    SplitInvalid,
}

impl Status {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Reconstructed => {
                "Parity reconstruction produced a private repaired copy; file checksums are checked separately"
            }
            Self::StructureOnly => "RAR rebuilt the structure without parity reconstruction",
            Self::NoChange => "Parity reconstruction did not produce changed archive data",
            Self::ToolMissing => {
                "Install the RAR command-line tool to reconstruct recovery records and REV volumes"
            }
            Self::Failed => {
                "Parity reconstruction failed or there is not enough valid recovery data"
            }
            Self::Limited => "Parity reconstruction safety limit reached",
            Self::Merged => {
                "Split ZIP volumes were merged into a private copy; entry checksums are checked separately"
            }
            Self::ZipToolMissing => "Install Info-ZIP to reconstruct native split ZIP archives",
            Self::IncompleteVolumes => {
                "Split ZIP has missing volumes; available local entries are scanned"
            }
            Self::SplitInvalid => {
                "Split ZIP conversion failed validation; available local entries are scanned"
            }
        }
    }
}

pub(crate) struct Reconstruction {
    pub status: Status,
    pub archive: Option<PathBuf>,
    pub method: &'static str,
    pub exit_code: Option<i32>,
    pub changed_files: Vec<String>,
    pub files: Vec<ReconstructedFile>,
    workspace: Option<Workspace>,
}

pub(crate) struct ReconstructedFile {
    pub name: String,
    pub size: u64,
    pub path: PathBuf,
    pub verified: bool,
}

impl Reconstruction {
    pub(crate) fn json(&self) -> serde_json::Value {
        let mut report = serde_json::json!({ "status": self.status.label(), "method": self.method,
            "exit_code": self.exit_code, "changed_files": self.changed_files,
            "protected_files": self.files.iter().map(|file| serde_json::json!({"name": file.name, "size": file.size, "md5_verified": file.verified})).collect::<Vec<_>>(),
            "candidate_created": self.archive.is_some(), "data_range_source": if self.archive.is_some() { "private_repaired_copy" } else { "original" },
            "limits": { "input_files": MAX_SET_FILES, "input_bytes": MAX_SET_BYTES,
                "workspace_bytes": MAX_WORKSPACE_BYTES, "engine_seconds": MAX_ENGINE_SECONDS } });
        if self.method.starts_with("PAR2") {
            report["limits"]["par2"] = par2::limits();
        }
        report
    }
}

pub(crate) fn supported(path: &Path) -> bool {
    (family(path).is_some() && !is_split_zip(path)) || par2::find(path).is_some()
}

fn find_tool() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("ZARCHIVE_RAR_TOOL") {
        let path = PathBuf::from(path);
        return path.is_file().then(|| path.canonicalize().ok()).flatten();
    }
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|directory| directory.join(if cfg!(windows) { "rar.exe" } else { "rar" }))
            .find_map(|candidate| {
                candidate
                    .is_file()
                    .then(|| candidate.canonicalize().ok())
                    .flatten()
            })
    })
}

pub(crate) fn reconstruct(source: &Path) -> Result<Reconstruction> {
    if let Some(par2) = par2::find(source) {
        return par2::reconstruct(source, &par2);
    }
    if is_split_zip(source) {
        return reconstruct_split_zip(source);
    }
    reconstruct_with_tool(source, find_tool().as_deref())
}

fn reconstruct_with_tool(source: &Path, tool: Option<&Path>) -> Result<Reconstruction> {
    let Some(tool) = tool else {
        return Ok(Reconstruction {
            status: Status::ToolMissing,
            archive: None,
            method: "RAR",
            exit_code: None,
            changed_files: Vec::new(),
            files: Vec::new(),
            workspace: None,
        });
    };
    let workspace = match stage(source) {
        Ok(workspace) => workspace,
        Err(error) => {
            crate::task_queue::checkpoint()?;
            return Err(error);
        }
    };
    let mut before = std::collections::BTreeMap::new();
    for path in &workspace.archives {
        before.insert(path.file_name().unwrap().to_os_string(), digest(path)?);
    }
    let rev = !workspace.recovery_volumes.is_empty();
    let method = if rev {
        "RAR recovery volumes"
    } else {
        "RAR recovery record"
    };
    let input = if rev {
        workspace.recovery_volumes[0].clone()
    } else {
        workspace
            .archives
            .first()
            .context("No matching RAR or REV files were found")?
            .clone()
    };
    let mut command = Command::new(tool);
    command
        .current_dir(workspace.directory.path())
        .env_remove("RAR")
        .args([
            if rev { "rc" } else { "r" },
            "-cfg-",
            "-idq",
            "-y",
            "-p-",
            "-mt2",
            "--",
        ])
        .arg(input.file_name().unwrap())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    crate::task_queue::checkpoint()?;
    let outcome = match run_engine(&mut command, workspace.directory.path()) {
        Ok(outcome) => outcome,
        Err(error) if error.to_string() == "Could not start the RAR parity engine" => {
            return Ok(Reconstruction {
                status: Status::ToolMissing,
                archive: None,
                method,
                exit_code: None,
                changed_files: Vec::new(),
                files: Vec::new(),
                workspace: Some(workspace),
            });
        }
        Err(error) => return Err(error),
    };
    let mut result = Reconstruction {
        status: if outcome.limited {
            Status::Limited
        } else {
            Status::Failed
        },
        archive: None,
        method,
        exit_code: outcome.code,
        changed_files: Vec::new(),
        files: Vec::new(),
        workspace: Some(workspace),
    };
    if outcome.limited
        || workspace_size(result.workspace.as_ref().unwrap().directory.path())
            .map_or(true, |bytes| bytes > MAX_WORKSPACE_BYTES)
    {
        result.status = Status::Limited;
        return Ok(result);
    }
    let workspace = result.workspace.as_ref().unwrap();
    let mut archives = Vec::new();
    for entry in std::fs::read_dir(workspace.directory.path())? {
        let path = entry?.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            continue;
        }
        let Some((_, recovery)) = family(&path) else {
            continue;
        };
        if recovery {
            continue;
        }
        archives.push(path.clone());
        let name = path.file_name().unwrap().to_os_string();
        if before.get(&name) != Some(&digest(&path)?) {
            result
                .changed_files
                .push(name.to_string_lossy().into_owned());
        }
    }
    archives.sort();
    if rev {
        let first = unrar::Archive::new(&workspace.selected).first_part();
        result.archive = (first.is_file() && family(&first).is_some_and(|(_, recovery)| !recovery))
            .then_some(first)
            .or_else(|| {
                archives.iter().find_map(|path| {
                    let first = unrar::Archive::new(path).first_part();
                    first.is_file().then_some(first)
                })
            })
            .or_else(|| archives.first().cloned());
        result.status = if !result.changed_files.is_empty() {
            Status::Reconstructed
        } else if outcome.success {
            Status::NoChange
        } else {
            Status::Failed
        };
    } else {
        let name = input.file_name().unwrap().to_string_lossy();
        let fixed = workspace.directory.path().join(format!("fixed.{name}"));
        let rebuilt = workspace.directory.path().join(format!("rebuilt.{name}"));
        if fixed.is_file() {
            result.archive = Some(fixed);
            result.status = Status::Reconstructed;
        } else if rebuilt.is_file() {
            result.archive = Some(rebuilt);
            result.status = Status::StructureOnly;
        } else if outcome.success {
            result.status = Status::NoChange;
        }
    }
    if result.status == Status::Failed {
        result.archive = None;
    }
    Ok(result)
}

fn digest(path: &Path) -> Result<[u8; 32]> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0u8; 65536];
    loop {
        crate::task_queue::checkpoint()?;
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(hash.finalize().into())
}

struct ProcessGuard(Child);

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

struct EngineResult {
    code: Option<i32>,
    success: bool,
    limited: bool,
}

fn workspace_size(directory: &Path) -> Result<u64> {
    let mut total = 0u64;
    let mut count = 0usize;
    for entry in std::fs::read_dir(directory)? {
        let metadata = std::fs::symlink_metadata(entry?.path())?;
        count += 1;
        anyhow::ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink() && count <= MAX_SET_FILES * 3,
            "Parity reconstruction safety limit reached"
        );
        total = total
            .checked_add(metadata.len())
            .context("Parity reconstruction safety limit reached")?;
    }
    Ok(total)
}

fn run_engine(command: &mut Command, directory: &Path) -> Result<EngineResult> {
    let mut child = ProcessGuard(
        command
            .spawn()
            .context("Could not start the RAR parity engine")?,
    );
    let started = Instant::now();
    loop {
        if crate::task_queue::cancellation_requested() {
            anyhow::bail!("Task cancelled");
        }
        if started.elapsed() > Duration::from_secs(MAX_ENGINE_SECONDS)
            || workspace_size(directory).map_or(true, |bytes| bytes > MAX_WORKSPACE_BYTES)
        {
            return Ok(EngineResult {
                code: None,
                success: false,
                limited: true,
            });
        }
        if let Some(status) = child.0.try_wait()? {
            crate::task_queue::checkpoint()?;
            return Ok(EngineResult {
                code: status.code(),
                success: status.success(),
                limited: false,
            });
        }
        std::thread::park_timeout(Duration::from_millis(25));
    }
}

pub(crate) struct Workspace {
    directory: tempfile::TempDir,
    pub selected: PathBuf,
    pub archives: Vec<PathBuf>,
    pub recovery_volumes: Vec<PathBuf>,
}

fn numbered_stem(stem: &str) -> &str {
    if let Some((base, suffix)) = stem.rsplit_once(".part") {
        if !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit()) {
            return base;
        }
    }
    if let Some((base, suffix)) = stem.rsplit_once('.') {
        if !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit()) {
            return base;
        }
    }
    stem
}

fn family(path: &Path) -> Option<(String, bool)> {
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    let (stem, extension) = name.rsplit_once('.')?;
    if extension == "rar" {
        return Some((numbered_stem(stem).to_owned(), false));
    }
    if extension == "rev" {
        let numbered = numbered_stem(stem);
        if numbered != stem {
            return Some((numbered.to_owned(), true));
        }
        if let Some((prefix, last)) = stem.rsplit_once('_') {
            if let Some((base, previous)) = prefix.rsplit_once('_') {
                if !last.is_empty()
                    && !previous.is_empty()
                    && last.bytes().all(|byte| byte.is_ascii_digit())
                    && previous.bytes().all(|byte| byte.is_ascii_digit())
                {
                    return Some((base.to_owned(), true));
                }
            }
        }
        return Some((stem.to_owned(), true));
    }
    if extension.len() == 3
        && matches!(extension.as_bytes()[0], b'r'..=b'z')
        && extension.as_bytes()[1..].iter().all(u8::is_ascii_digit)
    {
        return Some((stem.to_owned(), false));
    }
    None
}

pub(crate) fn stage(source: &Path) -> Result<Workspace> {
    let (key, _) = family(source).context("Parity reconstruction requires a RAR or REV file")?;
    let parent = source
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut files = Vec::new();
    let mut total = 0u64;
    for entry in std::fs::read_dir(parent)? {
        crate::task_queue::checkpoint()?;
        let entry = entry?;
        let path = entry.path();
        let Some((candidate, recovery)) = family(&path) else {
            continue;
        };
        if candidate != key {
            continue;
        }
        let metadata = std::fs::symlink_metadata(&path)?;
        anyhow::ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "Parity reconstruction requires regular files without symlinks"
        );
        anyhow::ensure!(
            files.len() < MAX_SET_FILES,
            "Parity reconstruction safety limit reached"
        );
        total = total
            .checked_add(metadata.len())
            .context("Parity reconstruction safety limit reached")?;
        anyhow::ensure!(
            total <= MAX_SET_BYTES,
            "Parity reconstruction safety limit reached"
        );
        files.push((path, recovery, metadata.len()));
    }
    anyhow::ensure!(!files.is_empty(), "No matching RAR or REV files were found");
    let directory = tempfile::tempdir()?;
    let selected = directory
        .path()
        .join(source.file_name().context("The source has no filename")?);
    let mut result = Workspace {
        directory,
        selected,
        archives: Vec::new(),
        recovery_volumes: Vec::new(),
    };
    files.sort_by(|left, right| left.0.cmp(&right.0));
    for (path, recovery, expected_size) in files {
        crate::task_queue::checkpoint()?;
        let destination = result.directory.path().join(path.file_name().unwrap());
        let mut input = File::open(&path)?;
        let mut output = File::options()
            .write(true)
            .create_new(true)
            .open(&destination)?;
        use std::io::Read;
        let copied =
            crate::task_queue::copy(&mut input.by_ref().take(expected_size + 1), &mut output)?;
        anyhow::ensure!(
            copied == expected_size,
            "The archive changed during parity staging"
        );
        output.sync_all()?;
        if recovery {
            result.recovery_volumes.push(destination);
        } else {
            result.archives.push(destination);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parity_staging_copies_only_matching_volumes_and_leaves_originals_unchanged() {
        let root = tempfile::tempdir().unwrap();
        for name in [
            "set.part2.rar",
            "set.part3.rar",
            "set.part1.rev",
            "other.part1.rar",
            "set-notes.txt",
        ] {
            std::fs::write(root.path().join(name), name.as_bytes()).unwrap();
        }
        let source = root.path().join("set.part2.rar");
        let staged = stage(&source).unwrap();
        assert_eq!(staged.archives.len(), 2);
        assert_eq!(staged.recovery_volumes.len(), 1);
        assert!(!staged.directory.path().join("other.part1.rar").exists());
        assert!(!staged.directory.path().join("set-notes.txt").exists());
        std::fs::write(&staged.selected, b"private change").unwrap();
        assert_eq!(std::fs::read(source).unwrap(), b"set.part2.rar");
        let temporary = staged.directory.path().to_owned();
        drop(staged);
        assert!(!temporary.exists());
    }

    #[test]
    fn parity_families_cover_embedded_records_and_legacy_recovery_volumes() {
        for name in [
            "set.rar",
            "set.r00",
            "set.part01.rar",
            "set.001.rar",
            "set.part01.rev",
            "set_1_3.rev",
        ] {
            assert_eq!(family(Path::new(name)).unwrap().0, "set");
        }
        assert!(family(Path::new("set.txt")).is_none());
    }

    #[test]
    fn parity_missing_tool_is_reported_without_touching_the_source() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.rar");
        std::fs::write(&source, b"original bytes").unwrap();
        let result = reconstruct_with_tool(&source, None).unwrap();
        assert_eq!(result.status, Status::ToolMissing);
        assert!(result.archive.is_none());
        assert_eq!(std::fs::read(source).unwrap(), b"original bytes");
    }

    #[test]
    #[ignore = "requires official RAR CLI via PATH or ZARCHIVE_RAR_TOOL"]
    fn parity_real_rar_reconstructs_recovery_record_and_missing_rev_volume() {
        let tool = find_tool()
            .expect("Install rar or set ZARCHIVE_RAR_TOOL to the official RAR executable");
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("payload.bin");
        let mut payload = b"PARITY RECOVERY TEST PAYLOAD START".to_vec();
        payload.extend((0..65536).map(|index| (index * 73 + 19) as u8));
        std::fs::write(&source, &payload).unwrap();
        let create = |name: &str, switches: &[&str]| {
            let status = Command::new(&tool)
                .current_dir(root.path())
                .args(["a", "-cfg-", "-idq", "-y", "-m0"])
                .args(switches)
                .arg(name)
                .arg("payload.bin")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "RAR fixture creation failed: {status}");
        };
        create("record.rar", &["-rr10p"]);
        let path = root.path().join("record.rar");
        let mut bytes = std::fs::read(&path).unwrap();
        let offset = memchr::memmem::find(&bytes, &payload[..32]).unwrap();
        bytes[offset + 777] ^= 0x3c;
        std::fs::write(&path, &bytes).unwrap();
        let result = reconstruct_with_tool(&path, Some(&tool)).unwrap();
        assert_eq!(result.status, Status::Reconstructed, "{}", result.json());
        let fixed = result.archive.as_ref().unwrap();
        let data = unrar::Archive::new(fixed)
            .open_for_processing()
            .unwrap()
            .read_header()
            .unwrap()
            .unwrap()
            .read()
            .unwrap()
            .0;
        assert_eq!(data, payload);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        let recovered = crate::recovery::recover_with_parity(&path, None, false, true).unwrap();
        assert_eq!(recovered.source, path);
        assert!(
            recovered
                .entries
                .iter()
                .any(|entry| entry.status == crate::recovery::Status::Recovered)
        );
        assert_eq!(
            recovered.json()["parity_reconstruction"]["candidate_created"],
            true
        );
        let exported = root.path().join("record-recovered.zip");
        recovered.export_zip(&exported).unwrap();
        let mut archive = zip::ZipArchive::new(File::open(&exported).unwrap()).unwrap();
        let mut restored = Vec::new();
        std::io::Read::read_to_end(
            &mut archive
                .by_name(recovered.entries[0].output_name.as_ref().unwrap())
                .unwrap(),
            &mut restored,
        )
        .unwrap();
        assert_eq!(restored, payload);

        create("without-record.rar", &[]);
        let structural =
            reconstruct_with_tool(&root.path().join("without-record.rar"), Some(&tool)).unwrap();
        assert_eq!(
            structural.status,
            Status::StructureOnly,
            "{}",
            structural.json()
        );

        create("volumes.rar", &["-v16k", "-rv2"]);
        let first = root.path().join("volumes.part1.rar");
        let selected = root.path().join("volumes.part2.rar");
        assert!(first.is_file() && selected.is_file());
        let original = std::fs::read(&selected).unwrap();
        std::fs::remove_file(&first).unwrap();
        let result = reconstruct_with_tool(&selected, Some(&tool)).unwrap();
        assert_eq!(result.status, Status::Reconstructed, "{}", result.json());
        assert!(
            result
                .changed_files
                .iter()
                .any(|name| name == "volumes.part1.rar")
        );
        let data = unrar::Archive::new(result.archive.as_ref().unwrap())
            .open_for_processing()
            .unwrap()
            .read_header()
            .unwrap()
            .unwrap()
            .read()
            .unwrap()
            .0;
        assert_eq!(data, payload);
        assert!(!first.exists());
        assert_eq!(std::fs::read(selected).unwrap(), original);
        let rev = std::fs::read_dir(root.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| {
                path.extension().is_some_and(|extension| extension == "rev")
                    && path
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with("volumes")
            })
            .unwrap();
        let recovered = crate::recovery::recover_with_parity(&rev, None, false, true).unwrap();
        assert!(
            recovered
                .entries
                .iter()
                .any(|entry| entry.status == crate::recovery::Status::Recovered),
            "{}",
            recovered.json()
        );
        assert_eq!(
            recovered.json()["parity_reconstruction"]["method"],
            "RAR recovery volumes"
        );
        assert!(!first.exists());
    }
}

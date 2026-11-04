use super::*;
use par2_rs::create::{Par2Creator, Par2CreatorOptions, RecoveryAmount};
use par2_rs::types::{CancellationToken, ProgressPhase, ProgressStage};
use std::sync::{Arc, Mutex};

const MAX_CREATE_MEMORY: usize = 512 * 1024 * 1024;

pub(crate) fn create_par2(
    archive_path: &Path,
    first_volume: &Path,
    split: bool,
    recovery_percent: u32,
    control: crate::task_queue::TaskControl,
    reporter: crate::task_queue::TaskReporter,
) -> Result<Vec<PathBuf>> {
    anyhow::ensure!((1..=100).contains(&recovery_percent), "PAR2 recovery percentage must be between 1 and 100");
    anyhow::ensure!(control.checkpoint(), "Task cancelled");
    let inputs = if split {
        crate::volumes::paths(first_volume)?
    } else {
        vec![first_volume.to_owned()]
    };
    anyhow::ensure!(!inputs.is_empty() && inputs.len() <= MAX_SET_FILES, "PAR2 input file limit exceeded");
    let mut total_bytes = 0u64;
    for path in &inputs {
        let metadata = std::fs::symlink_metadata(path)?;
        anyhow::ensure!(metadata.is_file() && !metadata.file_type().is_symlink(), "PAR2 inputs must be regular files");
        total_bytes = total_bytes.checked_add(metadata.len()).context("PAR2 input size limit reached")?;
        anyhow::ensure!(total_bytes <= MAX_SET_BYTES, "PAR2 input size limit reached");
    }
    let parent = archive_path.parent().filter(|parent| !parent.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let parent = std::fs::canonicalize(parent).context("The PAR2 output directory is unavailable")?;
    let name = archive_path.file_name().context("The archive path has no filename")?.to_string_lossy();
    let output = parent.join(format!("{name}.par2"));
    reporter.phase("Creating PAR2 recovery files...");
    let cancellation = CancellationToken::new();
    let callback_cancel = cancellation.clone();
    let callback_control = control.clone();
    let callback_reporter = reporter.clone();
    let progress = Arc::new(Mutex::new(None::<(ProgressStage, ProgressPhase, u32, u32)>));
    let callback_progress = progress.clone();
    let mut options = Par2CreatorOptions::with_output(output, Some(parent), inputs);
    options.recovery_amount = RecoveryAmount::Percent(recovery_percent);
    options.memory_limit = Some(MAX_CREATE_MEMORY);
    options.cancellation = cancellation.clone();
    options.progress = Some(Arc::new(move |update| {
        if !callback_control.checkpoint() {
            callback_cancel.cancel();
            return;
        }
        let mut current = callback_progress.lock().unwrap_or_else(|error| error.into_inner());
        let value = match *current {
            Some((stage, phase, previous, total)) if stage == update.stage && phase == update.phase =>
                (stage, phase, previous.max(update.current), total.max(update.total)),
            _ => (update.stage, update.phase, update.current, update.total),
        };
        *current = Some(value);
        callback_reporter.set(u64::from(value.2), u64::from(value.3));
    }));
    let creator = Par2Creator::new(options);
    let plan = creator.plan()?;
    anyhow::ensure!(plan.skipped_empty.is_empty(), "PAR2 cannot protect an empty archive volume");
    anyhow::ensure!(control.checkpoint(), "Task cancelled");
    let outcome = creator.create(&plan)?;
    anyhow::ensure!(outcome.output_paths.iter().all(|path| path.is_file()), "PAR2 files were not completely written");
    Ok(outcome.output_paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn par2_creation_writes_interoperable_recovery_volumes_and_never_overwrites() {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("archive.zip");
        std::fs::write(&archive, b"archive bytes protected by PAR2".repeat(128)).unwrap();
        let outputs = create_par2(&archive, &archive, false, 20, Default::default(), Default::default()).unwrap();
        assert!(outputs.len() >= 2);
        let set = rust_par2::parse(&outputs[0]).unwrap();
        assert_eq!(set.files.len(), 1);
        assert_eq!(set.files.values().next().unwrap().filename, "archive.zip");
        assert!(outputs
            .iter()
            .map(|path| rust_par2::parse(path).unwrap().recovery_block_count)
            .sum::<u32>() > 0);
        assert!(create_par2(&archive, &archive, false, 20, Default::default(), Default::default()).is_err());
        assert!(outputs.iter().all(|path| path.is_file()));
    }

    #[test]
    fn par2_creation_honors_cancellation_before_reading_sources() {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("archive.zip");
        std::fs::write(&archive, b"protected bytes").unwrap();
        let control = crate::task_queue::TaskControl::default();
        control.cancel();
        assert!(create_par2(&archive, &archive, false, 10, control, Default::default()).is_err());
        assert!(!root.path().join("archive.zip.par2").exists());
    }

    #[test]
    fn par2_created_during_creation_restores_and_exports_the_original_archive_format() {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("source.zip");
        let mut writer = zip::ZipWriter::new(File::create(&archive).unwrap());
        writer
            .start_file(
                "notes.txt",
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored),
            )
            .unwrap();
        writer.write_all(b"source format survives PAR2 repair").unwrap();
        writer.finish().unwrap();
        create_par2(
            &archive,
            &archive,
            false,
            100,
            Default::default(),
            Default::default(),
        )
        .unwrap();
        let mut bytes = std::fs::read(&archive).unwrap();
        let mut reader = zip::ZipArchive::new(std::io::Cursor::new(&bytes)).unwrap();
        let data_start = reader.by_index_raw(0).unwrap().data_start().unwrap() as usize;
        drop(reader);
        bytes[data_start] ^= 0x40;
        let damaged = bytes.clone();
        std::fs::write(&archive, bytes).unwrap();

        let result = crate::recovery::recover_with_parity(&archive, None, false, true).unwrap();
        assert_eq!(result.original_format(), Some(crate::archive::ResolvedFormat::Zip));
        let destination = root.path().join("exported.zip");
        result
            .export_format(&destination, result.original_format().unwrap())
            .unwrap();
        let mut recovered = zip::ZipArchive::new(File::open(destination).unwrap()).unwrap();
        let mut payload = String::new();
        recovered
            .by_name("recovered/00001-notes.txt")
            .unwrap()
            .read_to_string(&mut payload)
            .unwrap();
        assert_eq!(payload, "source format survives PAR2 repair");
        assert_eq!(std::fs::read(archive).unwrap(), damaged);
    }
}
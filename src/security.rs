//! Applies conservative security heuristics without executing archived content.

use crate::inspection::{EntryInfo, Report};

pub(crate) const SNIFF_BYTES: usize = 64 * 1024;
pub(crate) const MAX_SIGNATURES: usize = 20_000;

fn contains(bytes: &[u8], signature: &[u8]) -> bool {
    bytes
        .windows(signature.len())
        .any(|window| window == signature)
}

fn signature_matches(extension: &str, detected: &str) -> bool {
    extension == detected
        || matches!(
            (extension, detected),
            ("jpeg", "jpg")
                | ("tif", "tiff")
                | ("htm", "html")
                | ("dll" | "sys" | "scr" | "com" | "cpl", "exe")
                | ("tgz", "gz")
                | ("tbz" | "tbz2", "bz2")
                | ("txz", "xz")
                | ("pax", "tar")
                | (
                    "docx" | "xlsx" | "pptx" | "odt" | "ods" | "odp" | "epub" | "jar" | "apk",
                    "zip"
                )
        )
}

pub(crate) fn content(report: &mut Report, name: &str, prefix: &[u8], tail: &[u8]) {
    if report.content_signatures.len() >= MAX_SIGNATURES {
        report.limited = true;
        report.finding(
            "Inspection limit reached",
            name,
            "Content signature limit reached",
        );
        return;
    }
    let extension = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .unwrap_or_default();
    let detected = infer::get(prefix);
    let pe = prefix.starts_with(b"MZ")
        && prefix
            .get(0x3c..0x40)
            .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
            .map(u32::from_le_bytes)
            .and_then(|offset| usize::try_from(offset).ok())
            .is_some_and(|offset| {
                offset
                    .checked_add(4)
                    .and_then(|end| prefix.get(offset..end))
                    == Some(b"PE\0\0")
            });
    let elf = prefix.starts_with(b"\x7fELF")
        && matches!(prefix.get(4), Some(1 | 2))
        && matches!(prefix.get(5), Some(1 | 2));
    let macho = prefix.get(..4).is_some_and(|bytes| {
        matches!(
            bytes,
            [0xfe, 0xed, 0xfa, 0xce]
                | [0xfe, 0xed, 0xfa, 0xcf]
                | [0xce, 0xfa, 0xed, 0xfe]
                | [0xcf, 0xfa, 0xed, 0xfe]
        )
    });
    let script = prefix.starts_with(b"#!");
    let executable =
        pe || elf || macho || script || detected.is_some_and(|kind| kind.extension() == "exe");
    if let Some(kind) = detected {
        report
            .content_signatures
            .insert(name.to_owned(), kind.mime_type().to_owned());
        if !extension.is_empty() && !signature_matches(&extension, kind.extension()) {
            report.finding(
                "Format mismatch",
                name,
                "Detected content signature does not match the filename",
            );
        }
    } else if executable {
        report.content_signatures.insert(
            name.to_owned(),
            if pe {
                "PE executable"
            } else if elf {
                "ELF executable"
            } else if macho {
                "Mach-O executable"
            } else {
                "Executable script"
            }
            .into(),
        );
    }
    if executable
        && !report
            .findings
            .iter()
            .any(|finding| finding.kind == "Suspicious executable" && finding.entry == name)
    {
        report.finding(
            "Suspicious executable",
            name,
            "Executable or script signature detected; content was not executed",
        );
    }
    if (pe
        && !matches!(
            extension.as_str(),
            "exe" | "dll" | "sys" | "scr" | "com" | "cpl"
        ))
        || ((elf || macho || script)
            && matches!(
                extension.as_str(),
                "txt" | "pdf" | "png" | "jpg" | "jpeg" | "csv"
            ))
    {
        report.finding(
            "Format mismatch",
            name,
            "Executable signature is disguised by a document or image filename",
        );
    }
    let primary_image_or_document = prefix.starts_with(b"%PDF-")
        || prefix.starts_with(b"\x89PNG\r\n\x1a\n")
        || prefix.starts_with(b"\xff\xd8\xff")
        || prefix.starts_with(b"GIF87a")
        || prefix.starts_with(b"GIF89a");
    let has_zip = contains(prefix, b"PK\x03\x04")
        && contains(tail, b"PK\x01\x02")
        && tail.windows(22).enumerate().any(|(offset, window)| {
            window.starts_with(b"PK\x05\x06")
                && offset + 22 + u16::from_le_bytes([window[20], window[21]]) as usize == tail.len()
        });
    let image_with_pdf = !prefix.starts_with(b"%PDF-")
        && primary_image_or_document
        && contains(&prefix[..prefix.len().min(1024)], b"%PDF-")
        && contains(tail, b"%%EOF");
    if ((primary_image_or_document || executable) && has_zip) || image_with_pdf {
        report.finding("Polyglot file", name,
            "Signatures suggest multiple file formats; this can also be a legitimate self-extracting or embedded container");
    }
}

pub(crate) fn hard_link(report: &mut Report, name: &str, target: &str) {
    let target = target.replace('\\', "/");
    let mut depth = 0usize;
    let mut unsafe_target = target.starts_with('/') || target.contains(':');
    for component in target.split('/') {
        match component {
            ".." if depth == 0 => unsafe_target = true,
            ".." => depth -= 1,
            "" | "." => {}
            _ => depth += 1,
        }
    }
    if unsafe_target {
        report.finding(
            "Suspicious link",
            name,
            "A hard link target may escape the archive root",
        );
    }
}

fn executable_extension(extension: &str) -> bool {
    matches!(
        extension,
        "exe"
            | "dll"
            | "sys"
            | "scr"
            | "com"
            | "cpl"
            | "msi"
            | "msp"
            | "bat"
            | "cmd"
            | "ps1"
            | "vbs"
            | "vbe"
            | "js"
            | "jse"
            | "wsf"
            | "hta"
            | "lnk"
            | "url"
            | "desktop"
            | "sh"
            | "jar"
    )
}

pub(crate) fn metadata(report: &mut Report, info: &EntryInfo) {
    let normalized = info.name.replace('\\', "/");
    let components: Vec<_> = normalized
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    let basename = components.last().copied().unwrap_or("");
    let lower = basename.to_ascii_lowercase();
    let extensions: Vec<_> = lower.split('.').collect();
    let extension = extensions.last().copied().unwrap_or("");
    if !info.directory && executable_extension(extension) {
        report.finding(
            "Suspicious executable",
            &info.name,
            "Executable, script, shortcut, or installer requires review; it was not executed",
        );
        if extensions.len() >= 3 && !extensions[extensions.len() - 2].is_empty() {
            report.finding(
                "Double extension",
                &info.name,
                "Multiple filename extensions conceal an executable file type",
            );
        }
    }
    if components.iter().enumerate().any(|(index, component)| {
        component.contains(':')
            && !(index == 0
                && component.len() == 2
                && component.as_bytes()[0].is_ascii_alphabetic()
                && component.ends_with(':'))
    }) {
        report.finding(
            "Alternate data stream",
            &info.name,
            "A colon in a path component may create an NTFS alternate data stream",
        );
    }
    if info.link {
        if let Some(target) = &info.link_target {
            let target = target.replace('\\', "/");
            let mut depth = components.len().saturating_sub(1);
            let mut unsafe_target = target.starts_with('/') || target.contains(':');
            for component in target.split('/') {
                match component {
                    ".." if depth == 0 => unsafe_target = true,
                    ".." => depth -= 1,
                    "" | "." => {}
                    _ => depth += 1,
                }
            }
            if unsafe_target {
                report.finding(
                    "Suspicious link",
                    &info.name,
                    "A symbolic or hard link target may escape the archive root",
                );
            }
        }
    }
    if components.len() > 128
        || normalized.len() > 4096
        || normalized.chars().any(|character| {
            character.is_control()
                || matches!(character, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        || components.iter().any(|component| {
            let stem = component
                .split('.')
                .next()
                .unwrap_or("")
                .to_ascii_uppercase();
            matches!(
                stem.as_str(),
                "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
            ) || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && matches!(stem.as_bytes()[3], b'1'..=b'9'))
                || component.ends_with(['.', ' '])
        })
    {
        report.finding(
            "Suspicious structure",
            &info.name,
            "Unusual path depth, length, control characters, or platform-dependent filename",
        );
    }
    if info
        .compressed_size
        .is_some_and(|packed| info.size > 10 * 1024 * 1024 && info.size / packed.max(1) > 1000)
    {
        report.finding(
            "Archive bomb",
            &info.name,
            "Extreme declared expansion ratio; large output may exhaust storage or memory",
        );
    }
    if info.error.as_deref().is_some_and(|error| {
        let error = error.to_ascii_lowercase();
        ["header", "size mismatch", "overflow", "invalid", "overlap"]
            .iter()
            .any(|needle| error.contains(needle))
    }) {
        report.finding(
            "Parser anomaly",
            &info.name,
            "Inconsistent archive structure or metadata was rejected by the parser",
        );
    }
}

pub(crate) fn structure(report: &mut Report) {
    let mut names = std::collections::BTreeMap::new();
    let mut links = std::collections::BTreeMap::new();
    let mut expanded = 0u64;
    let mut findings = Vec::new();
    for info in &report.entries {
        expanded = expanded.saturating_add(info.size);
        let name = info.name.replace('\\', "/");
        let name = name
            .trim_start_matches("./")
            .trim_end_matches('/')
            .to_lowercase();
        let canonical = name
            .split('/')
            .map(|part| part.trim_end_matches(['.', ' ']))
            .collect::<Vec<_>>()
            .join("/");
        if info.link {
            if let Some(target) = &info.link_target {
                let mut path = canonical.split('/').map(str::to_owned).collect::<Vec<_>>();
                path.pop();
                for component in target.replace('\\', "/").split('/') {
                    match component {
                        ".." => {
                            path.pop();
                        }
                        "" | "." => {}
                        component => path.push(component.to_lowercase()),
                    }
                }
                links.insert(canonical.clone(), path.join("/"));
            }
        }
        if let Some((previous, directory)) =
            names.insert(canonical.clone(), (info.name.clone(), info.directory))
        {
            if directory != info.directory || previous != info.name {
                findings.push((
                    info.name.clone(),
                    "Platform-dependent filename collision".to_owned(),
                ));
            }
        }
    }
    for info in &report.entries {
        let name = info.name.replace('\\', "/").to_lowercase();
        let components: Vec<_> = name.split('/').take(129).collect();
        for index in 1..components.len() {
            let parent = components[..index].join("/");
            if names.get(&parent).is_some_and(|(_, directory)| !directory) {
                findings.push((
                    info.name.clone(),
                    "A path uses a regular file as a directory".to_owned(),
                ));
                break;
            }
        }
    }
    for (entry, detail) in findings {
        report.finding("Suspicious structure", entry, detail);
    }
    for start in links.keys() {
        let mut current = start;
        let mut visited = std::collections::BTreeSet::new();
        while let Some(next) = links.get(current) {
            if !visited.insert(current) || visited.len() > 128 {
                report.finding(
                    "Suspicious link",
                    start,
                    "A link cycle or excessive link chain was detected",
                );
                break;
            }
            current = next;
        }
    }
    if expanded > 100 * 1024 * 1024 && expanded / report.archive_size.max(1) > 1000 {
        report.finding(
            "Archive bomb",
            "",
            "Extreme aggregate expansion ratio across archive entries",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(name: &str) -> EntryInfo {
        EntryInfo {
            name: name.into(),
            directory: false,
            size: 0,
            compressed_size: None,
            method: String::new(),
            modified: None,
            attributes: None,
            owner_id: None,
            group_id: None,
            comment: None,
            extra_fields: None,
            link_target: None,
            stored_crc: None,
            computed_crc: None,
            sha256: None,
            encrypted: false,
            link: false,
            error: None,
        }
    }

    #[test]
    fn security_metadata_rules_find_disguised_executables_ads_links_and_bombs() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("test.zip");
        let mut report = crate::inspection::inspect(
            &path,
            None,
            crate::inspection::InspectionMode::Audit,
            &std::sync::atomic::AtomicBool::new(false),
        );
        report.findings.clear();
        metadata(&mut report, &info("invoice.pdf.exe"));
        metadata(&mut report, &info("readme.txt:hidden.exe"));
        metadata(&mut report, &info("CON.txt"));
        let mut link = info("dir/link");
        link.link = true;
        link.link_target = Some("../../outside".into());
        metadata(&mut report, &link);
        let mut bomb = info("huge.txt");
        bomb.size = 100 * 1024 * 1024;
        bomb.compressed_size = Some(100);
        metadata(&mut report, &bomb);
        for kind in [
            "Suspicious executable",
            "Double extension",
            "Alternate data stream",
            "Suspicious structure",
            "Suspicious link",
            "Archive bomb",
        ] {
            assert!(
                report.findings.iter().any(|finding| finding.kind == kind),
                "{kind}"
            );
        }
        report.findings.clear();
        metadata(&mut report, &info("backup.tar.gz"));
        let mut link = info("dir/link");
        link.link = true;
        link.link_target = Some("../inside.txt".into());
        metadata(&mut report, &link);
        assert!(report.findings.is_empty());
    }

    #[test]
    fn security_signatures_find_executables_and_correlated_polyglot_markers() {
        let root = tempfile::tempdir().unwrap();
        let mut report = crate::inspection::inspect(
            &root.path().join("test.zip"),
            None,
            crate::inspection::InspectionMode::Audit,
            &std::sync::atomic::AtomicBool::new(false),
        );
        report.findings.clear();
        let mut pe = vec![0u8; 132];
        pe[..2].copy_from_slice(b"MZ");
        pe[0x3c..0x40].copy_from_slice(&128u32.to_le_bytes());
        pe[128..].copy_from_slice(b"PE\0\0");
        content(&mut report, "invoice.pdf", &pe, &pe);
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.kind == "Suspicious executable")
        );
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.kind == "Format mismatch")
        );
        let mut image = b"GIF89a%PDF-1.7 PK\x03\x04".to_vec();
        image.extend_from_slice(b"PK\x01\x02 %%EOF");
        image.extend_from_slice(b"PK\x05\x06");
        image.extend_from_slice(&[0u8; 18]);
        report.findings.clear();
        content(&mut report, "image.gif", &image, &image);
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.kind == "Polyglot file")
        );
        report.findings.clear();
        content(
            &mut report,
            "image.png",
            b"\x89PNG\r\n\x1a\nPK\x03\x04",
            b"no ZIP directory",
        );
        assert!(
            !report
                .findings
                .iter()
                .any(|finding| finding.kind == "Polyglot file")
        );
    }
}

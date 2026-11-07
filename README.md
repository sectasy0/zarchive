<h1>
  <img src="assets/readme-header.svg" alt="zarchive" width="224" height="56">
</h1>

<p>
  A modern, security-focused, and fast cross-platform archive manager for
  compression, encryption, and inspection.
</p>

<p><strong>macOS · Windows · Linux</strong></p>

zarchive brings file browsing, archive creation, and detailed content inspection
into a single desktop workspace. Explore archives before extracting them, verify
the integrity of their contents, and prepare encrypted packages for storage or
sharing. Long-running operations work in the background, keeping the interface
available while your files are processed.

![zarchive showing sample archives in its file browser](assets/screenshot.png)

## Features

- **Browse before extracting.** Navigate folders and archives, use Back and
  Forward history, browse the folder tree, sort by name, type, or size, and open
  individual files in their default desktop applications. Up/Down selects rows;
  Right enters a folder and Left moves to its parent. Selection follows the
  visible search results, filters, and sorting without opening a preview.
- **Preview and edit text without extracting.** Select a text file on disk,
  an archive entry, or a standalone compressed text file, then press Space or
  choose **Inspector** from its context menu to preview it in the
  bottom panel. UTF-8 and BOM-marked UTF-16 are supported, with syntax
  highlighting, line numbers, searching, scrolling, and copying. Choose Edit to
  modify local files or supported archive entries, then Save or Cancel. CSV table
  previews can switch to the text editor for editing when the target is writable.
  Archive changes use private staging and do not create an extracted copy in the
  browsed folder. Previews are limited to 128 KiB, and truncated previews cannot
  be saved to avoid discarding unseen data. Standalone compressed streams and
  RAR entries can be previewed but not edited.
- **Inspect and edit binary files.** The hex view shows offsets, byte values, and
  printable ASCII. Edit bytes as two-digit hex values and save local files or
  supported archive entries without visibly extracting them. Binary previews use
  the same 128 KiB safety limit; standalone compressed streams and RAR entries
  are not editable.
- **Preview CSV as a table.** CSV files, including archive entries and compressed
  CSV files, use a virtualized table with a fixed header and horizontal scrolling.
  Commas, semicolons, tabs, quoted fields, and multiline values are supported.
  Table previews are limited to 5,000 rows and 64 columns.
- **Search inside archives.** The toolbar search also checks files inside archives
  in the current disk folder, without opening each archive first. Scanning runs
  in the background, and results identify both the archive and the entry path.
  Inside an open archive, search covers the current folder and its subfolders.
  Files are not extracted during search; protected contents still require a
  password to open. Archive scans respect inspection limits and return up to
  5,000 matching entries. Archives with encrypted filenames must be unlocked
  before their entries can be searched.
- **Unified search and filters.** Combine file names with `ext:zip`, `min:1MiB`,
  `max:10MB`, `after:2026-01-01`, `before:2026-12-31`, and `type:file` or
  `type:folder` in the same search field. The dropdown offers operators, matching
  values, and a filter editor. Dates are inclusive; items without a known date
  are excluded from date-filtered results. Clear filters without removing the
  file-name query, or clear the search to reset everything.
- **Create and extract.** Package multiple files and folders, choose compression
  methods and levels, include hidden files, force ZIP64, or tune 7z dictionary and
  solid-compression settings. Encrypt ZIP and 7z archives, optionally encrypt 7z
  filenames, and extract entire archives or selected entries.
- **Convert archives with explicit loss warnings.** Choose **Files > Convert
  archive...**, select a target format, analyze the source and acknowledge the
  listed losses before saving. ZIP, 7z, TAR and its supported compressed variants,
  CPIO, AR, PAX and XAR can be converted in either direction; RAR output requires
  the external `rar` command-line tool. Standalone compressed streams can also be
  read and packaged into a target archive. Conversion preserves file bytes,
  names, hidden files and empty directories, but reads and rewrites content using
  target defaults rather than copying compressed records. Original timestamps,
  permissions, ownership, ACLs, extended attributes, comments and format-specific
  fields are not transferred. Checksums are recalculated; signatures, recovery
  records and external PAR2/REV sidecars do not carry over. Source encryption and
  volume boundaries are not retained: the result is one **unencrypted** archive.
  Passwords unlock only the source. AR rejects folders and nested paths instead
  of flattening them. Links, unsafe or duplicate paths, unreadable content and
  incomplete analysis block conversion. Limits are 10,000 entries, 128 MiB per
  file and 512 MiB of file contents in total. Work uses private temporary staging
  on the destination filesystem, so free disk space is needed for extracted
  contents and the new archive. Source integrity is checked before publication;
  existing destinations are never overwritten and the source is unchanged.
- **Manage background tasks.** A shared queue runs up to three tasks concurrently.
  Open the task indicator beside the status-bar error counters to view progress,
  pause, resume, cancel, or clear finished tasks. Extraction, archive creation and
  changes, moves, Trash, folder sizes, inspections, searches, and file previews use
  the queue. Tasks sharing a file or folder wait for each other. Pause and cancel
  are cooperative: an active codec or native system step may finish before the
  next checkpoint. Paused running tasks retain their worker reservation;
  cancellation does not undo files already moved or actions already committed.
- **Protect sensitive files.** Open password-protected ZIP and 7z archives and
  create AES-256 encrypted archives, including optional filename encryption for
  7z. Protected archives are marked in the file list. Passwords are never saved
  to preferences or logs.
- **Inspect archive contents.** Review entry sizes, compression, checksums,
  permissions, ownership, comments, extra fields, link targets, and filesystem
  metadata where available. Overview includes archive integrity and entry
  summaries; the Entries tab has a context menu for extraction into the current
  folder, copying entry paths, and removing entries with confirmation.
  Drop files onto Entries to add them to the archive, or onto an entry folder to
  add them there. The inspector refreshes after archive changes, including
  deletion of the last entry. Empty XAR archives remain readable by system tools.
- **Verify integrity.** Read archive data in the background, validate available
  checksums, and calculate CRC32 and SHA-256 during a full integrity check.
- **Repair and recover.** Use **Inspect > Archive Repair & Recovery** to recover
  readable entries into private staging, review partial and unverified files,
  and save a new ZIP plus its JSON recovery report without modifying the source.
  **Salvage Mode** scans preserved headers and possible PNG/JPEG/PDF signatures
  even when the archive directory cannot be read.
- **Audit individual archives or batches.** Identify suspicious paths, duplicate
  content, archive bombs, possible polyglot files, executable content, NTFS
  alternate data streams, unsafe links, and mismatches between signatures and
  extensions. Analyze nested archives to a configurable depth and copy reports
  with detected content types and active limits as JSON. Choose
  **Full audit** to bypass the standard limits of 32 archives per batch, 20,000
  entries, 256 MiB per entry, 1 GiB total data, and 500 findings. The 7z
  dictionary safety limit of 128 MiB, expansion protection, signature-record
  limit, and nested-audit budgets remain in effect.
- **Compare two archives.** Select archive A and archive B in separate fields to
  compare files, directories, moves and renames, empty folders, content, and
  available metadata. Results distinguish byte equality from semantic equality
  for JSON, XML, YAML, and TOML, and include SHA-256, SHA-512, BLAKE3, CRC32,
  compression, permissions, owner/group IDs, comments, and other metadata when
  available in the archive format. Supported nested archives are compared up to
  four levels deep and 128 nested archives. Large structured files over 8 MiB
  are not semantically parsed, and text diffs are limited to files up to 2 MiB;
  byte comparisons and hashes still process the full file. Copy or save the
  visual comparison report as JSON.
- **Drag and drop.** Open archives, navigate to folders, or add files and folders
  to a new archive directly from your desktop.
  Drag files or folders from the file list or desktop onto an existing archive,
  or into an open archive's current folder, to add them in the background. Valid
  targets use the accent color; invalid or unavailable targets turn red.
  Drop a file, archive, or folder onto a filesystem folder to move it instead.
  Folder targets are available in the file list, the sidebar tree, and the
  Home/Documents shortcuts. Moves run in the background without overwriting
  existing items; drops into the same folder or a folder's descendants are rejected.
- **Manage files safely.** Use context menus to copy paths, extract individual
  entries, or remove files from supported archives with confirmation. Move
  filesystem items between folders or send them to Trash; existing destinations
  are never overwritten.
- **Make it your workspace.** Choose a light, dark, or system theme, an accent
  color, one of six interface languages, and a default startup folder. Folder
  sizes are calculated automatically by default, with an option to turn that
  background work off. **Settings > Keyboard** lets you record, disable, and
  reset shortcuts for file opening, archive creation, extraction, integrity
  checks, auditing, row navigation, folder navigation, and the inspector.
  Conflicts are rejected and preferences persist between launches. Browser-only
  shortcuts do not capture typing in search fields or modal controls.

## Architecture

The crate is organized by application responsibility while retaining stable
facade modules for existing call sites:

- `main.rs` owns application state and startup; `app/view.rs` renders the main
  window. Feature workflows remain in focused modules such as `creation.rs`,
  `operations.rs`, `password.rs`, and `recovery.rs`.
- `archive.rs` is the archive service facade. `archive/format.rs` owns format
  metadata and capabilities; `archive/read.rs`, `archive/write.rs`, and
  `archive/extract.rs` own opening/listing, creation/mutation, and safe
  extraction respectively.
- `inspection.rs` runs metadata, integrity, and audit work, with shared result
  types in `inspection/model.rs`. `inspector.rs` coordinates the inspection UI;
  `inspector/comparison.rs` and `inspector/panes.rs` own comparison and
  virtualized report panels.
- `text_preview.rs` owns preview state and rendering, while
  `text_preview/loader.rs` handles bounded reads, decoding, and tabular data.
- `recovery.rs` owns the shared recovery report and export model;
  `recovery/engine.rs` routes work to format backends. `parity.rs` and its
  submodules provide parity reconstruction and creation.
- `task_queue.rs` is the shared boundary for cancellable background work and
  progress reporting. UI modules enqueue work; archive and inspection services
  cooperate through its checkpoints and readers.

When extending a subsystem, put its format-specific or view-specific logic in
that subsystem's folder and keep the facade focused on shared types and routing.

## Supported Formats

Formats are grouped into three priority levels: archives, compression formats,
and disk-image containers.

### Priority 1: Archives

| Format | Browse and extract | Create | Encryption when creating |
| :--- | :---: | :---: | :--- |
| ZIP | Yes | Yes | AES-256 |
| TAR | Yes | Yes | Not available |
| 7z | Yes | Yes | AES-256, optional filename encryption |
| RAR | Yes | Yes, with the external `rar` command-line tool | Not available |
| CPIO | Yes | Yes | Not available |
| AR | Yes | Yes (individual files only) | Not available |
| PAX | Yes | Yes | Not available |
| XAR | Yes | Yes | Not available |

**TAR compression methods:** no compression (`.tar`), Gzip (`.tar.gz`),
Bzip2 (`.tar.bz2`), XZ (`.tar.xz`), Zstandard (`.tar.zst`), LZ4 (`.tar.lz4`),
Lzip (`.tar.lz`), and Brotli (`.tar.br`). All of these variants support browsing,
extraction, and creation. Common shorthand extensions are also supported,
including `.tgz`, `.tbz`, `.tbz2`, and `.txz`.

CPIO (`.cpio`), PAX (`.pax`), and XAR (`.xar`) preserve folder structures and
empty directories. PAX is the POSIX extended TAR format. AR (`.ar`, `.a`) packages
individual files without a folder hierarchy. CPIO, AR, and PAX are created
without compression; XAR uses its built-in default compression. These formats
do not offer password encryption.

### Multipart Archives

Open any numbered part such as `backup.7z.001`, `backup.zip.002`, or
`backup.tar.gz.001`. All parts must be in the same folder and numbered
consecutively from `001`. The application reads them as one seekable stream
without assembling the entire archive in memory or writing a combined file.
Listing, file previews, extraction, integrity checks, and audits use all parts;
ZIP and 7z passwords work as with single-file archives. Numbered CPIO, AR, PAX,
XAR, and standalone compressed streams are also supported.

RAR volumes use the native UnRAR reader, including `backup.part01.rar` and the
older `backup.rar`/`backup.r00` naming scheme. Opening a later recognized volume
starts from the first part. Directory searches inspect only the first volume
to avoid duplicate results.

To create a multipart ZIP, 7z, or TAR archive (including compressed TAR), enable
**Split into volumes** directly in the creation dialog.
Choose 100 MB, 250 MB, 500 MB, 1 GB, 2 GB, or 4 GB, or enter a custom size in MB
or GB. Units are decimal: 1 MB is 1,000,000 bytes and 1 GB is 1,000,000,000 bytes.
Decimal custom values are accepted when they represent a whole number of bytes.
Saving `backup.zip` produces `backup.zip.001`, `backup.zip.002`, and so on; the
last part may be smaller. The application opens the first part after creation.

Creation writes a temporary archive, then stages the volumes and publishes them
without overwriting existing files. Failure or cancellation during splitting or
publication removes parts published by that attempt; completed sets are kept.
Keep the companion `backup.zip.volumes.json`
with the parts: it records their names, count, lengths, and SHA-256 checksums,
not passwords or file contents. This manifest is not a signature or proof of
authenticity. Opening checks completeness and lengths,
including a missing final part. Integrity checks and audits verify every part's
checksum, subject to the normal inspection limits; an incomplete verification
is not reported as passed. Without a manifest, external numbered archives remain
readable and are verified through the archive format's existing integrity checks.

Enable **Create PAR2 recovery files** in the creation dialog to protect the
completed archive. Choose 5%, 10%, 20%, 30%, or 50% recovery data. PAR2 creation
uses the native `par2-rs` engine and needs no separate executable. A single
archive produces a sidecar such as `backup.zip.par2`; split archives protect
each numbered part in the set. The volume manifest is not included because the
archive parts are the recovery inputs. The encoder has a 512 MiB working-memory
budget. PAR2 outputs are staged and published
without overwriting existing files. If PAR2 creation is cancelled or fails,
the already-created archive remains available and the status message reports
that the recovery files were not completed.

Volume support is a separate format capability: ZIP, 7z, and TAR creation use
numbered byte streams, not a native multi-disk ZIP implementation. Native RAR
volumes are readable, but volume creation through the external RAR tool is not
enabled. Other formats retain numbered-volume reading without offering volume
creation. Native split ZIP sets using `.z01`, `.z02`, and a final `.zip` are
supported by **Archive Repair & Recovery**, not normal browsing or mutation.
Numbered streams are limited to 10,000 parts; native split ZIP recovery accepts
up to 128 disks within its staging limits.

Existing multipart archives remain read-only: adding, replacing, and removing
entries are disabled.

### Updating Existing Archives

Existing ZIP, TAR (including all supported compressed TAR variants), 7z, CPIO,
AR, PAX, and XAR archives can receive additional files. ZIP is updated without
recompressing existing entries; the other formats are rebuilt in a staging
directory and replaced atomically. Name collisions are rejected rather than
overwritten. ZIP and 7z additions preserve encryption and request a password
when needed. AR supports individual files only, not folders.

Editing a supported text or binary archive entry replaces that entry through
private staging and publishes the updated archive atomically. No extracted copy
is left in the browsed folder. Password-protected ZIP and 7z entries require the
correct password. RAR entries and standalone compressed streams cannot be edited.

Removing entries from ordinary or AES-encrypted ZIP archives copies surviving
compressed/encrypted payloads without decrypting or recompressing them. AES
ciphertext, timestamps, permissions, and comments are preserved. Legacy ZipCrypto
archives still require a password and are rebuilt with AES encryption because
the ZIP library's raw-copy API does not preserve their encryption flags safely.

Creating or updating RAR requires the external `rar` command-line tool on
`PATH`; this is separate from the bundled RAR reader. RAR encryption and
updates to encrypted RAR archives are not supported.
Standalone compression streams such as `.gz` and `.zst` contain one file and
cannot accept additional files; use a TAR archive with that compression instead.

### Priority 2: Compression Formats

Compression formats encode a data stream rather than package multiple files.
Standalone compression handles one file; use an archive format to preserve a
folder structure or combine multiple files.

| Method | With TAR: browse, extract, and create | Standalone: decompress and create |
| :--- | :---: | :---: |
| Gzip | Yes | Yes (`.gz`) |
| Bzip2 | Yes | Yes (`.bz2`) |
| XZ | Yes | Yes (`.xz`) |
| Zstandard | Yes | Yes (`.zst`) |
| LZ4 | Yes | Yes (`.lz4`) |
| Lzip | Yes | Yes (`.lz`) |
| Brotli | Yes | Yes (`.br`) |

### Priority 3: Containers

Disk-image and virtual-disk containers are reserved for future support:

| Container | Extension | Current status |
| :--- | :--- | :--- |
| ISO | `.iso` | Not yet supported |
| DMG | `.dmg` | Not yet supported |
| QCOW2 | `.qcow2` | Not yet supported |
| VHDX | `.vhdx` | Not yet supported |

These containers cannot currently be browsed, extracted, created, modified,
or integrity-tested.

## Data Protection

Full-archive extraction creates a new destination folder without overwriting
existing content. Unsafe extraction paths are rejected, as are links in ZIP,
TAR, CPIO, AR, PAX, and XAR archives.
New archives are written to a temporary file before being published, protecting
an existing destination file if creation fails.

Preview, integrity checking, and auditing serve different purposes: displaying
an archive's file list does not verify its data. Auditing highlights items that
deserve attention; it is not an antivirus scanner. Checks operate within resource
limits, and incomplete inspections are not reported as fully successful.

### Security Audit

The audit combines metadata rules, file signatures from `infer`, and bounded
content analysis. It reports:

- **Archive bombs:** extreme declared, observed, or aggregate expansion ratios.
  Decompression stops when a dangerous ratio is observed, including in ZIP and
  standalone compressed streams. Reaching a safety limit makes the audit
  incomplete rather than marking unread data as verified.
- **Possible polyglots:** correlated signatures of multiple formats, such as
  an image or executable with a ZIP container, or an image with PDF markers.
  Self-extracting archives and legitimate embedded containers can also trigger
  these findings.
- **Executable content:** programs, scripts, installers, shortcuts, executable
  signatures, and deceptive double extensions such as `invoice.pdf.exe`.
- **NTFS alternate data streams:** colon-delimited stream names in path
  components, including names nested inside subdirectories.
- **Suspicious links:** symlink or hardlink targets outside the archive root,
  link cycles, and excessive link chains. Links are examined as metadata or
  bounded target text; they are never followed.
- **Unusual structures and parser anomalies:** overlapping ZIP entry ranges,
  inconsistent sizes or headers, file/directory conflicts, platform-dependent
  name collisions, reserved Windows names, excessive path depth, and control
  or bidirectional formatting characters. These are potential parser-risk
  indicators, not proof of a working exploit.

Set **Settings > Advanced > Nested archive audit depth** from 0 to 8. The default
is 2; 0 reports nested archives without recursively inspecting them. Nested
findings use paths such as `outer.zip!inner.zip!invoice.pdf.exe`. Nested files are
streamed into private temporary files, never extracted using their stored paths,
and are removed after inspection. Encrypted nested archives are reported as
uninspected; their passwords are not guessed or brute-forced.

Each top-level audit has hard nested limits of 64 archives, 32 MiB per staged
archive or nested entry, and 256 MiB of shared nested input/content reads.
Nested scans retain the standard bounded audit mode even when the parent uses
Full audit. Depth, budget, cancellation, encrypted content, or nested parser
failures prevent a fully successful audit. Reports retain at most 20,000 content
signatures. Checks use bounded prefixes and suffixes, so not every polyglot,
malware family, or parser exploit can be detected. Archive parsers run in the
application process; the audit is not a sandbox or an antivirus scanner.

### Try the Security Audit

Generate the small, harmless manual examples with:

```sh
cargo test --locked generate_security_audit_samples -- --ignored --nocapture
```

Set **Settings > Advanced > Nested archive audit depth** to **2**, open a sample,
and choose **Audit archive**. Review its findings rather than extracting it.

| Sample | Expected result |
| :--- | :--- |
| `samples/35-security-audit.zip` | Executable names, double extensions, ADS, signature mismatch, a GIF/ZIP polyglot, unsafe links, path conflicts, and findings two ZIP levels deep. Integrity passes because its bytes are intact; this does not mean the contents are safe. |
| `samples/36-bounded-expansion.zip` | Archive bomb warning and an incomplete audit. A tiny ZIP expands to a fixed 12 MiB of zeros; it is not an unbounded bomb. |
| `samples/37-parser-anomaly.zip` | Parser anomaly warning and an incomplete audit caused by deliberately aliased ZIP entry ranges. |

The executable-looking entries contain plain text, not runnable programs.
Links and unusual filenames exist only as archive entries; the generator does
not create external files or actual filesystem links. Existing samples are
verified, not overwritten. The sample directory is ignored by Git; the generator
and its regression test are part of the repository.

### Archive Repair & Recovery

Open **Inspect > Archive Repair & Recovery** for the selected file or open
archive. Use **Choose archive...** for an unreadable file that cannot be browsed
normally. An optional password is kept only in the secret input and the running
job. Enable **Salvage Mode**, then choose **Recover files**. Recovery and export
run in the background queue; closing the dialog cancels pending work and releases
its private temporary files.

- **ZIP:** Stored and Deflated entries are recovered independently, using
  preserved central metadata or local headers. A missing central directory is
  reconstructed when the recovered entries are saved to a new ZIP. CRC and size
  matches are required for a verified recovery. Damaged stored bytes and readable
  prefixes of damaged entries are retained separately as partial data. Encrypted
  entries require the password and are conservatively labeled unverified.
- **TAR:** Recovery scans aligned 512-byte blocks in plain TAR/PAX archives,
  recognizes V7, USTAR, and GNU headers, reconstructs damaged header checksums,
  and resynchronizes after destroyed headers or missing blocks. Preserved GNU
  long-name records are supported. Truncated file payloads are kept as partial
  data. Choose **Export** to build a new TAR in the source format with correct
  headers, padding, end markers, and an embedded recovery report. Original names
  remain in the report while exported files use safe generated names. Missing
  header names or invalid size fields cannot be
  reconstructed reliably, and damaged PAX/GNU sparse metadata can prevent full
  recovery. TAR has no per-file CRC, so header repair does not verify its content.
- **CPIO:** ODC, `newc`, and CRC ASCII record headers are resynchronized after a
  destroyed record. Names and lengths are bounded before the native parser is
  called. The CRC variant's additive content checksum is verified when present;
  other variants remain unverified. Missing names or lengths cannot be invented.
- **AR:** Preserved member headers can be read after a destroyed global or member
  header. GNU filename tables and BSD long-name members are supported. If the GNU
  filename table is missing, a generated name is recorded instead of claiming the
  original name was recovered. AR has no per-member content checksum.
- **XAR:** Entries are recovered independently, retaining later files after an
  earlier entry fails. A valid bounded zlib/XML contents table can reconstruct
  incorrect header lengths, magic, or version fields. Truncated entry data and
  incorrect declared sizes retain available bytes as partial output. Known
  extracted checksums are verified; absent checksums remain unverified. A lost
  or unreadable contents table cannot reliably recover filenames and heap ranges.
- **7z:** Recovery decodes independent compression blocks separately, so a failed
  block does not discard verified files from other blocks. Per-file CRCs are
  checked when available. Files dependent on a broken solid stream are reported
  separately; changing a checksum does not reconstruct missing compressed bytes.
  A damaged start-header CRC can be reconstructed in memory only when the
  referenced next header is intact and its CRC verifies. Destroyed end metadata
  or encrypted headers without the correct password can still prevent recovery.
  Header reads and dictionary sizes retain their safety limits.
- **RAR:** A native UnRAR data callback retains readable bytes without creating
  archive-supplied paths or links, validates through the native decoder and CRC
  where available, and attempts later entries after a damaged file. Original RAR
  volume naming is handled by the reader; missing volumes and password errors
  are reported explicitly. Solid dependencies may prevent later files from
  decoding. Optional parity reconstruction uses RAR recovery records or matching
  `.rev` recovery volumes on a private copy before normal file recovery. The
  original RAR and its volumes are never rewritten.
- **Compressed TAR and standalone streams:** GZIP, BZIP2, XZ, Zstandard, LZ4,
  LZIP, and Brotli are decoded into bounded private staging. Available decoded
  TAR records then use the TAR repair path, even if a damaged footer prevents
  complete wrapper validation. Standalone streams retain their readable payload.
  Diagnostics distinguish compressed input from offsets in the decoded stream.
  Damage to a compression dictionary or an early block can still make later
  compressed bytes undecodable.
- **Other variants:** The native reader remains a fallback for variants without
  a dedicated resynchronizer. Unsupported methods, encrypted metadata, and
  damaged streams can prevent further recovery.
- **Missing numbered volumes:** Existing `.001` parts are used without collapsing
  gaps. A companion volume manifest supplies expected positions and sizes; without
  it, positions are inferred from available part sizes and may be uncertain.
  Entries wholly in readable ranges may survive while entries crossing a missing
  range are partial or unavailable. Matching PAR2 recovery data can reconstruct
  protected volume files before archive reading. Without sufficient valid parity,
  missing bytes are never zero-filled or invented.
- **Signature carving:** Salvage Mode recognizes possible PNG, JPEG, and PDF data
  inside unreadable bytes. File endings are heuristic: results are labeled
  **Signature candidate (unverified)**, not guaranteed complete files.

The result list distinguishes **Recovered**, **Recovered (unverified)**,
**Partially recovered**, **Not recovered**, and **Skipped**, with suspect source
ranges and diagnostic notes. Each failed or uncertain entry includes its reason
and a suggested next step. Where known, the UI and JSON report show the expected
and recovered size, expected and actual CRC32, and compressed source range.
Unknown offsets are not presented as measured locations. Errors distinguish
missing headers, truncated data, failed checksums, missing volumes, passwords,
unsupported compression, damaged solid dependencies, and safety limits. For
encrypted data, an unsuccessful decode can mean either a wrong password or
corruption; the report does not pretend to distinguish them without evidence.
**Not recovered** describes this attempt; it does
not prove that specialist tools could never recover the file. Unreadable regions
can contain entries whose names and existence cannot be reconstructed.

The single **Export** action writes recovered entries using the original
archive format's writer, with generated safe names under `recovered/`,
`unverified/`, `partial/`, and `carved/`, and embeds `recovery-report.json`.
This keeps the export in the source format rather than asking you to choose an
unrelated ZIP or TAR. The report retains original names and output mappings.
Exports never overwrite existing files. Recovered archives are **not encrypted**,
even when the source was encrypted; recovered data is never executed or opened
automatically. Directories and filesystem links are not recreated. RAR export
requires the existing external RAR writer. AR export flattens generated paths,
with the exact mapping in the embedded report. New headers and checksums describe
the recovered bytes; they do not prove partial data matches the original.

Recovery is bounded to 10,000 entries, 128 MiB per output file, 1 GiB of total
staged output, 4 GiB of aggregate input reads, 512 MiB of raw signature scanning,
20,000 signature candidates, and 16 MiB per carved candidate. Oversized or ZIP64
directories fall back to local-header scanning instead of unbounded metadata
allocation. Cancellation and limits can leave additional recoverable data
unchecked. Recovery is best effort, not a parser sandbox or a guarantee of repair.
Compressed staging shares the input and scan budgets and stops at 512 MiB or an
extreme expansion ratio. XAR contents-table reconstruction retains a 16 MiB cap.
Native multi-disk ZIP and PAR2 reconstruction are described below. No format can
be completely reconstructed without enough surviving information or valid parity data.

#### Recovery Engine

Recovery separates the UI, shared result/export model, bounded seekable sources,
and orchestration from ZIP, RAR, 7z, TAR, native-container, structured-format, and
signature-salvage backends. Reconstruction prepares private copies before normal
entry verification; it does not bypass shared output limits or promote an entry
solely because an external program returned success.

#### PAR2 Reconstruction

Select a `.par2` file in **Archive Repair & Recovery**, or select a protected file
with an adjacent `filename.par2` or `basename.par2` sidecar. Reconstruction is
selected by default for a PAR2 source. Keep all matching `.volNN+NN.par2` volumes
and surviving protected files in the same folder.

PAR2 uses the native `rust-par2` Reed-Solomon engine; no external PAR2 executable
is required. Metadata is combined across volumes, including recovery-only
volumes, while conflicting sets or file descriptions are rejected. Only blocks
matching their slice checksums are fed to the solver. Each protected file must
match its full-file MD5 before it is exported as **Recovered**. This detects
accidental corruption, not authenticity. A reconstructed archive can then be
read by the regular recovery backends. Verified protected files are also kept
as exportable entries, rather than discarded when they are not archives.

The initial implementation supports flat filenames only: directory paths,
traversal, drive/stream names, reserved names, symlinks, and name collisions are
rejected before protected-file access. Limits include 128 protected files,
128 parity volumes, 1 GiB combined input, 16 MiB unique metadata, 8 MiB slices,
8,192 source blocks, at most 256 missing blocks, and a 128 MiB repair-buffer
budget. Operations check cooperative cancellation and a 120-second deadline
between stages and blocks; an individual solver call is not preempted.
Existing recovery output limits still apply. JSON records per-file MD5
verification and PAR2-specific limits. Failed or incomplete reconstruction never
exports zero-filled staging gaps as recovered data.

#### Native Split ZIP

Select the final `.zip` or an available `.z01`, `.z02`, etc. Recovery stages
matching regular disks and can use Info-ZIP (`zip` on `PATH`, or a trusted
executable configured through `ZARCHIVE_ZIP_TOOL`) to convert a complete set.
Every entry in the converted copy must pass bounded size and CRC verification
before it is accepted. Some system Info-ZIP builds return success yet corrupt
cross-disk payloads; those copies are discarded automatically.

Without a valid converted copy, the built-in ZIP backend scans local entries
directly across the original disk layout and rebuilds the exported ZIP using
the existing writer. Missing disks remain positional gaps; inferred sizes and
uncertain offsets are reported. Intact entries after a gap can still be
recovered. Missing bytes are not parity-reconstructed without a matching PAR2
set. Damaged or ZIP64 disk-count metadata can prevent layout recovery, and
encrypted entries may require a usable reconstructed central directory.

#### Parity Reconstruction

For a RAR or REV source, enable **Reconstruct using parity data** before choosing
**Recover files**. Put all available original RAR volumes and matching REV files
in one folder. You can select an existing later volume or a REV file even when
the first RAR volume is missing.

This feature requires the official **RAR command-line tool**, not only UnRAR.
Install `rar` (`rar.exe` on Windows) on `PATH`, or set `ZARCHIVE_RAR_TOOL` to the
full path of a trusted RAR executable before launching zarchive. RAR is a
separately licensed optional dependency; zarchive does not download or bundle it.
For example:

```sh
ZARCHIVE_RAR_TOOL=/absolute/path/to/rar cargo run
```

The application stages only matching regular RAR/REV files in a private folder,
rejects symlinks, then runs `rar r` for an embedded recovery record or `rar rc`
for REV volumes. No shell is used, passwords are not placed on the command line,
stdin is disabled, and user RAR configuration is ignored. The tool receives only
private filenames, not paths to the original archive. Native recovery then
checks files from the resulting copy before they are exported.

Reports distinguish parity reconstruction, structure-only rebuilding, unchanged
data, a missing tool, insufficient recovery data, and safety limits. A `fixed.*`
candidate does not by itself prove every file was repaired: normal CRC and size
verification still decides each entry's status. Diagnostic offsets from this
stage refer to the private repaired copy, as stated in the result notes.

The parity stage accepts up to 128 input files totaling 1 GiB, monitors a 3 GiB
workspace budget, and stops its child process after 120 seconds or cancellation.
Originals stay untouched; staged copies and repair output are deleted when no
longer needed. Limits and tool failures fall back to ordinary best-effort
recovery. External repair is not sandboxed and still depends on having enough
valid parity data. Encrypted headers that require interactive credentials may
not be repaired by this noninteractive stage; the normal recovery password field
is used afterward when reading a repaired candidate.

This external engine implements RAR recovery records and REV volumes; PAR2 uses
the separate native backend described above. 7z has no equivalent native
recovery record. The parity checkbox is not offered for 7z, ZIP, or TAR without
a matching PAR2 sidecar.

The real-engine integration test creates an archive with a recovery record,
corrupts its stored payload, reconstructs it, and compares recovered bytes. It
also deletes a first RAR volume and verifies reconstruction from REV, including
when selecting a REV file. Run it explicitly after installing RAR:

```sh
cargo test --locked parity_real_rar_reconstructs_recovery_record_and_missing_rev_volume -- --ignored --nocapture
```

Ordinary CI tests cover staging, optional-tool handling, and UI integration
without requiring RAR; the real-engine test is intentionally ignored by default.

For a manual TAR repair example, generate a small V7 archive with a bad checksum,
a destroyed middle header, and a truncated last file, plus its repaired copy:

```sh
cargo test --locked generate_tar_repair_sample -- --ignored --nocapture
```

Open `samples/38-tar-repair.tar` through **Archive Repair & Recovery**, run
**Recover files**, and choose **Export**. The first file should be
unverified, the last partial, and the missing middle header listed as a damaged
range. `samples/38-tar-repaired.tar` is the generator's verified readable output.
Existing sample files are not overwritten.

## Cross-Platform

zarchive is a cross-platform project targeting **macOS, Windows, and Linux**.
It combines a consistent workspace with system file dialogs and document opening
through your default applications. Prebuilt package availability depends on the
release.

## Continuous Integration

GitHub Actions runs the **CI** workflow on pushes, pull requests, and manual
dispatches. It uses the Rust version pinned in `rust-toolchain.toml` and
`Cargo.lock` (`--locked`) for reproducible dependency resolution.

The test job runs `cargo test --locked` on macOS ARM64. The current GUI tests
include macOS-specific keyboard shortcuts, so the complete suite is not yet
run on Linux or Windows. Tests marked `#[ignore]` are manual sample generators
or a performance benchmark and are not run automatically.

After the tests pass, native release builds run for:

| Platform | Architecture | Artifact |
| :--- | :--- | :--- |
| macOS | Apple Silicon (ARM64) | `zarchive-macos-arm64` |
| Linux (Ubuntu 24.04) | x64 | `zarchive-linux-x64` |
| Windows | x64 | `zarchive-windows-x64` |

Download the archives from **Actions > CI > workflow run > Artifacts**. Artifacts
are retained for 14 days. The Windows ZIP includes the native dependency DLLs;
keep them beside `zarchive.exe` after extraction. Unix archives contain the
executable and still require the corresponding system libraries. On macOS,
install the compression libraries with `brew install libb2 xz zstd lz4`.
Build-time native dependencies for each runner are defined in
`.github/actions/setup-native/action.yml`.

These are unsigned CI builds, not standalone installers or notarized macOS
applications. This workflow does not publish GitHub Releases and needs no
signing certificates or repository secrets.

## License

zarchive is released under the **WTFPL** license.
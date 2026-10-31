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

- **Browse before extracting.** Navigate folders and archives, sort by name,
  type, or size, and open individual files in their default desktop applications.
- **Preview text without extracting.** Single-click a text file on disk, an
  archive entry, or a standalone compressed text file to preview it in the bottom
  panel. UTF-8 and BOM-marked UTF-16 are supported in a read-only GPUI Kit Editor,
  with automatic syntax highlighting, line numbers, text selection, searching,
  scrolling, and copying. Languages are detected from the original filename,
  including archived and compressed files. The editor
  surface follows the application's light or dark palette. Previews read up to
  128 KiB and limit archive scanning; large
  content is marked as truncated, and binary files are not displayed as text.
  Drag the top edge of the preview panel to adjust its height; the chosen size
  is retained while switching files during the session.
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
- **Create and extract.** Package multiple files and folders, choose a compression
  method and level, and extract entire archives or selected entries.
- **Manage background tasks.** A shared queue runs up to three tasks concurrently.
  Open the task indicator beside the status-bar error counters to view progress,
  pause, resume, cancel, or clear finished tasks. Extraction, archive creation and
  changes, moves, Trash, folder sizes, inspections, searches, and file previews use
  the queue. Tasks sharing a file or folder wait for each other. Pause and cancel
  are cooperative: an active codec or native system step may finish before the
  next checkpoint. Paused running tasks retain their worker reservation;
  cancellation does not undo files already moved or actions already committed.
- **Protect sensitive files.** Create password-protected ZIP and 7z archives with
  AES-256 encryption, including optional filename encryption for 7z. Passwords
  are never saved to preferences or logs.
- **Inspect archive contents.** Review entries, sizes, compression methods, and
  available checksums without opening the files themselves.
  The Entries tab has a context menu for extraction into the current folder,
  copying entry paths, and removing entries with confirmation. Drop files onto
  Entries to add them to the archive, or onto an entry folder to add them there.
  The inspector refreshes after archive changes, including deletion of the last
  entry. Empty XAR archives remain readable by the system's archive tools.
- **Verify integrity.** Read archive data in the background, validate available
  checksums, and calculate CRC32 and SHA-256 during a full integrity check.
- **Audit individual archives or batches.** Identify suspicious paths, duplicate
  content, nested archives, unusual compression ratios, and mismatches between
  file signatures and extensions. Copy inspection reports as JSON.
- **Drag and drop.** Open archives, navigate to folders, or add files and folders
  to a new archive directly from your desktop.
  Drag files or folders from the file list or desktop onto an existing archive,
  or into an open archive's current folder, to add them in the background. Valid
  targets use the accent color; invalid or unavailable targets turn red.
  Drop a file, archive, or folder onto a filesystem folder to move it instead.
  Folder targets are available in the file list, the sidebar tree, and the
  Home/Documents shortcuts. Moves run in the background without overwriting
  existing items; drops into the same folder or a folder's descendants are rejected.
- **Make it your workspace.** Choose a light, dark, or system theme, an accent
  color, an interface language, and a default startup folder.

## Supported Formats

Formats are grouped into three priority levels: archives, compression formats,
and disk-image containers.

### Priority 1: Archives

| Format | Browse and extract | Create | Encryption when creating |
| :--- | :---: | :---: | :--- |
| ZIP | Yes | Yes | AES-256 |
| TAR | Yes | Yes | Not available |
| 7z | Yes | Yes | AES-256, optional filename encryption |
| RAR | Yes | No | Not available |
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

### Updating Existing Archives

Existing ZIP, TAR (including all supported compressed TAR variants), 7z, CPIO,
AR, PAX, and XAR archives can receive additional files. ZIP is updated without
recompressing existing entries; the other formats are rebuilt in a staging
directory and replaced atomically. Name collisions are rejected rather than
overwritten. ZIP and 7z additions preserve encryption and request a password
when needed. AR supports individual files only, not folders.

Removing entries from ordinary or AES-encrypted ZIP archives copies surviving
compressed/encrypted payloads without decrypting or recompressing them. AES
ciphertext, timestamps, permissions, and comments are preserved. Legacy ZipCrypto
archives still require a password and are rebuilt with AES encryption because
the ZIP library's raw-copy API does not preserve their encryption flags safely.

Updating RAR requires the external `rar` command-line tool on `PATH`; this is
separate from the bundled RAR reader. Encrypted RAR updates are not supported.
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

## Cross-Platform

zarchive is a cross-platform project targeting **macOS, Windows, and Linux**.
It combines a consistent workspace with system file dialogs and document opening
through your default applications. Prebuilt package availability depends on the
release.

## License

zarchive is released under the **WTFPL** license.
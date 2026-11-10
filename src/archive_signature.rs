//! Signs and verifies archive entry manifests using embedded Minisign signatures.

use crate::{
    ArchiveManager, Modal, Theme,
    archive,
    file_actions::EntryTarget,
    inspection::{self, InspectionMode, Integrity, Report},
    secret::{self, SecretField, SecretInput},
};
use anyhow::{Context, Result, ensure};
use gpui::{App, Context as UiContext, IntoElement, Window, div, prelude::*, px, rgb};
use gpui::component::{Disableable, Sizable, button::{Button, ButtonVariants}};
use minisign::{PublicKey, PublicKeyBox, SecretKey, SecretKeyBox, SignatureBox};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Cursor,
    path::Path,
    sync::{Arc, atomic::AtomicBool},
};
use zeroize::Zeroizing;

pub(crate) const SIGNATURE_ENTRY: &str = ".zarchive/signature.minisig.json";
const SIGNATURE_VERSION: u32 = 1;
const MAX_SIGNATURE_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct SignedEntry {
    path: String,
    size: u64,
    directory: bool,
    link_target: Option<String>,
    sha256: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct SignedPayload {
    version: u32,
    entries: Vec<SignedEntry>,
}

#[derive(Debug, Deserialize, Serialize)]
struct EmbeddedSignature {
    version: u32,
    public_key: String,
    signature: String,
    payload: SignedPayload,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Verification {
    pub key_id: String,
    pub entries: usize,
}

pub(crate) struct State {
    pub target: Option<EntryTarget>,
    pub key_path: Option<std::path::PathBuf>,
    pub passphrase: SecretInput,
}

impl State {
    pub(crate) fn new(cx: &mut App) -> Self {
        Self {
            target: None,
            key_path: None,
            passphrase: SecretInput::new(cx),
        }
    }

    pub(crate) fn clear(&mut self) {
        self.target = None;
        self.key_path = None;
        self.passphrase.clear();
    }
}

impl ArchiveManager {
    pub(crate) fn open_sign_archive(
        &mut self,
        target: EntryTarget,
        window: &mut Window,
        cx: &mut UiContext<Self>,
    ) {
        self.archive_signature.clear();
        self.archive_signature.target = Some(target);
        self.show_modal(Modal::SignArchive, window, cx);
    }

    fn choose_signing_key(&mut self, cx: &mut UiContext<Self>) {
        let Some(path) = rfd::FileDialog::new()
            .set_title(self.language.text("Select Minisign secret key"))
            .add_filter("Minisign secret key", &["key"])
            .pick_file()
        else {
            return;
        };
        self.archive_signature.key_path = Some(path);
        cx.notify();
    }

    pub(crate) fn submit_archive_signature(
        &mut self,
        window: &mut Window,
        cx: &mut UiContext<Self>,
    ) {
        let Some(target) = self.archive_signature.target.clone() else { return; };
        let Some(key_path) = self.archive_signature.key_path.clone() else { return; };
        let key_passphrase = Zeroizing::new(self.archive_signature.passphrase.text.to_string());
        let archive_password = target
            .password
            .as_ref()
            .map(|password| Zeroizing::new(password.as_str().to_owned()));
        self.close_modal(window, cx);
        let archive_path = target.path.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        self.background_queue.enqueue(
            "Signing archive...",
            crate::task_queue::TaskControl::with_cancel(cancel.clone())
                .for_paths([archive_path.clone()]),
            move |control, reporter| async move {
                reporter.phase("Signing archive...");
                if !control.checkpoint() {
                    anyhow::bail!("Task cancelled");
                }
                let key_id = sign_archive(
                    &archive_path,
                    &key_path,
                    &key_passphrase,
                    archive_password.as_deref().map(|password| password.as_str()),
                    &cancel,
                )?;
                if !control.checkpoint() {
                    anyhow::bail!("Task cancelled");
                }
                Ok(key_id)
            },
            move |manager, result, _, cx| {
                manager.status_message = Some(match result {
                    Ok(key_id) => format!(
                        "{} · {}: {}",
                        manager.language.text("Signature embedded in archive"),
                        manager.language.text("Key ID"),
                        key_id
                    ),
                    Err(crate::task_queue::TaskFailure::Cancelled) => {
                        manager.language.text("Signature operation cancelled").to_owned()
                    }
                    Err(crate::task_queue::TaskFailure::Failed(error)) => format!(
                        "{}: {error:#}",
                        manager.language.text("Could not sign archive")
                    ),
                });
                cx.notify();
            },
            cx,
        );
    }

    pub(crate) fn verify_archive_signature(
        &mut self,
        target: &EntryTarget,
        cx: &mut UiContext<Self>,
    ) {
        let archive_path = target.path.clone();
        let archive_password = target
            .password
            .as_ref()
            .map(|password| Zeroizing::new(password.as_str().to_owned()));
        let cancel = Arc::new(AtomicBool::new(false));
        self.background_queue.enqueue(
            "Verifying archive signature...",
            crate::task_queue::TaskControl::with_cancel(cancel.clone())
                .for_paths([archive_path.clone()]),
            move |control, reporter| async move {
                reporter.phase("Verifying archive signature...");
                if !control.checkpoint() {
                    anyhow::bail!("Task cancelled");
                }
                let verification = verify_archive_signature(
                    &archive_path,
                    archive_password.as_deref().map(|password| password.as_str()),
                    &cancel,
                )?;
                if !control.checkpoint() {
                    anyhow::bail!("Task cancelled");
                }
                Ok(verification)
            },
            move |manager, result, _, cx| {
                manager.status_message = Some(match result {
                    Ok(verification) => format!(
                        "{} · {}: {} · {}",
                        manager.language.text("Embedded signature is valid"),
                        manager.language.text("Key ID"),
                        verification.key_id,
                        manager.language.text("Embedded key is not independently trusted")
                    ),
                    Err(crate::task_queue::TaskFailure::Cancelled) => {
                        manager.language.text("Signature operation cancelled").to_owned()
                    }
                    Err(crate::task_queue::TaskFailure::Failed(error)) => format!(
                        "{}: {error:#}",
                        manager.language.text("Could not verify archive signature")
                    ),
                });
                cx.notify();
            },
            cx,
        );
    }
}

pub(crate) fn view(
    manager: &ArchiveManager,
    theme: Theme,
    cx: &mut UiContext<ArchiveManager>,
) -> impl IntoElement {
    let language = manager.language;
    let state = &manager.archive_signature;
    let Some(target) = state.target.as_ref() else {
        return div().id("archive-signature-content");
    };
    let key_path = state.key_path.as_ref();
    div()
        .id("archive-signature-content")
        .debug_selector(|| "archive-signature-content".to_owned())
        .flex()
        .flex_col()
        .gap_3()
        .w_full()
        .child(div().truncate().child(target.path.display().to_string()))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .truncate()
                        .text_color(rgb(theme.muted_text))
                        .child(key_path.map_or_else(
                            || language.text("No signing key selected").to_owned(),
                            |path| path.display().to_string(),
                        )),
                )
                .child(
                    Button::new("choose-signing-key")
                        .ghost()
                        .small()
                        .icon(gpui::assets::IconName::FolderOpen)
                        .label(language.text("Choose key..."))
                        .on_click(cx.listener(|manager, _, _, cx| {
                            manager.choose_signing_key(cx);
                        })),
                ),
        )
        .child(language.text("Secret key passphrase"))
        .child(secret::field(
            "archive-signing-key-passphrase",
            &state.passphrase,
            SecretField::ArchiveSigning,
            true,
            language,
            cx,
        ))
        .child(
            div()
                .text_xs()
                .text_color(rgb(theme.muted_text))
                .child(language.text(
                    "The signature is embedded in the archive and covers its entry paths and contents.",
                )),
        )
}

pub(crate) fn footer(
    manager: &ArchiveManager,
    cx: &mut UiContext<ArchiveManager>,
) -> impl IntoElement {
    let language = manager.language;
    Button::new("sign-archive")
        .primary()
        .small()
        .icon(gpui::assets::IconName::ShieldCheck)
        .label(language.text("Sign archive"))
        .disabled(manager.archive_signature.key_path.is_none())
        .on_click(cx.listener(|manager, _, window, cx| {
            manager.submit_archive_signature(window, cx);
        }))
}

fn checked_payload(report: &Report) -> Result<SignedPayload> {
    ensure!(
        report.integrity == Integrity::Passed && report.error.is_none(),
        "The archive must pass an integrity check before signing"
    );
    let mut entries: Vec<_> = report
        .entries
        .iter()
        .filter(|entry| entry.name != SIGNATURE_ENTRY)
        .map(|entry| {
            ensure!(
                entry.directory || entry.link || entry.sha256.is_some(),
                "Could not hash archive entry: {}",
                entry.name
            );
            Ok(SignedEntry {
                path: entry.name.clone(),
                size: entry.size,
                directory: entry.directory,
                link_target: entry.link_target.clone(),
                sha256: entry.sha256.clone(),
            })
        })
        .collect::<Result<_>>()?;
    entries.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.directory.cmp(&right.directory))
    });
    ensure!(
        entries.windows(2).all(|pair| pair[0].path != pair[1].path),
        "The archive contains duplicate paths and cannot be signed"
    );
    Ok(SignedPayload {
        version: SIGNATURE_VERSION,
        entries,
    })
}

fn inspect_archive(
    path: &Path,
    password: Option<&str>,
    cancel: &AtomicBool,
) -> Result<Report> {
    let report = inspection::inspect(path, password, InspectionMode::Integrity, cancel);
    if let Some(error) = &report.error {
        anyhow::bail!("Archive integrity check failed: {error}");
    }
    Ok(report)
}

fn load_secret_key(path: &Path, passphrase: &str) -> Result<SecretKey> {
    let bytes = fs::read(path).context("Could not read the Minisign secret key")?;
    if let Ok(secret) = SecretKey::from_bytes(&bytes)
        && !secret.is_encrypted()
    {
        return Ok(secret);
    }
    let key_text = std::str::from_utf8(&bytes).context("Invalid Minisign secret key file")?;
    let boxed = SecretKeyBox::from_string(key_text)?;
    if passphrase.is_empty() {
        boxed
            .clone()
            .into_unencrypted_secret_key()
            .or_else(|_| boxed.into_secret_key(Some(String::new())))
            .context("Enter the passphrase for this Minisign secret key")
    } else {
        boxed
            .into_secret_key(Some(passphrase.to_owned()))
            .context("Could not unlock the Minisign secret key")
    }
}

fn signature_entries(report: &Report) -> Vec<&inspection::EntryInfo> {
    report
        .entries
        .iter()
        .filter(|entry| entry.name == SIGNATURE_ENTRY)
        .collect()
}

pub(crate) fn sign_archive(
    archive_path: &Path,
    secret_key_path: &Path,
    key_passphrase: &str,
    archive_password: Option<&str>,
    cancel: &AtomicBool,
) -> Result<String> {
    ensure!(
        archive::can_add_to_archive(archive_path),
        "This archive format cannot store an embedded signature"
    );
    let report = inspect_archive(archive_path, archive_password, cancel)?;
    let signatures = signature_entries(&report);
    ensure!(signatures.len() <= 1, "The archive contains duplicate signature records");
    let payload = checked_payload(&report)?;
    let payload_bytes = serde_json::to_vec(&payload)?;
    let secret_key = load_secret_key(secret_key_path, key_passphrase)?;
    let public_key = PublicKey::from_secret_key(&secret_key)?;
    let signature = minisign::sign(
        Some(&public_key),
        &secret_key,
        Cursor::new(&payload_bytes),
        Some("zarchive embedded archive signature v1"),
        Some("zarchive archive signature"),
    )?;
    let manifest = EmbeddedSignature {
        version: SIGNATURE_VERSION,
        public_key: public_key.to_box()?.to_string(),
        signature: signature.into_string(),
        payload,
    };
    let bytes = serde_json::to_vec_pretty(&manifest)?;
    if signatures.len() == 1 {
        archive::replace_archive_entry_with_password(
            archive_path,
            SIGNATURE_ENTRY,
            &bytes,
            archive_password,
        )?;
    } else {
        let staging = tempfile::tempdir()?;
        let manifest_path = staging.path().join("signature.minisig.json");
        fs::write(&manifest_path, bytes)?;
        archive::add_to_archive_with_password(
            archive_path,
            &[manifest_path],
            ".zarchive",
            archive_password,
        )?;
    }
    let key_id = u64::from_le_bytes(public_key.keynum().try_into()?);
    Ok(format!("{key_id:016X}"))
}

pub(crate) fn verify_archive_signature(
    archive_path: &Path,
    archive_password: Option<&str>,
    cancel: &AtomicBool,
) -> Result<Verification> {
    let report = inspect_archive(archive_path, archive_password, cancel)?;
    let signatures = signature_entries(&report);
    ensure!(signatures.len() == 1, "The archive must contain exactly one signature record");
    ensure!(
        signatures[0].size <= MAX_SIGNATURE_BYTES,
        "The embedded signature record is too large"
    );
    let mut signature_file = archive::extract_entry_with_password(
        archive_path,
        SIGNATURE_ENTRY,
        archive_password,
    )?;
    let manifest: EmbeddedSignature = serde_json::from_reader(&mut signature_file)
        .context("The embedded signature record is invalid")?;
    ensure!(manifest.version == SIGNATURE_VERSION, "Unsupported embedded signature version");
    ensure!(manifest.payload.version == SIGNATURE_VERSION, "Unsupported signed manifest version");

    let public_key = PublicKeyBox::from_string(&manifest.public_key)?.into_public_key()?;
    let signature = SignatureBox::from_string(&manifest.signature)?;
    let payload_bytes = serde_json::to_vec(&manifest.payload)?;
    minisign::verify(
        &public_key,
        &signature,
        Cursor::new(payload_bytes),
        true,
        false,
        false,
    )
    .context("The embedded Minisign signature is invalid")?;

    let actual_payload = checked_payload(&report)?;
    ensure!(
        actual_payload == manifest.payload,
        "Archive entries do not match the signed manifest"
    );
    let key_id = u64::from_le_bytes(public_key.keynum().try_into()?);
    Ok(Verification {
        key_id: format!("{key_id:016X}"),
        entries: manifest.payload.entries.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use minisign::KeyPair;

    #[test]
    fn embedded_minisign_signature_verifies_contents_and_detects_changes() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("notes.txt");
        fs::write(&source, b"signed archive contents").unwrap();
        let archive_path = directory.path().join("signed.zip");
        archive::create_zip(&archive_path, std::slice::from_ref(&source)).unwrap();

        let keypair = KeyPair::generate_encrypted_keypair(Some("key-passphrase".into())).unwrap();
        let secret_key_path = directory.path().join("signing.key");
        fs::write(&secret_key_path, keypair.sk.to_box(None).unwrap().to_string()).unwrap();

        let cancel = AtomicBool::new(false);
        let key_id = sign_archive(&archive_path, &secret_key_path, "key-passphrase", None, &cancel).unwrap();
        let verified = verify_archive_signature(&archive_path, None, &cancel).unwrap();
        assert_eq!(verified.key_id, key_id);
        assert_eq!(verified.entries, 1);

        archive::replace_archive_entry_with_password(
            &archive_path,
            "notes.txt",
            b"changed contents",
            None,
        )
        .unwrap();
        assert!(verify_archive_signature(&archive_path, None, &cancel).is_err());
    }

    #[gpui::test]
    fn closing_sign_modal_does_not_render_cleared_state(cx: &mut gpui::TestAppContext) {
        let (view, cx) = crate::tests::component_window(cx, |window, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.archive_signature.target = Some(EntryTarget {
                name: "signed.zip".into(),
                key: "signed.zip".into(),
                path: std::path::PathBuf::from("signed.zip"),
                archive_entry: None,
                password: None,
                is_directory: false,
            });
            manager.show_modal(Modal::SignArchive, window, cx);
            manager
        });

        cx.run_until_parked();
        assert!(cx.debug_bounds("archive-signature-content").is_some());
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();

        cx.read(|cx| {
            let manager = view.read(cx);
            assert!(manager.modal.is_none());
            assert!(manager.archive_signature.target.is_none());
            assert!(manager.archive_signature.passphrase.text.is_empty());
        });
    }
}
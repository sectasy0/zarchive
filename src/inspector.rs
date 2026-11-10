//! Coordinates archive inspections and renders their overview, entry, and audit panels.

mod comparison;
mod panes;
use panes::{EntryPane, FindingsPane};
pub(crate) use comparison::{ComparisonDialog, comparison_dialog_content, comparison_footer};

use crate::{
    ArchiveManager, Theme,
    file_actions::EntryTarget,
    inspection::{self, InspectionMode, Integrity, Report},
    password::ArchivePassword,
};
use gpui::component::{
    Disableable, Selectable, Sizable,
    button::{Button, ButtonVariants},
};
use gpui::component::menu::ContextMenuExt;
use gpui::{Context, IntoElement, Window, div, prelude::*, px, rgb};
use std::ops::{Deref, DerefMut};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Clone, Copy, PartialEq, Eq)]
/// Selects the inspector panel currently shown to the user.
pub enum InspectorTab {
    /// Archive metadata and integrity summary.
    Overview,
    /// Virtualized list of archive entries.
    Entries,
    /// Audit findings and safety warnings.
    Findings,
}

/// Owns inspection jobs, reports, active selection, and panel state.
pub struct Inspector {
    sessions: Vec<InspectorSession>,
    active_session: usize,
    next_session_id: u64,
    pub(crate) height: Option<gpui::Pixels>,
    target_tabs_scroll: gpui::ScrollHandle,
}

pub struct InspectorSession {
    id: u64,
    /// Whether the inspector panel is visible.
    pub visible: bool,
    /// Inspection mode used to produce the current reports.
    pub mode: InspectionMode,
    /// Currently selected inspector tab.
    pub tab: InspectorTab,
    /// Reports returned by the latest inspection request.
    pub reports: Vec<Arc<Report>>,
    /// Index of the report currently displayed.
    pub active: usize,
    /// Selected archive entry name, when the Entries tab has a selection.
    pub selected_name: Option<String>,
    /// Whether a batch request was truncated to its inspection limit.
    pub batch_limited: bool,
    pub(crate) text_preview: Option<gpui::Entity<crate::text_preview::Preview>>,
    pub(crate) text_cancel: Arc<AtomicBool>,
    passwords: std::collections::BTreeMap<PathBuf, Option<ArchivePassword>>,
    entries_view: Option<gpui::Entity<EntryPane>>,
    findings_view: Option<gpui::Entity<FindingsPane>>,
    revision: u64,
    source: Option<PathBuf>,
    cancel: Arc<AtomicBool>,
    task: Option<u64>,
}

impl Default for InspectorSession {
    /// Creates a hidden inspector with no reports or active inspection task.
    fn default() -> Self {
        Self {
            id: 0,
            visible: false,
            mode: InspectionMode::Preview,
            tab: InspectorTab::Overview,
            reports: Vec::new(),
            active: 0,
            selected_name: None,
            batch_limited: false,
            text_preview: None,
            text_cancel: Arc::new(AtomicBool::new(false)),
            passwords: std::collections::BTreeMap::new(),
            entries_view: None,
            findings_view: None,
            revision: 0,
            source: None,
            cancel: Arc::new(AtomicBool::new(false)),
            task: None,
        }
    }
}

impl Default for Inspector {
    fn default() -> Self {
        Self {
            sessions: vec![InspectorSession::default()],
            active_session: 0,
            next_session_id: 1,
            height: None,
            target_tabs_scroll: gpui::ScrollHandle::new(),
        }
    }
}

impl Deref for Inspector {
    type Target = InspectorSession;

    fn deref(&self) -> &Self::Target {
        &self.sessions[self.active_session]
    }
}

impl DerefMut for Inspector {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.sessions[self.active_session]
    }
}

impl Inspector {
    pub(crate) fn select_or_create(&mut self, source: Option<PathBuf>) -> u64 {
        let existing = source.as_ref().and_then(|source| {
            self.sessions
                .iter()
                .position(|session| session.source.as_ref() == Some(source))
        });
        let index = existing.unwrap_or_else(|| {
            let active_is_empty = self.sessions[self.active_session].source.is_none()
                && self.sessions[self.active_session].reports.is_empty()
                && self.sessions[self.active_session].text_preview.is_none();
            if active_is_empty {
                self.active_session
            } else {
                let id = self.next_session_id;
                self.next_session_id = self.next_session_id.wrapping_add(1);
                let mut session = InspectorSession::default();
                session.id = id;
                self.sessions.push(session);
                self.sessions.len() - 1
            }
        });
        self.active_session = index;
        self.target_tabs_scroll.scroll_to_item(index);
        let session = &mut self.sessions[index];
        session.source = source;
        session.visible = true;
        session.id
    }

    fn active_session_id(&self) -> u64 {
        self.sessions[self.active_session].id
    }

    fn session_mut(&mut self, id: u64) -> Option<&mut InspectorSession> {
        self.sessions.iter_mut().find(|session| session.id == id)
    }

    fn activate(&mut self, id: u64) {
        if let Some(index) = self.sessions.iter().position(|session| session.id == id) {
            self.active_session = index;
            self.sessions[index].visible = true;
            self.target_tabs_scroll.scroll_to_item(index);
        }
    }

    fn close_tab(&mut self, index: usize) {
        if self.sessions.len() <= 1 || index >= self.sessions.len() {
            self.close();
            return;
        }
        self.sessions.remove(index);
        if self.active_session > index {
            self.active_session -= 1;
        } else if self.active_session == index {
            self.active_session = index.min(self.sessions.len() - 1);
        }
        self.target_tabs_scroll.scroll_to_item(self.active_session);
        self.sessions[self.active_session].visible = true;
    }

    pub(crate) fn close_active_tab(&mut self) {
        self.close_tab(self.active_session);
    }

    fn tab_label(&self, index: usize, language: crate::i18n::Language) -> String {
        let session = &self.sessions[index];
        session
            .source
            .as_deref()
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned())
            .or_else(|| session.source.as_deref().map(|path| path.display().to_string()))
            .unwrap_or_else(|| language.text("Inspector").to_owned())
    }

    #[cfg(test)]
    pub(crate) fn test_session_count(&self) -> usize {
        self.sessions.len()
    }

    #[cfg(test)]
    pub(crate) fn test_has_preview_for(&self, source: &Path) -> bool {
        self.sessions.iter().any(|session| {
            session.source.as_deref() == Some(source) && session.text_preview.is_some()
        })
    }

    fn reset_active(&mut self) {
        self.passwords.clear();
        self.clear_text_preview();
        self.text_cancel = Arc::new(AtomicBool::new(false));
        self.cancel.store(true, Ordering::Relaxed);
        self.cancel = Arc::new(AtomicBool::new(false));
        self.task = None;
        self.entries_view = None;
        self.findings_view = None;
        self.reports.clear();
        self.active = 0;
        self.selected_name = None;
        self.batch_limited = false;
        self.revision = self.revision.wrapping_add(1);
    }

    pub(crate) fn preview_height(&self, window_height: gpui::Pixels) -> gpui::Pixels {
        let maximum = (window_height - px(220.0)).max(px(80.0));
        self.height.unwrap_or((window_height * 0.4).min(px(258.0))).clamp(px(80.0), maximum)
    }

    pub(crate) fn clear_text_preview(&mut self) {
        self.text_cancel.store(true, Ordering::Relaxed);
        self.text_preview = None;
    }

    /// Reports whether an inspection task is still running.
    pub fn is_busy(&self) -> bool {
        self.task.is_some()
    }

    /// Cancels pending inspection work and clears panel-local state.
    pub fn close(&mut self) {
        self.passwords.clear();
        self.clear_text_preview();
        self.cancel.store(true, Ordering::Relaxed);
        self.task = None;
        self.entries_view = None;
        self.findings_view = None;
        self.visible = false;
        self.revision = self.revision.wrapping_add(1);
    }

}

impl Drop for Inspector {
    fn drop(&mut self) {
        for session in &self.sessions {
            session.cancel.store(true, Ordering::Relaxed);
            session.text_cancel.store(true, Ordering::Relaxed);
        }
    }
}

impl Drop for InspectorSession {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.text_cancel.store(true, Ordering::Relaxed);
    }
}

#[derive(Clone)]
pub(crate) struct PreviewResize {
    pub bottom: gpui::Pixels,
    pub maximum: gpui::Pixels,
}

impl gpui::Render for PreviewResize {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size(px(0.0))
    }
}

pub(crate) fn resize_preview(
    manager: &mut ArchiveManager,
    event: &gpui::DragMoveEvent<PreviewResize>,
    _: &mut Window,
    cx: &mut Context<ArchiveManager>,
) {
    if manager.modal.is_some() { return; }
    let drag = event.drag(cx);
    manager.inspector.height = Some((drag.bottom - event.event.position.y).clamp(px(80.0), drag.maximum));
    cx.notify();
}

impl ArchiveManager {
    /// Resolves the selected filesystem item or current archive for inspection.
    pub fn inspection_target(&self) -> Option<(PathBuf, Option<ArchivePassword>)> {
        if self.show_favorites {
            return None;
        }
        if let Some(target) = self.selected_search_target() { return Some((target.path.clone(), target.password.clone())); }
        if let Some(directory) = &self.current_directory {
            let selected = self.selected_entry.as_deref()?;
            let entry = self
                .entries
                .iter()
                .find(|entry| entry.source_path.as_deref().unwrap_or(&entry.name) == selected)?;
            if entry.is_directory {
                return None;
            }
            return Some((
                directory.join(entry.source_path.as_deref().unwrap_or(&entry.name)),
                None,
            ));
        }
        self.archive_path
            .clone()
            .map(|path| (path, self.archive_password.clone()))
    }

    /// Updates the preview selection or starts a preview for a newly selected archive.
    pub fn preview_archive_target(
        &mut self,
        target: &EntryTarget,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if !target.is_directory && (target.archive_entry.is_some()
            || !crate::archive::is_supported_archive(&target.path)
            || crate::archive::ResolvedFormat::from_file(&target.path).is_some_and(|format| format.is_single_compressed()))
        {
            self.preview_text_target(target, window, cx);
            return;
        }
        self.inspector.clear_text_preview();
        if target.archive_entry.is_some() {
            if self
                .inspector
                .reports
                .get(self.inspector.active)
                .is_none_or(|report| report.path != target.path)
                && self.inspector.source.as_ref() != Some(&target.path)
            {
                self.inspect_sources(
                    vec![(target.path.clone(), target.password.clone())],
                    InspectionMode::Preview,
                    window,
                    cx,
                );
            }
            self.inspector.selected_name = target.archive_entry.clone();
            if let Some(pane) = &self.inspector.entries_view {
                pane.update(cx, |pane, cx| {
                    pane.select_name(target.archive_entry.clone());
                    cx.notify();
                });
            }
            cx.notify();
            return;
        }
        if !target.is_directory && crate::archive::is_supported_archive(&target.path) {
            if self.inspector.visible
                && self.inspector.mode == InspectionMode::Preview
                && self.inspector.source.as_ref() == Some(&target.path)
            {
                return;
            }
            self.inspect_sources(
                vec![(target.path.clone(), target.password.clone())],
                InspectionMode::Preview,
                window,
                cx,
            );
        } else if self.inspector.mode == InspectionMode::Preview {
            self.inspector.close();
        }
    }

    /// Starts an inspection for the current archive or selected archive file.
    pub fn inspect_current(
        &mut self,
        mode: InspectionMode,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(target) = self.inspection_target() {
            self.inspect_sources(vec![target], mode, window, cx);
        }
    }

    /// Runs a bounded batch of inspections and publishes reports asynchronously.
    pub fn inspect_sources(
        &mut self,
        mut sources: Vec<(PathBuf, Option<ArchivePassword>)>,
        mode: InspectionMode,
        _window: &Window,
        cx: &mut Context<Self>,
    ) {
        let preserved_audit_findings = if mode == InspectionMode::Integrity && sources.len() == 1 {
            self.inspector
                .reports
                .get(self.inspector.active)
                .filter(|report| report.path == sources[0].0)
                .map(|report| report.findings.clone())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let target_source = (sources.len() == 1).then(|| sources[0].0.clone());
        self.inspector.select_or_create(target_source.clone());
        self.inspector.reset_active();
        self.inspector.passwords = sources.iter().cloned().collect();
        self.inspector.batch_limited = sources.len() > 32 && !mode.is_full_audit();
        if !mode.is_full_audit() {
            sources.truncate(32);
        }
        self.inspector.source = target_source;
        self.inspector.visible = true;
        self.inspector.mode = mode;
        self.inspector.active = 0;
        self.inspector.reports.clear();
        self.inspector.entries_view = None;
        self.inspector.findings_view = None;
        self.inspector.selected_name = None;
        self.inspector.tab = if mode.is_audit() {
            InspectorTab::Findings
        } else if mode == InspectionMode::Preview {
            InspectorTab::Entries
        } else {
            InspectorTab::Overview
        };
        self.inspector.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.inspector.cancel.clone();
        let control = crate::task_queue::TaskControl::with_cancel(cancel.clone()).for_paths(sources.iter().map(|(path, _)| path.clone()));
        let total = sources.len() as u64;
        let revision = self.inspector.revision;
        let session_id = self.inspector.active_session_id();
        let audit_options = inspection::AuditOptions { max_depth: self.advanced_settings.audit_max_depth.min(8) as u8 };
        self.inspector.task = Some(self.background_queue.enqueue(
            "Inspecting archive...",
            control,
            move |control, reporter| async move {
            reporter.phase("Inspecting archive...");
            reporter.set(0, total);
            let mut reports = Vec::new();
            for (path, password) in sources {
                if !control.checkpoint() { anyhow::bail!("cancelled"); }
                reporter.phase(path.file_name().unwrap_or_default().to_string_lossy());
                reports.push(inspection::inspect_with_options(
                    &path,
                    password.as_ref().map(|password| password.as_str()),
                    mode,
                    &cancel,
                    audit_options,
                ));
                reporter.advance(1);
            }
            let mut hashes = std::collections::BTreeMap::new();
            if mode.is_audit() {
                for report in &mut reports {
                    if let Some(hash) = &report.archive_sha256 {
                        if let Some(previous) =
                            hashes.insert(hash.clone(), report.path.display().to_string())
                        {
                            report.findings.push(inspection::Finding {
                                kind: "Duplicate archive",
                                entry: String::new(),
                                detail: previous,
                            });
                        }
                    }
                }
            }
            if !preserved_audit_findings.is_empty() {
                for report in &mut reports {
                    for finding in &preserved_audit_findings {
                        if !report.findings.iter().any(|current| {
                            current.kind == finding.kind
                                && current.entry == finding.entry
                                && current.detail == finding.detail
                        }) {
                            report.findings.push(finding.clone());
                        }
                    }
                    report.findings.truncate(500);
                }
            }
            if !control.checkpoint() { anyhow::bail!("cancelled"); }
            Ok(reports)
            },
            move |this, result, _, cx| {
                let Some(session) = this.inspector.session_mut(session_id) else { return; };
                if session.revision != revision || !session.visible { return; }
                session.task = None;
                if let Ok(reports) = result {
                    session.reports = reports.into_iter().map(Arc::new).collect();
                }
                cx.notify();
            },
            cx,
        ));
        cx.notify();
    }

    fn export_inspection_report(&mut self, cx: &mut Context<Self>) {
        if self.inspector.reports.is_empty() {
            return;
        }
        let language = self.language;
        let mut dialog = rfd::FileDialog::new()
            .set_title(language.text("Save inspection report"))
            .set_file_name("zarchive-inspection-report.json")
            .add_filter("JSON", &["json"]);
        if let Some(parent) = self
            .inspector
            .reports
            .get(self.inspector.active)
            .and_then(|report| report.path.parent())
        {
            dialog = dialog.set_directory(parent);
        }
        let Some(path) = dialog.save_file() else {
            return;
        };
        if self
            .inspector
            .reports
            .iter()
            .any(|report| report.path == path)
        {
            self.status_message = Some(
                language
                    .text("Report path must differ from archive")
                    .to_owned(),
            );
            cx.notify();
            return;
        }
        let reports = self.inspector.reports.clone();
        let mode = self.inspector.mode;
        let batch_limited = self.inspector.batch_limited;
        self.background_queue.enqueue(
            "Exporting report...",
            crate::task_queue::TaskControl::default().for_paths([path.clone()]),
            move |_, _| async move {
                let data = report_bundle_json(mode, batch_limited, &reports);
                std::fs::write(path, serde_json::to_vec_pretty(&data)?)?;
                Ok(())
            },
            move |this, result, _, cx| {
                match result {
                    Ok(()) => {
                        this.status_message = Some(this.language.text("Report saved").to_owned())
                    }
                    Err(crate::task_queue::TaskFailure::Cancelled) => {}
                    Err(crate::task_queue::TaskFailure::Failed(error)) => {
                        this.status_message = Some(format!(
                            "{}: {error:#}",
                            this.language.text("Could not save report")
                        ));
                    }
                }
                cx.notify();
            },
            cx,
        );
    }


}

/// Renders one label/value pair in the inspection overview.
fn detail(label: &'static str, value: String, theme: Theme) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .min_w(px(0.0))
        .flex_1()
        .py_2()
        .border_b_1()
        .border_color(rgb(theme.subtle_border))
        .child(
            div()
                .text_xs()
                .text_color(rgb(theme.muted_text))
                .child(label),
        )
        .child(div().text_sm().min_w(px(0.0)).truncate().child(value))
}

fn archive_metadata_overview(
    report: &Report,
    language: crate::i18n::Language,
    theme: Theme,
) -> impl IntoElement {
    let accessed = report
        .accessed
        .clone()
        .unwrap_or_else(|| "-".to_owned());
    let permissions = report
        .filesystem_permissions
        .clone()
        .unwrap_or_else(|| "-".to_owned());
    let readonly = report.filesystem_readonly.map_or_else(
        || "-".to_owned(),
        |readonly| language.text(if readonly { "Read-only" } else { "Read/write" }).to_owned(),
    );
    let filesystem_owner = match (report.filesystem_owner_id, report.filesystem_group_id) {
        (Some(owner), Some(group)) => format!("UID {owner} / GID {group}"),
        (Some(owner), None) => format!("UID {owner}"),
        (None, Some(group)) => format!("GID {group}"),
        (None, None) => "-".to_owned(),
    };
    let permission_entries = report
        .entries
        .iter()
        .filter(|entry| entry.attributes.is_some())
        .count();
    let ownership_entries = report
        .entries
        .iter()
        .filter(|entry| entry.owner_id.is_some() || entry.group_id.is_some())
        .count();
    let links = report.entries.iter().filter(|entry| entry.link).count();
    let comments = report.entries.iter().filter(|entry| entry.comment.is_some()).count();
    let extra_fields = report
        .entries
        .iter()
        .filter(|entry| entry.extra_fields.is_some())
        .count();
    let mut compression_methods = std::collections::BTreeMap::<String, usize>::new();
    for entry in report.entries.iter().filter(|entry| !entry.directory) {
        *compression_methods.entry(entry.method.clone()).or_default() += 1;
    }
    let compression_methods = if compression_methods.is_empty() {
        "-".to_owned()
    } else {
        compression_methods
            .iter()
            .map(|(method, count)| {
                format!("{} × {count}", language.text_data(method))
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    let archive_comment = report
        .archive_comment
        .as_deref()
        .unwrap_or("-")
        .to_owned();
    let entry_count = report.entries.len();

    div()
        .id("archive-metadata-overview")
        .debug_selector(|| "archive-metadata-overview".to_owned())
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .pt_2()
                .text_sm()
                .font_weight(gpui::FontWeight::MEDIUM)
                .child(language.text("Archive file")),
        )
        .child(
            div()
                .flex()
                .gap_4()
                .child(detail(
                    language.text("Filesystem permissions"),
                    permissions,
                    theme,
                ))
                .child(detail(language.text("Filesystem state"), readonly, theme)),
        )
        .child(
            div()
                .flex()
                .gap_4()
                .child(detail(language.text("Owner/group"), filesystem_owner, theme))
                .child(detail(language.text("Accessed"), accessed, theme)),
        )
        .child(
            div()
                .flex()
                .gap_4()
                .child(detail(
                    language.text("Archive comment"),
                    archive_comment,
                    theme,
                ))
                .child(detail(
                    language.text("Integrity"),
                    language
                        .text_data(&format!("{:?}", report.integrity))
                        .to_owned(),
                    theme,
                )),
        )
        .child(
            div()
                .pt_2()
                .text_sm()
                .font_weight(gpui::FontWeight::MEDIUM)
                .child(language.text("Entry metadata")),
        )
        .child(
            div()
                .flex()
                .gap_4()
                .child(detail(
                    language.text("Files"),
                    report
                        .entries
                        .iter()
                        .filter(|entry| !entry.directory)
                        .count()
                        .to_string(),
                    theme,
                ))
                .child(detail(
                    language.text("Directories"),
                    report
                        .entries
                        .iter()
                        .filter(|entry| entry.directory)
                        .count()
                        .to_string(),
                    theme,
                )),
        )
        .child(
            div()
                .flex()
                .gap_4()
                .child(detail(
                    language.text("Entries with permissions"),
                    format!("{permission_entries} / {entry_count}"),
                    theme,
                ))
                .child(detail(
                    language.text("Entries with owner/group"),
                    format!("{ownership_entries} / {entry_count}"),
                    theme,
                )),
        )
        .child(
            div()
                .flex()
                .gap_4()
                .child(detail(language.text("Links"), links.to_string(), theme))
                .child(detail(
                    language.text("Encrypted entries"),
                    report
                        .entries
                        .iter()
                        .filter(|entry| entry.encrypted)
                        .count()
                        .to_string(),
                    theme,
                )),
        )
        .child(
            div()
                .flex()
                .gap_4()
                .child(detail(
                    language.text("Entry comments"),
                    comments.to_string(),
                    theme,
                ))
                .child(detail(
                    language.text("Extra fields"),
                    extra_fields.to_string(),
                    theme,
                )),
        )
        .child(detail(
            language.text("Compression methods"),
            compression_methods,
            theme,
        ))
}

/// Returns the archive filename, falling back to the full path when needed.
fn archive_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Removes an archive path prefix and redundant separators from an entry name.
fn entry_display_name(name: &str, archive_path: &Path) -> String {
    let mut name = name.replace('\\', "/");
    let archive_path = archive_path.to_string_lossy().replace('\\', "/");
    if let Some(relative) = name.strip_prefix(&archive_path) {
        if relative.is_empty() {
            name = archive_label(Path::new(&archive_path));
        } else if let Some(relative) = relative.strip_prefix('/') {
            name = relative.to_owned();
        }
    }
    name = name.trim_start_matches('/').to_owned();
    while let Some(relative) = name.strip_prefix("./") {
        name = relative.to_owned();
    }
    name
}

#[derive(Clone, Copy)]
/// Selects the semantic color treatment for an inspector notice.
enum NoticeTone {
    /// Successful integrity result.
    Good,
    /// Incomplete or password-gated result.
    Warning,
    /// Failed integrity result.
    Error,
    /// Informational state that is not an error or success.
    Neutral,
}

/// Renders a colored status notice with an optional action button.
fn notice(
    title: &'static str,
    description: &'static str,
    tone: NoticeTone,
    theme: Theme,
    action: Option<Button>,
) -> impl IntoElement {
    let dark = theme.window < 0x808080;
    let (background, foreground) = match (tone, dark) {
        (NoticeTone::Good, true) => (0x183b2c, 0x86efac),
        (NoticeTone::Good, false) => (0xdcfce7, 0x166534),
        (NoticeTone::Warning, true) => (0x3d3018, 0xfcd34d),
        (NoticeTone::Warning, false) => (0xfff7d6, 0x854d0e),
        (NoticeTone::Error, true) => (0x421e1e, 0xfca5a5),
        (NoticeTone::Error, false) => (0xfee2e2, 0xb91c1c),
        (NoticeTone::Neutral, true) => (0x2b2b2e, 0xd4d4d8),
        (NoticeTone::Neutral, false) => (0xf4f4f5, 0x3f3f46),
    };
    div()
        .debug_selector(|| "inspector-health-notice".to_owned())
        .flex()
        .items_center()
        .gap_3()
        .w_full()
        .rounded_md()
        .border_1()
        .border_color(rgb(foreground))
        .bg(rgb(background))
        .px_3()
        .py_2()
        .child(
            div()
                .w(px(3.0))
                .h(px(30.0))
                .rounded_md()
                .bg(rgb(foreground)),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .flex_1()
                .min_w(px(0.0))
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(rgb(foreground))
                        .child(title),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(foreground))
                        .child(description),
                ),
        )
        .when_some(action, |notice, action| notice.child(action))
}

/// Maps an inspection integrity result to its localized notice and optional unlock action.
fn inspection_notice(
    report: &Report,
    language: crate::i18n::Language,
    theme: Theme,
    cx: &mut Context<ArchiveManager>,
) -> impl IntoElement {
    let (title, description, tone) = match report.integrity {
        Integrity::Passed => (
            "Everything looks good",
            "No issues were found in the checked data",
            NoticeTone::Good,
        ),
        Integrity::NotTested => (
            "Integrity has not been tested",
            "Run a test to verify archive contents",
            NoticeTone::Neutral,
        ),
        Integrity::Failed => (
            "Integrity check failed",
            "Some archive data could not be checked",
            NoticeTone::Error,
        ),
        Integrity::Incomplete => (
            "Inspection incomplete",
            "Some archive data could not be checked",
            NoticeTone::Warning,
        ),
        Integrity::PasswordRequired => (
            "Password required",
            "This archive requires a password for verification",
            NoticeTone::Warning,
        ),
    };
    let action = (report.integrity == Integrity::PasswordRequired).then(|| {
        let path = report.path.clone();
        Button::new("inspector-unlock-archive")
            .primary()
            .small()
            .label(language.text("Unlock"))
            .on_click(
                cx.listener(move |this, _, _, cx| this.request_archive_password(path.clone(), cx)),
            )
    });
    notice(
        language.text(title),
        language.text(description),
        tone,
        theme,
        action,
    )
}

/// Returns the number of findings reported by the selected archive audit.
fn audit_findings_count(report: &Report) -> usize {
    report.findings.len()
}

fn report_bundle_json(
    mode: InspectionMode,
    batch_limited: bool,
    reports: &[Arc<Report>],
) -> serde_json::Value {
    serde_json::json!({
        "mode": format!("{mode:?}"),
        "batch_limited": batch_limited,
        "reports": reports.iter().map(|report| report.json()).collect::<Vec<_>>(),
    })
}

/// Renders one placeholder bar for a loading skeleton.
pub(crate) fn skeleton_bar(theme: Theme, width: f32, height: f32) -> impl IntoElement {
    div()
        .w(px(width))
        .h(px(height))
        .rounded_md()
        .bg(rgb(theme.subtle_border))
}

/// Renders a label/value placeholder for the overview loading state.
fn skeleton_field(theme: Theme, value_width: f32) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w(px(0.0))
        .gap_2()
        .py_2()
        .border_b_1()
        .border_color(rgb(theme.subtle_border))
        .child(skeleton_bar(theme, 74.0, 8.0))
        .child(skeleton_bar(theme, value_width, 12.0))
}

/// Builds the loading skeleton for the Overview tab.
fn overview_skeleton(theme: Theme) -> impl IntoElement {
    div()
        .id("inspector-loading-overview")
        .debug_selector(|| "inspector-loading-overview".to_owned())
        .flex()
        .flex_col()
        .w_full()
        .gap_2()
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .w_full()
                .h(px(54.0))
                .px_3()
                .rounded_md()
                .border_1()
                .border_color(rgb(theme.border))
                .bg(rgb(theme.sidebar))
                .child(skeleton_bar(theme, 3.0, 30.0))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(skeleton_bar(theme, 148.0, 9.0))
                        .child(skeleton_bar(theme, 226.0, 8.0)),
                ),
        )
        .child(skeleton_bar(theme, 124.0, 28.0))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .gap_4()
                        .child(skeleton_field(theme, 118.0))
                        .child(skeleton_field(theme, 94.0)),
                )
                .child(
                    div()
                        .flex()
                        .gap_4()
                        .child(skeleton_field(theme, 84.0))
                        .child(skeleton_field(theme, 128.0)),
                )
                .child(
                    div()
                        .flex()
                        .gap_4()
                        .child(skeleton_field(theme, 92.0))
                        .child(skeleton_field(theme, 72.0)),
                )
                .child(
                    div()
                        .flex()
                        .gap_4()
                        .child(skeleton_field(theme, 154.0))
                        .child(skeleton_field(theme, 166.0)),
                )
                .child(
                    div()
                        .flex()
                        .gap_4()
                        .child(skeleton_field(theme, 104.0))
                        .child(skeleton_field(theme, 196.0)),
                ),
        )
}

/// Builds the table-shaped loading skeleton for the Entries tab.
fn entries_skeleton(language: crate::i18n::Language, theme: Theme) -> impl IntoElement {
    let header = div()
        .id("inspector-entries-header")
        .debug_selector(|| "inspector-entries-header".to_owned())
        .flex()
        .items_center()
        .w_full()
        .h(px(28.0))
        .flex_shrink_0()
        .pr_2()
        .border_b_1()
        .border_color(rgb(theme.border))
        .bg(rgb(theme.sidebar))
        .child(
            gpui::base::TableHead::new("inspector-entry-name-head", 0)
                .flex()
                .items_center()
                .h_full()
                .min_w(px(0.0))
                .flex_1()
                .text_size(px(10.0))
                .text_color(rgb(theme.muted_text))
                .gap_1()
                .pl_2()
                .child(language.text("NAME")),
        )
        .child(
            gpui::base::TableHead::new("inspector-entry-type-head", 1)
                .flex()
                .items_center()
                .h_full()
                .w(px(130.0))
                .flex_shrink_0()
                .text_size(px(10.0))
                .text_color(rgb(theme.muted_text))
                .pl_2()
                .child(language.text("TYPE")),
        )
        .child(
            gpui::base::TableHead::new("inspector-entry-size-head", 2)
                .flex()
                .items_center()
                .h_full()
                .w(px(90.0))
                .flex_shrink_0()
                .text_size(px(10.0))
                .text_color(rgb(theme.muted_text))
                .pl_2()
                .child(language.text("SIZE")),
        );
    div()
        .id("inspector-loading-entries")
        .debug_selector(|| "inspector-loading-entries".to_owned())
        .flex()
        .flex_col()
        .flex_1()
        .w_full()
        .min_h(px(0.0))
        .overflow_hidden()
        .child(header)
        .children((0..7).map(|index| {
            gpui::base::TableRow::new(
                gpui::SharedString::from(format!("inspector-loading-entry-{index}")),
                index,
            )
            .debug_selector(move || format!("inspector-loading-row-{index}"))
            .flex()
            .items_center()
            .w_full()
            .h(px(32.0))
            .flex_shrink_0()
            .px_2()
            .border_b_1()
            .border_color(rgb(theme.subtle_border))
            .bg(rgb(if index % 2 == 1 {
                theme.sidebar
            } else {
                theme.surface
            }))
            .child(
                gpui::base::TableCell::new("inspector-loading-entry-name", 0)
                    .flex()
                    .flex_1()
                    .min_w(px(0.0))
                    .items_center()
                    .gap_2()
                    .child(skeleton_bar(theme, 16.0, 16.0))
                    .child(skeleton_bar(theme, 156.0 - index as f32 * 8.0, 9.0)),
            )
            .child(
                gpui::base::TableCell::new("inspector-loading-entry-type", 1)
                    .w(px(130.0))
                    .flex_shrink_0()
                    .pl_2()
                    .child(skeleton_bar(theme, 44.0 + index as f32 * 4.0, 9.0)),
            )
            .child(
                gpui::base::TableCell::new("inspector-loading-entry-size", 2)
                    .w(px(90.0))
                    .flex_shrink_0()
                    .pl_2()
                    .child(skeleton_bar(theme, 52.0 - index as f32 * 3.0, 9.0)),
            )
        }))
}

/// Builds placeholder rows for the Findings tab while an audit runs.
fn findings_skeleton(theme: Theme) -> impl IntoElement {
    div()
        .id("inspector-loading-findings")
        .debug_selector(|| "inspector-loading-findings".to_owned())
        .flex()
        .flex_col()
        .flex_1()
        .min_h(px(0.0))
        .w_full()
        .overflow_hidden()
        .children((0..6).map(|index| {
            div()
                .debug_selector(move || format!("inspector-loading-finding-{index}"))
                .flex()
                .flex_col()
                .w_full()
                .gap_2()
                .px_3()
                .py_3()
                .border_b_1()
                .border_color(rgb(theme.subtle_border))
                .child(skeleton_bar(theme, 112.0 + (index % 3) as f32 * 28.0, 9.0))
                .child(skeleton_bar(theme, 196.0 - (index % 2) as f32 * 36.0, 8.0))
                .child(skeleton_bar(theme, 268.0 - (index % 3) as f32 * 32.0, 8.0))
        }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::test::TestWindowExt;
    use std::io::Write;

    #[test]
    fn report_bundle_contains_mode_batch_state_and_reports() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples/14-audit-findings.zip");
        let report = Arc::new(inspection::inspect(
            &path,
            None,
            InspectionMode::FullAudit,
            &AtomicBool::new(false),
        ));
        let bundle = report_bundle_json(InspectionMode::FullAudit, false, &[report]);
        assert_eq!(bundle["mode"], "FullAudit");
        assert_eq!(bundle["batch_limited"], false);
        assert_eq!(bundle["reports"][0]["full_audit"], true);
        assert_eq!(bundle["reports"][0]["limits"]["max_entries"], serde_json::Value::Null);
    }

    #[gpui::test]
    fn inspector_targets_open_as_independent_tabs(cx: &mut gpui::TestAppContext) {
        let first = PathBuf::from("first.zip");
        let second = PathBuf::from("second.zip");
        let (view, cx) = crate::tests::component_window(cx, |_, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.inspector.select_or_create(Some(first.clone()));
            manager.inspector.select_or_create(Some(second.clone()));
            manager
        });
        assert!(cx.debug_bounds("inspector-target-tabs").is_some());
        let first_tab = cx.debug_bounds("inspector-target-tab:0").unwrap();
        cx.simulate_click(first_tab.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        cx.read(|cx| {
            let manager = view.read(cx);
            assert_eq!(manager.inspector.source.as_ref(), Some(&first));
            assert_eq!(manager.inspector.sessions.len(), 2);
        });
    }

    #[gpui::test]
    fn overview_renders_archive_and_entry_metadata(cx: &mut gpui::TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("notes.txt");
        let path = directory.path().join("archive.zip");
        std::fs::write(&source, b"overview metadata").unwrap();
        crate::archive::create_archive(
            &path,
            std::slice::from_ref(&source),
            crate::archive::CreateOptions::default(),
        )
        .unwrap();
        let report = Arc::new(inspection::inspect(
            &path,
            None,
            InspectionMode::Audit,
            &AtomicBool::new(false),
        ));
        let (view, cx) = crate::tests::component_window(cx, |_, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.inspector.visible = true;
            manager.inspector.tab = InspectorTab::Overview;
            manager.inspector.reports = vec![report];
            manager
        });
        assert!(cx.debug_bounds("archive-metadata-overview").is_some());
        assert!(cx.read(|cx| view.read(cx).inspector.reports[0]
            .filesystem_permissions
            .is_some()));
    }

    #[gpui::test]
    fn compare_archives_command_opens_two_independent_file_fields(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = crate::tests::component_window(cx, |_, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.current_directory = None;
            manager
        });
        click_inspect_action(cx, "Compare archives");
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("comparison-archive-a").is_some());
        assert!(cx.debug_bounds("comparison-archive-b").is_some());
        cx.read(|cx| {
            let manager = view.read(cx);
            assert!(matches!(manager.modal, Some(crate::Modal::CompareArchives)));
            assert!(!manager.inspector.visible);
        });
    }

    #[gpui::test]
    fn comparison_dialog_renders_separate_inputs_and_filters_results(
        cx: &mut gpui::TestAppContext,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let path_a = directory.path().join("a.zip");
        let path_b = directory.path().join("b.zip");
        let mut zip_a = zip::ZipWriter::new(std::fs::File::create(&path_a).unwrap());
        let mut zip_b = zip::ZipWriter::new(std::fs::File::create(&path_b).unwrap());
        for (zip, changed) in [(&mut zip_a, b"old".as_slice()), (&mut zip_b, b"new")] {
            zip.start_file("changed.txt", zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(changed).unwrap();
            zip.start_file("common.txt", zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b"same").unwrap();
        }
        zip_a.finish().unwrap();
        zip_b.finish().unwrap();
        let report = Arc::new(
            crate::comparison::compare_archives(
                &path_a,
                &path_b,
                &AtomicBool::new(false),
            )
            .unwrap(),
        );
        let (view, cx) = crate::tests::component_window(cx, |window, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.comparison_dialog.path_a = Some(path_a);
            manager.comparison_dialog.path_b = Some(path_b);
            manager.comparison_dialog.report = Some(report);
            manager.show_modal(crate::Modal::CompareArchives, window, cx);
            manager
        });
        assert!(cx.debug_bounds("comparison-archive-a").is_some());
        assert!(cx.debug_bounds("comparison-archive-b").is_some());
        assert!(cx.debug_bounds("archive-comparison-summary").is_some());
        assert!(cx.debug_bounds("archive-comparison-list").is_some());
        assert!(cx.debug_bounds("archive-comparison-item-0").is_some());
        assert!(cx.debug_bounds("archive-comparison-item-1").is_some());
        let dialog_bounds = cx.debug_bounds("dialog-0").unwrap();
        let footer_bounds = cx.debug_bounds("comparison-actions").unwrap();
        let report_actions_bounds = cx.debug_bounds("comparison-report-actions").unwrap();
        let copy_bounds = cx.debug_bounds("copy-comparison-report").unwrap();
        let save_bounds = cx.debug_bounds("save-comparison-report").unwrap();
        let start_bounds = cx.debug_bounds("comparison-start").unwrap();
        assert!(report_actions_bounds.left() >= dialog_bounds.left());
        assert!(copy_bounds.right() <= save_bounds.left());
        assert!(save_bounds.right() <= start_bounds.left());
        assert!(save_bounds.bottom() <= footer_bounds.bottom());
        assert!(start_bounds.right() <= dialog_bounds.right());
        assert!(start_bounds.right() >= footer_bounds.right() - px(2.0));
        cx.update(|window, cx| window.click("Differences", cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("archive-comparison-item-0").is_some());
        assert!(cx.debug_bounds("archive-comparison-item-1").is_none());
        assert!(cx.read(|cx| view.read(cx).comparison_dialog.view.is_some()));
        assert!(cx.read(|cx| !view.read(cx).inspector.visible));
    }

    #[gpui::test]
    fn comparison_task_publishes_its_report_to_the_modal(cx: &mut gpui::TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("shared.txt");
        std::fs::write(&source, b"shared content").unwrap();
        let path_a = directory.path().join("a.zip");
        let path_b = directory.path().join("b.zip");
        crate::archive::create_archive(
            &path_a,
            std::slice::from_ref(&source),
            crate::archive::CreateOptions::default(),
        )
        .unwrap();
        crate::archive::create_archive(
            &path_b,
            std::slice::from_ref(&source),
            crate::archive::CreateOptions::default(),
        )
        .unwrap();
        let (view, cx) = crate::tests::component_window(cx, |window, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.current_directory = None;
            manager.comparison_dialog.path_a = Some(path_a.clone());
            manager.comparison_dialog.path_b = Some(path_b.clone());
            manager.show_modal(crate::Modal::CompareArchives, window, cx);
            manager.start_archive_comparison(cx);
            manager
        });
        cx.run_until_parked();
        cx.read(|cx| {
            let manager = view.read(cx);
            assert!(!manager.comparison_dialog.is_busy());
            assert!(manager.comparison_dialog.report.is_some());
            assert!(matches!(manager.modal, Some(crate::Modal::CompareArchives)));
            assert!(!manager.inspector.visible);
        });
    }

    #[gpui::test]
    fn entries_context_menu_extracts_removes_and_accepts_new_files(cx: &mut gpui::TestAppContext) {
        fn finish(view: &gpui::Entity<ArchiveManager>, cx: &mut gpui::VisualTestContext) {
            for _ in 0..24 {
                cx.run_until_parked();
                cx.update(|window, cx| window.simulate_next_frame(cx));
                cx.run_until_parked();
                if cx.read(|cx| {
                    let manager = view.read(cx);
                    !manager.inspector.is_busy() && !manager.file_actions.operations.is_busy()
                }) { return; }
            }
            panic!("inspector operation did not finish");
        }
        fn choose(cx: &mut gpui::VisualTestContext, label: &'static str) {
            cx.update(|window, cx| window.right_click("inspected-entry:0", cx));
            cx.run_until_parked();
            let bounds = cx.update(|window, _| window.within("popup-menu").find_by_label(label).bounds());
            cx.simulate_mouse_move(bounds.center(), None, gpui::Modifiers::default());
            cx.simulate_click(bounds.center(), gpui::Modifiers::default());
            cx.run_until_parked();
        }
        let directory = tempfile::tempdir().unwrap();
        let sources = tempfile::tempdir().unwrap();
        let original = sources.path().join("original.txt");
        let addition = directory.path().join("addition.txt");
        std::fs::write(&original, "old 中文").unwrap();
        std::fs::write(&addition, "new 🙂").unwrap();
        let path = directory.path().join("archive.xar");
        crate::archive::create_archive(&path, std::slice::from_ref(&original), crate::archive::CreateOptions {
            format: crate::archive::ResolvedFormat::Xar, ..Default::default()
        }).unwrap();
        let (view, cx) = crate::tests::component_window(cx, |window, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.current_directory = Some(directory.path().to_path_buf());
            manager.entries = crate::read_directory(directory.path()).unwrap();
            manager.inspect_sources(vec![(path.clone(), None)], InspectionMode::Preview, window, cx);
            manager
        });
        finish(&view, cx);
        choose(cx, "Extract here");
        finish(&view, cx);
        assert_eq!(std::fs::read_to_string(directory.path().join("original.txt")).unwrap(), "old 中文");
        choose(cx, "Remove from archive");
        cx.read(|cx| {
            let manager = view.read(cx);
            assert!(manager.modal == Some(crate::Modal::Delete));
            assert!(manager.file_actions.pending_delete.as_ref().unwrap().archive_entry.is_some());
        });
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
        cx.update(|window, cx| window.click("confirm-delete", cx));
        finish(&view, cx);
        assert!(crate::archive::list_archive(&path).unwrap().is_empty());
        assert!(directory.path().join("original.txt").exists());
        assert!(original.exists());
        assert!(cx.debug_bounds("inspector-entries-drop-area").is_some());
        let start = cx.update(|window, _| window.find(gpui::SharedString::from(addition.to_string_lossy().into_owned())).bounds()).center();
        let end = cx.debug_bounds("inspector-entries-drop-area").unwrap().center();
        cx.simulate_mouse_move(start, None, gpui::Modifiers::default());
        cx.simulate_mouse_down(start, gpui::MouseButton::Left, gpui::Modifiers::default());
        cx.simulate_mouse_move(start + gpui::point(px(20.0), px(0.0)), gpui::MouseButton::Left, gpui::Modifiers::default());
        cx.run_until_parked();
        cx.simulate_mouse_move(end, gpui::MouseButton::Left, gpui::Modifiers::default());
        cx.run_until_parked();
        cx.simulate_mouse_up(end, gpui::MouseButton::Left, gpui::Modifiers::default());
        finish(&view, cx);
        assert!(crate::archive::list_archive(&path).unwrap().iter().any(|entry| entry.name == "addition.txt"));
        assert!(addition.exists());
        cx.read(|cx| {
            let manager = view.read(cx);
            assert!(manager.inspector.visible);
            assert!(manager.inspector.reports[manager.inspector.active].entries.iter().any(|entry| entry.name == "addition.txt"));
        });
    }

    #[gpui::test]
    fn preview_panel_can_be_resized_and_preserves_its_height(cx: &mut gpui::TestAppContext) {
        fn drag_to(cx: &mut gpui::VisualTestContext, height: gpui::Pixels) {
            let handle = cx.debug_bounds("preview-resize-handle").unwrap();
            let bottom = cx.debug_bounds("status-bar").unwrap().top();
            let end = gpui::point(handle.center().x, bottom - height);
            cx.simulate_mouse_move(handle.center(), None, gpui::Modifiers::default());
            cx.simulate_mouse_down(handle.center(), gpui::MouseButton::Left, gpui::Modifiers::default());
            cx.simulate_mouse_move(handle.center() - gpui::point(px(0.0), px(12.0)), gpui::MouseButton::Left, gpui::Modifiers::default());
            cx.run_until_parked();
            cx.simulate_mouse_move(end, gpui::MouseButton::Left, gpui::Modifiers::default());
            cx.run_until_parked();
            cx.simulate_mouse_up(end, gpui::MouseButton::Left, gpui::Modifiers::default());
            cx.run_until_parked();
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preview.txt");
        std::fs::write(&path, "preview\ntext\n").unwrap();
        let target = EntryTarget { name: "preview.txt".into(), key: "preview".into(), path,
            archive_entry: None, password: None, is_directory: false };
        let (view, cx) = crate::tests::component_window(cx, |_, cx| crate::tests::test_manager(cx));
        cx.simulate_resize(gpui::size(px(1100.0), px(720.0)));
        cx.update(|window, cx| view.update(cx, |manager, cx| manager.preview_text_target(&target, window, cx)));
        cx.run_until_parked();
        assert_eq!(cx.debug_bounds("resizable-preview").unwrap().size.height, px(258.0));
        drag_to(cx, px(360.0));
        assert_eq!(cx.debug_bounds("resizable-preview").unwrap().size.height, px(360.0));
        assert!(cx.debug_bounds("file-list").unwrap().size.height > px(80.0));
        let csv_path = directory.path().join("preview.csv");
        std::fs::write(&csv_path, "name,value\nitem,42\n").unwrap();
        let csv_target = EntryTarget { name: "preview.csv".into(), path: csv_path, ..target.clone() };
        cx.update(|window, cx| view.update(cx, |manager, cx| manager.preview_text_target(&csv_target, window, cx)));
        cx.run_until_parked();
        assert_eq!(cx.debug_bounds("resizable-preview").unwrap().size.height, px(360.0));
        assert!(cx.debug_bounds("csv-preview-table").is_some());
        drag_to(cx, px(600.0));
        assert_eq!(cx.debug_bounds("resizable-preview").unwrap().size.height, px(500.0));
        assert!(cx.debug_bounds("file-list").unwrap().size.height > px(80.0));
        drag_to(cx, px(20.0));
        assert_eq!(cx.debug_bounds("resizable-preview").unwrap().size.height, px(80.0));
        view.update(cx, |manager, cx| { manager.inspector.close(); cx.notify(); });
        cx.run_until_parked();
        cx.update(|window, cx| view.update(cx, |manager, cx| manager.preview_text_target(&target, window, cx)));
        cx.run_until_parked();
        assert_eq!(cx.debug_bounds("resizable-preview").unwrap().size.height, px(80.0));
    }

    #[test]
    /// Removes the archive path prefix from entry labels without changing relative names.
    fn entry_names_are_relative_to_the_open_archive() {
        let archive = Path::new("/Users/test/archive.zip");
        assert_eq!(
            entry_display_name("/Users/test/archive.zip/docs/readme.txt", archive),
            "docs/readme.txt"
        );
        assert_eq!(
            entry_display_name("./docs/readme.txt", archive),
            "docs/readme.txt"
        );
        assert_eq!(
            entry_display_name("docs/readme.txt", archive),
            "docs/readme.txt"
        );
    }

    #[test]
    /// Counts every audit finding, including findings added after integrity checks.
    fn audit_findings_count_includes_all_audit_findings() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("damaged.zip");
        std::fs::write(&path, b"not an archive").unwrap();
        let mut report = inspection::inspect(
            &path,
            None,
            InspectionMode::Integrity,
            &AtomicBool::new(false),
        );
        assert_eq!(report.integrity, Integrity::Failed);
        report.findings.push(inspection::Finding {
            kind: "Duplicate content",
            entry: "copy.txt".into(),
            detail: "original.txt".into(),
        });
        assert_eq!(audit_findings_count(&report), report.findings.len());
    }

    #[test]
    /// Uses the complete finding count for a damaged archive report.
    fn damaged_sample_produces_a_failed_integrity_count() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples/12-damaged.zip");
        let report = inspection::inspect(
            &path,
            None,
            InspectionMode::Integrity,
            &AtomicBool::new(false),
        );
        assert_eq!(report.integrity, Integrity::Failed);
        assert_eq!(audit_findings_count(&report), report.findings.len());
    }

    #[gpui::test]
    /// Virtualizes large audit lists and isolates local scrolling from the archive manager.
    fn audit_findings_virtualize_large_reports_without_notifying_archive_manager(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("notes.txt");
        let path = root.path().join("archive.zip");
        std::fs::write(&source, b"notes").unwrap();
        crate::archive::create_archive(&path, &[source], crate::archive::CreateOptions::default())
            .unwrap();
        let mut report =
            inspection::inspect(&path, None, InspectionMode::Audit, &AtomicBool::new(false));
        report.findings = (0..500)
            .map(|index| inspection::Finding {
                kind: "Unsafe path",
                entry: format!("../unsafe-{index}.txt"),
                detail: "Entry could escape an extraction folder".into(),
            })
            .collect();
        let (view, cx) = crate::tests::component_window(cx, |_, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.inspector.visible = true;
            manager.inspector.tab = InspectorTab::Findings;
            manager.inspector.reports = vec![Arc::new(report)];
            manager
        });
        assert!(cx.debug_bounds("inspector-health-notice").is_none());
        assert!(cx.debug_bounds("inspector-audit-finding-0").is_some());
        assert!(cx.debug_bounds("inspector-audit-finding-499").is_none());
        let pane = cx.read(|cx| view.read(cx).inspector.findings_view.clone().unwrap());
        let notifications = std::rc::Rc::new(std::cell::Cell::new(0));
        let observed = notifications.clone();
        let _subscription =
            cx.update(|_, cx| cx.observe(&view, move |_, _| observed.set(observed.get() + 1)));
        pane.update(cx, |pane, cx| {
            pane.scroll
                .scroll_to_item(499, gpui::ScrollStrategy::Bottom);
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("inspector-audit-finding-499").is_some());
        assert!(cx.debug_bounds("inspector-audit-finding-100").is_none());
        assert_eq!(notifications.get(), 0);
    }

    #[gpui::test]
    /// Shows tab-specific loading skeletons with geometry matching loaded content.
    fn loading_inspector_uses_tab_specific_skeletons(cx: &mut gpui::TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("notes.txt");
        let path = root.path().join("archive.zip");
        std::fs::write(&source, b"notes").unwrap();
        crate::archive::create_archive(&path, &[source], crate::archive::CreateOptions::default())
            .unwrap();
        let report = Arc::new(inspection::inspect(
            &path,
            None,
            InspectionMode::Preview,
            &AtomicBool::new(false),
        ));
        let (view, cx) = crate::tests::component_window(cx, |_, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.inspector.visible = true;
            manager.inspector.tab = InspectorTab::Overview;
            manager.inspector.task = Some(1);
            manager
        });
        assert!(cx.debug_bounds("inspector-loading-overview").is_some());

        view.update(cx, |manager, cx| {
            manager.inspector.tab = InspectorTab::Entries;
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("inspector-loading-entries").is_some());
        assert!(cx.debug_bounds("inspector-entries-header").is_some());
        let skeleton_header = cx.debug_bounds("inspector-entries-header").unwrap();
        let skeleton_row = cx.debug_bounds("inspector-loading-row-0").unwrap();

        view.update(cx, |manager, cx| {
            manager.inspector.tab = InspectorTab::Findings;
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("inspector-loading-findings").is_some());

        view.update(cx, |manager, cx| {
            manager.inspector.tab = InspectorTab::Entries;
            manager.inspector.reports = vec![report];
            manager.inspector.task = None;
            cx.notify();
        });
        cx.run_until_parked();
        let loaded_header = cx.debug_bounds("inspector-entries-header").unwrap();
        let loaded_row = cx.debug_bounds("inspected-entry-0").unwrap();
        assert_eq!(skeleton_header.origin, loaded_header.origin);
        assert_eq!(skeleton_header.size, loaded_header.size);
        assert_eq!(skeleton_row.origin, loaded_row.origin);
        assert_eq!(skeleton_row.size, loaded_row.size);
    }

    #[gpui::test]
    /// Virtualizes large entry lists and preserves selection while scrolling or switching reports.
    fn entries_virtualize_twenty_thousand_items_and_keep_selection_visible(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("notes.txt");
        let path = root.path().join("archive.zip");
        std::fs::write(&source, b"notes").unwrap();
        crate::archive::create_archive(&path, &[source], crate::archive::CreateOptions::default())
            .unwrap();
        let mut report = inspection::inspect(
            &path,
            None,
            InspectionMode::Preview,
            &AtomicBool::new(false),
        );
        let template = report.entries[0].clone();
        report.entries = (0..20_000)
            .map(|index| {
                let mut entry = template.clone();
                entry.name = format!("entry-{index:05}.txt");
                entry
            })
            .collect();
        let (view, cx) = crate::tests::component_window(cx, |_, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.current_directory = None;
            manager.entries.clear();
            manager.inspector.visible = true;
            manager.inspector.tab = InspectorTab::Entries;
            manager.inspector.reports = vec![Arc::new(report)];
            manager
        });
        let first_row = cx.debug_bounds("inspected-entry-0").unwrap();
        let header = cx.debug_bounds("inspector-entries-header").unwrap();
        assert!(first_row.size.width > gpui::px(200.0));
        assert!(header.size.width > gpui::px(200.0));
        assert_eq!(
            cx.debug_bounds("inspector-entry-size-head")
                .unwrap()
                .origin
                .x,
            cx.debug_bounds("inspector-entry-size-cell-0")
                .unwrap()
                .origin
                .x
        );
        assert_eq!(
            cx.debug_bounds("inspector-entry-type-head")
                .unwrap()
                .origin
                .x,
            cx.debug_bounds("inspector-entry-type-cell-0")
                .unwrap()
                .origin
                .x
        );
        assert!(cx.debug_bounds("inspector-entry-details").is_none());
        assert!(cx.debug_bounds("inspected-entry-100").is_none());
        assert!(cx.debug_bounds("inspected-entry-19999").is_none());
        assert!(
            cx.debug_bounds("inspected-entry-20").is_none(),
            "offscreen rows must not be rendered"
        );
        let pane = cx.read(|cx| view.read(cx).inspector.entries_view.clone().unwrap());
        let notifications = std::rc::Rc::new(std::cell::Cell::new(0));
        let observed = notifications.clone();
        let _subscription =
            cx.update(|_, cx| cx.observe(&view, move |_, _| observed.set(observed.get() + 1)));
        pane.update(cx, |pane, cx| {
            pane.scroll
                .scroll_to_item(19_999, gpui::ScrollStrategy::Bottom);
            cx.notify();
        });
        cx.run_until_parked();
        let bounds = cx
            .debug_bounds("inspected-entry-19999")
            .expect("last virtual entry is rendered after scrolling");
        assert!(cx.debug_bounds("inspected-entry-100").is_none());
        cx.simulate_mouse_move(bounds.center(), None, gpui::Modifiers::default());
        cx.simulate_click(bounds.center(), gpui::Modifiers::default());
        cx.read(|cx| assert_eq!(pane.read(cx).selected_index, Some(19_999)));
        assert_eq!(
            notifications.get(),
            0,
            "Entries must not invalidate the main archive view"
        );
        assert!(cx.debug_bounds("inspected-entry-19999").is_some());
        view.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        cx.read(|cx| assert_eq!(pane.read(cx).selected_index, Some(19_999)));
        assert!(cx.debug_bounds("inspected-entry-19999").is_some());
        view.update(cx, |manager, cx| {
            manager.inspector.selected_name = Some("entry-00042.txt".into());
            cx.notify();
        });
        cx.run_until_parked();
        cx.read(|cx| assert_eq!(pane.read(cx).selected_index, Some(42)));
        assert!(cx.debug_bounds("inspected-entry-42").is_some());
        let next = inspection::inspect(
            &path,
            None,
            InspectionMode::Preview,
            &AtomicBool::new(false),
        );
        view.update(cx, |manager, cx| {
            manager.inspector.reports.push(Arc::new(next));
            manager.inspector.active = 1;
            manager.inspector.selected_name = None;
            cx.notify();
        });
        cx.run_until_parked();
        cx.read(|cx| {
            assert_eq!(pane.read(cx).selected_index, None);
            assert_eq!(pane.read(cx).report.entries.len(), 1);
        });
        assert!(cx.debug_bounds("inspected-entry-0").is_some());
        assert!(cx.debug_bounds("inspected-entry-19999").is_none());
    }

    #[gpui::test]
    /// Avoids recurring frame polling and duplicate work for the same preview target.
    fn preview_does_not_poll_frames_or_restart_the_same_archive(cx: &mut gpui::TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("notes.txt");
        let path = root.path().join("archive.zip");
        std::fs::write(&source, b"contents").unwrap();
        crate::archive::create_archive(&path, &[source], crate::archive::CreateOptions::default())
            .unwrap();
        let target = EntryTarget {
            name: "archive.zip".into(),
            key: "archive.zip".into(),
            path,
            archive_entry: None,
            password: None,
            is_directory: false,
        };
        let (view, cx) = crate::tests::component_window(cx, |_, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.current_directory = None;
            manager
        });
        cx.update(|window, cx| {
            view.update(cx, |manager, cx| {
                manager.preview_archive_target(&target, window, cx);
                let revision = manager.inspector.revision;
                for _ in 0..20 {
                    manager.preview_archive_target(&target, window, cx);
                }
                assert_eq!(manager.inspector.revision, revision);
            });
            assert_eq!(
                window.simulate_next_frame(cx),
                0,
                "waiting for a preview must not schedule render frames"
            );
        });
        cx.run_until_parked();
        cx.read(|cx| {
            let manager = view.read(cx);
            assert!(!manager.inspector.is_busy());
            assert_eq!(manager.inspector.reports.len(), 1);
        });
        cx.update(|window, cx| {
            view.update(cx, |manager, cx| {
                let revision = manager.inspector.revision;
                manager.preview_archive_target(&target, window, cx);
                assert_eq!(manager.inspector.revision, revision);
                assert!(!manager.inspector.is_busy());
            })
        });
    }

    /// Advances frame callbacks until the current inspection task completes.
    fn complete(cx: &mut gpui::VisualTestContext, view: &gpui::Entity<ArchiveManager>) {
        for _ in 0..8 {
            cx.run_until_parked();
            cx.update(|window, cx| {
                window.simulate_next_frame(cx);
            });
            cx.run_until_parked();
            if !cx.read(|cx| view.read(cx).inspector.is_busy()) {
                break;
            }
        }
    }

    /// Opens the inspection dropdown and activates the item with the requested label.
    fn click_inspect_action(cx: &mut gpui::VisualTestContext, label: &'static str) {
        cx.update(|window, cx| window.click("toolbar-inspect-dropdown", cx));
        cx.run_until_parked();
        cx.update(|window, cx| window.render_frame(cx));
        let bounds =
            cx.update(|window, _| window.within("popup-menu").find_by_label(label).bounds());
        cx.simulate_mouse_move(bounds.center(), None, gpui::Modifiers::default());
        cx.simulate_click(bounds.center(), gpui::Modifiers::default());
        cx.run_until_parked();
    }

    #[gpui::test]
    fn full_audit_menu_completes_for_nested_archives(cx: &mut gpui::TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("invoice.pdf.exe");
        std::fs::write(&source, b"Harmless text for the Full audit menu regression.").unwrap();
        let inner = root.path().join("inner.zip");
        crate::archive::create_zip(&inner, &[source]).unwrap();
        let middle = root.path().join("middle.zip");
        crate::archive::create_zip(&middle, &[inner]).unwrap();
        let path = root.path().join("outer.zip");
        crate::archive::create_zip(&path, &[middle]).unwrap();
        let (view, cx) = crate::tests::component_window(cx, |_, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.current_directory = None;
            manager.archive_path = Some(path.clone());
            manager.archive_name = Some("outer.zip".to_owned());
            manager.entries = crate::archive::list_archive(&path).unwrap();
            manager
        });
        click_inspect_action(cx, "Full audit");
        complete(cx, &view);
        cx.read(|cx| {
            let manager = view.read(cx);
            assert!(!manager.inspector.is_busy());
            assert!(manager.inspector.mode == InspectionMode::FullAudit);
            let report = &manager.inspector.reports[0];
            assert_eq!(report.integrity, Integrity::Passed, "{:?}", report.error);
            assert_eq!(report.json()["nested_audit"]["archives_inspected"], 2);
            assert!(report.findings.iter().any(|finding| finding.kind == "Double extension"
                && finding.entry == "middle.zip!inner.zip!invoice.pdf.exe"));
        });
    }

    #[gpui::test]
    /// Preserves audit findings when the same archive is tested for integrity afterward.
    fn integrity_test_keeps_findings_from_previous_audit(cx: &mut gpui::TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("audit-findings.zip");
        inspection::tests::write_audit_sample(&path);
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let audit =
            inspection::inspect(&path, None, InspectionMode::Audit, &AtomicBool::new(false));
        assert_eq!(audit.integrity, Integrity::Passed);
        let expected_findings = audit.findings.clone();
        assert!(!expected_findings.is_empty());
        let (view, cx) = crate::tests::component_window(cx, |_, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.current_directory = Some(path.parent().unwrap().to_path_buf());
            manager.entries = vec![crate::archive::ArchiveEntry {
                name: name.clone(),
                source_path: None,
                size: std::fs::metadata(&path).unwrap().len(),
                is_directory: false,
                modified: None,
                is_encrypted: false,
            }];
            manager.selected_entry = Some(name.clone());
            manager.inspector.visible = true;
            manager.inspector.mode = InspectionMode::Audit;
            manager.inspector.tab = InspectorTab::Findings;
            manager.inspector.reports = vec![Arc::new(audit)];
            manager
        });

        click_inspect_action(cx, "Audit archive");
        complete(cx, &view);
        cx.read(|cx| {
            assert_eq!(
                view.read(cx).inspector.reports[0].integrity,
                Integrity::Passed
            );
            assert_eq!(
                view.read(cx).inspector.reports[0].findings.len(),
                expected_findings.len()
            );
        });

        click_inspect_action(cx, "Test integrity");
        complete(cx, &view);
        cx.read(|cx| {
            let report = &view.read(cx).inspector.reports[0];
            assert_eq!(report.integrity, Integrity::Passed);
            assert_eq!(report.findings, expected_findings);
        });

        click_inspect_action(cx, "Test integrity");
        complete(cx, &view);
        cx.read(|cx| {
            let report = &view.read(cx).inspector.reports[0];
            assert_eq!(report.integrity, Integrity::Passed);
            assert_eq!(report.findings, expected_findings);
        });
    }

    #[gpui::test]
    /// Keeps the selected archive row visible when the inspector opens at the list bottom.
    fn selected_archive_stays_visible_when_inspector_opens_at_list_bottom(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("notes.txt");
        let archive_path = root.path().join("zz-archive.zip");
        std::fs::write(&source, b"notes").unwrap();
        crate::archive::create_archive(
            &archive_path,
            &[source],
            crate::archive::CreateOptions::default(),
        )
        .unwrap();
        let (view, cx) = crate::tests::component_window(cx, |_, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.current_directory = Some(root.path().to_path_buf());
            manager.entries = (0..200)
                .map(|index| crate::archive::ArchiveEntry {
                    name: format!("file-{index:03}.txt"),
                    source_path: None,
                    size: 1,
                    is_directory: false,
                    modified: None,
                    is_encrypted: false,
                })
                .chain(std::iter::once(crate::archive::ArchiveEntry {
                    name: "zz-archive.zip".into(),
                    source_path: None,
                    size: std::fs::metadata(&archive_path).unwrap().len(),
                    is_directory: false,
                    modified: None,
                    is_encrypted: false,
                }))
                .collect();
            manager
        });
        view.update(cx, |manager, cx| {
            manager
                .file_scroll
                .scroll_to_item(200, gpui::ScrollStrategy::Bottom);
            cx.notify();
        });
        cx.run_until_parked();
        let selected = cx.debug_bounds("file:zz-archive.zip").unwrap();
        let viewport = cx.debug_bounds("file-list").unwrap();
        assert!(selected.origin.y >= viewport.origin.y);
        assert!(selected.bottom() <= viewport.bottom());
        cx.simulate_mouse_move(selected.center(), None, gpui::Modifiers::default());
        cx.simulate_click(selected.center(), gpui::Modifiers::default());
        complete(cx, &view);
        let selected = cx
            .debug_bounds("file:zz-archive.zip")
            .expect("selected archive stays rendered after inspector opens");
        let viewport = cx.debug_bounds("file-list").unwrap();
        assert!(selected.origin.y >= viewport.origin.y);
        assert!(selected.bottom() <= viewport.bottom());
    }

    #[gpui::test]
    /// Covers explicit preview, integrity checks, and multi-archive audit findings.
    fn space_previews_selected_archive_and_integrity_button_checks_it(cx: &mut gpui::TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let one = root.path().join("one.txt");
        let two = root.path().join("two.txt");
        let path = root.path().join("archive.zip");
        std::fs::write(&one, b"duplicate data").unwrap();
        std::fs::write(&two, b"duplicate data").unwrap();
        crate::archive::create_archive(
            &path,
            &[one, two],
            crate::archive::CreateOptions::default(),
        )
        .unwrap();
        let (view, cx) = crate::tests::component_window(cx, |_, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.current_directory = Some(root.path().to_path_buf());
            manager.entries = vec![crate::archive::ArchiveEntry {
                name: "archive.zip".into(),
                source_path: None,
                size: 100,
                is_directory: false,
                modified: None,
                is_encrypted: false,
            }];
            manager
        });
        cx.update(|window, cx| window.click("archive.zip", cx));
        complete(cx, &view);
        assert!(cx.debug_bounds("archive-inspector").is_none());
        cx.read(|cx| assert!(!view.read(cx).inspector.visible));
        cx.simulate_keystrokes("space");
        complete(cx, &view);
        assert!(cx.debug_bounds("archive-inspector").is_some());
        assert!(cx.debug_bounds("toolbox-inspect").is_none());
        assert!(cx.debug_bounds("toolbox-integrity").is_none());
        assert!(
            cx.debug_bounds("toolbar-icon:toolbar-inspect-dropdown")
                .is_some()
        );
        cx.read(|cx| {
            let this = view.read(cx);
            assert!(this.archive_path.is_none());
            assert_eq!(this.selected_entry.as_deref(), Some("archive.zip"));
            assert_eq!(this.inspector.mode, InspectionMode::Preview);
            assert_eq!(this.inspector.reports[0].entries.len(), 2);
            assert_eq!(this.inspector.reports[0].integrity, Integrity::NotTested);
        });
        click_inspect_action(cx, "Test integrity");
        complete(cx, &view);
        assert!(cx.debug_bounds("inspector-health-notice").is_some());
        cx.read(|cx| {
            assert_eq!(
                view.read(cx).inspector.reports[0].integrity,
                Integrity::Passed
            )
        });
        let damaged = root.path().join("damaged.zip");
        std::fs::write(&damaged, b"not an archive").unwrap();
        let duplicate = root.path().join("duplicate.zip");
        std::fs::copy(&path, &duplicate).unwrap();
        let mismatched = root.path().join("mismatched.tar");
        std::fs::copy(&path, &mismatched).unwrap();
        cx.update(|window, cx| {
            view.update(cx, |this, cx| {
                this.inspect_sources(
                    vec![
                        (path.clone(), None),
                        (duplicate, None),
                        (damaged, None),
                        (mismatched, None),
                    ],
                    InspectionMode::Audit,
                    window,
                    cx,
                );
            })
        });
        complete(cx, &view);
        assert!(cx.debug_bounds("inspector-health-notice").is_none());
        cx.read(|cx| {
            let reports = &view.read(cx).inspector.reports;
            assert_eq!(reports.len(), 4);
            assert!(
                reports[0]
                    .findings
                    .iter()
                    .any(|finding| finding.kind == "Duplicate content")
            );
            assert!(
                reports[1]
                    .findings
                    .iter()
                    .any(|finding| finding.kind == "Duplicate archive")
            );
            assert_eq!(reports[2].integrity, Integrity::Failed);
            assert!(
                reports[3]
                    .findings
                    .iter()
                    .any(|finding| finding.kind == "Format mismatch")
            );
        });
        cx.update(|window, cx| window.click("close-inspector", cx));
        assert!(cx.debug_bounds("archive-inspector").is_none());
    }

    #[gpui::test]
    /// Opens the existing password dialog from a password-required inspection notice.
    fn password_required_notice_opens_existing_unlock_modal(cx: &mut gpui::TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("private.txt");
        let path = root.path().join("locked.zip");
        std::fs::write(&source, b"private contents").unwrap();
        crate::archive::create_archive_with_password(
            &path,
            &[source],
            crate::archive::CreateOptions {
                format: crate::archive::ResolvedFormat::Zip,
                ..crate::archive::CreateOptions::default()
            },
            Some("inspector-test-password"),
        )
        .unwrap();
        let (view, cx) = crate::tests::component_window(cx, |_, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.current_directory = Some(root.path().to_path_buf());
            manager.entries = vec![crate::archive::ArchiveEntry {
                name: "locked.zip".into(),
                source_path: None,
                size: std::fs::metadata(&path).unwrap().len(),
                is_directory: false,
                modified: None,
                is_encrypted: false,
            }];
            manager
        });
        cx.update(|window, cx| window.click("locked.zip", cx));
        complete(cx, &view);
        click_inspect_action(cx, "Test integrity");
        complete(cx, &view);
        cx.read(|cx| {
            assert_eq!(
                view.read(cx).inspector.reports[0].integrity,
                Integrity::PasswordRequired
            )
        });
        cx.update(|window, cx| window.click("inspector-unlock-archive", cx));
        cx.read(|cx| {
            let manager = view.read(cx);
            assert!(manager.modal == Some(crate::Modal::Password));
            assert_eq!(
                manager.password_prompt.path.as_deref(),
                Some(path.as_path())
            );
        });
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("unlock-password").is_some());
    }

    #[gpui::test]
    fn security_audit_depth_is_rendered_and_used_by_background_inspections(cx: &mut gpui::TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("invoice.pdf.exe");
        std::fs::write(&source, b"#!/bin/sh\nexit 0\n").unwrap();
        let inner = root.path().join("inner.zip");
        crate::archive::create_zip(&inner, &[source]).unwrap();
        let outer = root.path().join("outer.zip");
        crate::archive::create_zip(&outer, &[inner]).unwrap();
        let (view, cx) = crate::tests::component_window(cx, |window, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.settings_category = crate::SettingsCategory::Advanced;
            manager.show_modal(crate::Modal::Settings, window, cx);
            manager
        });
        assert!(cx.debug_bounds("advanced-setting-audit-depth").is_some());
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        for depth in [0, 1] {
            cx.update(|window, cx| view.update(cx, |manager, cx| {
                manager.advanced_settings.audit_max_depth = depth;
                manager.inspect_sources(vec![(outer.clone(), None)], InspectionMode::Audit, window, cx);
            }));
            complete(cx, &view);
            cx.read(|cx| {
                let manager = view.read(cx);
                assert!(!manager.inspector.is_busy());
                let report = &manager.inspector.reports[0];
                assert_eq!(report.json()["nested_audit"]["max_depth"], depth);
                assert_eq!(report.findings.iter().any(|finding| finding.kind == "Double extension"), depth > 0);
            });
        }
    }

    #[gpui::test]
    /// Prevents a completed worker from reopening a panel that the user closed.
    fn cancelled_inspection_does_not_restore_closed_panel(cx: &mut gpui::TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("damaged.zip");
        std::fs::write(&path, b"damaged").unwrap();
        let (view, cx) = crate::tests::component_window(cx, |_, cx| crate::tests::test_manager(cx));
        cx.update(|window, cx| {
            view.update(cx, |this, cx| {
                this.inspect_sources(vec![(path, None)], InspectionMode::Audit, window, cx);
                this.inspector.close();
                cx.notify();
            })
        });
        complete(cx, &view);
        cx.read(|cx| {
            let this = view.read(cx);
            assert!(!this.inspector.visible);
            assert!(this.inspector.reports.is_empty());
        });
    }
}

/// Renders the inspector panel and its active overview, entries, or findings view.
fn target_tabs(
    manager: &ArchiveManager,
    language: crate::i18n::Language,
    theme: Theme,
    cx: &mut Context<ArchiveManager>,
) -> impl IntoElement {
    let active = manager.inspector.active_session;
    let tabs: Vec<_> = manager
        .inspector
        .sessions
        .iter()
        .enumerate()
        .map(|(index, session)| {
            let icon = session
                .source
                .as_deref()
                .map(|path| {
                    if crate::archive::is_supported_archive(path) {
                        "archive"
                    } else if path.is_dir() {
                        "folder"
                    } else {
                        "file"
                    }
                })
                .unwrap_or("file");
            (
                index,
                session.id,
                manager.inspector.tab_label(index, language),
                icon,
            )
        })
        .collect();
    let mut bar = div()
        .id("inspector-target-tabs")
        .debug_selector(|| "inspector-target-tabs".to_owned())
        .flex()
        .items_center()
        .gap_0()
        .h(px(30.0))
        .flex_shrink_0()
        .border_b_1()
        .border_color(rgb(theme.border))
        .bg(rgb(theme.sidebar));
    let tab_items = tabs.iter().cloned().map(|(index, id, label, icon)| {
        let activate = cx.listener(move |this, _, _, cx| {
            this.inspector.activate(id);
            this.inspector_focus = true;
            cx.notify();
        });
        let selected = index == active;
        let tab_selector = format!("inspector-target-tab:{id}");
        let mut tab = div()
            .id(format!("inspector-target-tab-group-{id}"))
            .debug_selector(move || tab_selector.clone())
            .relative()
            .flex()
            .items_center()
            .gap_1()
            .min_w(px(90.0))
            .max_w(px(180.0))
            .h(px(30.0))
            .flex_shrink_0()
            .px_2()
            .border_r_1()
            .border_color(rgb(theme.border))
            .bg(rgb(if selected { theme.surface } else { theme.titlebar }))
            .text_color(rgb(if selected { theme.text } else { theme.muted_text }))
            .text_xs()
            .cursor_pointer()
            .on_click(activate)
            .when(!selected, |tab| {
                tab.hover(|style| {
                    style
                        .bg(rgb(theme.hover))
                        .border_color(rgb(theme.border))
                        .text_color(rgb(theme.text))
                })
            })
            .child(crate::app_icon(
                icon,
                13.0,
                if selected { theme.accent } else { theme.icon },
            ))
            .child(div().flex_1().min_w(px(0.0)).truncate().child(label));
        if tabs.len() > 1 {
            let close = cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.inspector.close_tab(index);
                cx.notify();
            });
            tab = tab.child(
                Button::new(format!("close-inspector-target-tab-{id}"))
                    .ghost()
                    .xsmall()
                    .w(px(18.0))
                    .h(px(18.0))
                    .flex_shrink_0()
                    .icon(gpui::assets::IconName::X)
                    .tooltip(language.text("Close"))
                    .on_click(close),
            );
        }
        if selected {
            tab = tab.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .h(px(2.0))
                    .bg(rgb(theme.accent)),
            );
        }
        tab.into_any_element()
    });
    let scroll_selector = "inspector-target-tabs-scroll";
    let tab_scroll = div()
        .id("inspector-target-tabs-scroll")
        .debug_selector(|| scroll_selector.to_owned())
        .flex()
        .items_center()
        .gap_0()
        .flex_1()
        .min_w(px(0.0))
        .h(px(30.0))
        .track_scroll(&manager.inspector.target_tabs_scroll)
        .overflow_x_scroll()
        .children(tab_items);
    bar = bar.child(tab_scroll);
    bar
}

/// Renders the inspector panel and its active overview, entries, or findings view.
pub fn view(
    manager: &mut ArchiveManager,
    theme: Theme,
    height: gpui::Pixels,
    cx: &mut Context<ArchiveManager>,
) -> impl IntoElement {
    let language = manager.language;
    if let Some(preview) = manager.inspector.text_preview.clone() {
        preview.update(cx, |preview, cx| {
            if preview.theme.window != theme.window || preview.theme.accent != theme.accent || preview.language != manager.language {
                preview.theme = theme;
                preview.language = manager.language;
                cx.notify();
            }
        });
        return div().id("archive-inspector").debug_selector(|| "archive-inspector".to_owned())
            .flex().flex_col()
            .h(height).min_h(px(0.0)).flex_shrink_0()
            .border_t_1().border_color(rgb(theme.border))
            .child(target_tabs(manager, language, theme, cx))
            .child(preview)
            .on_mouse_down(gpui::MouseButton::Left, cx.listener(|this, _, _, cx| {
                if !this.inspector_focus {
                    this.inspector_focus = true;
                    cx.notify();
                }
            }))
            .into_any_element();
    }
    let manager_view = cx.entity().downgrade();
    let busy = !manager.archive_drop_enabled();
    let entries_view = if manager.inspector.tab == InspectorTab::Entries {
        manager
            .inspector
            .reports
            .get(manager.inspector.active)
            .cloned()
            .map(|report| {
                let selected_name = manager.inspector.selected_name.clone();
                let batch_limited = manager.inspector.batch_limited;
                let password = manager.inspector.passwords.get(&report.path).cloned().flatten()
                    .or_else(|| (manager.archive_path.as_ref() == Some(&report.path)).then(|| manager.archive_password.clone()).flatten());
                let pane = manager.inspector.entries_view.get_or_insert_with(|| {
                    cx.new(|_| {
                        let mut pane = EntryPane {
                            report: report.clone(),
                            selected_index: None,
                            requested_name: None,
                            scroll: gpui::UniformListScrollHandle::new(),
                            language,
                            theme,
                            batch_limited,
                            manager: manager_view.clone(),
                            password: password.clone(),
                            busy,
                        };
                        pane.select_name(selected_name.clone());
                        pane
                    })
                });
                pane.update(cx, |pane, cx| {
                    let new_report = !Arc::ptr_eq(&pane.report, &report);
                    let new_selection = pane.requested_name != selected_name;
                    let changed = new_report
                        || new_selection
                        || pane.language != language
                        || pane.theme.surface != theme.surface
                        || pane.batch_limited != batch_limited;
                    let changed = changed || pane.busy != busy
                        || pane.password.as_ref().map(|password| password.as_str()) != password.as_ref().map(|password| password.as_str());
                    if new_report {
                        pane.report = report;
                        pane.scroll = gpui::UniformListScrollHandle::new();
                    }
                    if new_report || new_selection {
                        pane.select_name(selected_name);
                    }
                    pane.language = language;
                    pane.theme = theme;
                    pane.batch_limited = batch_limited;
                    pane.busy = busy;
                    pane.password = password;
                    if changed {
                        cx.notify();
                    }
                });
                pane.clone()
            })
    } else {
        None
    };
    let findings_view = if manager.inspector.tab == InspectorTab::Findings {
        manager
            .inspector
            .reports
            .get(manager.inspector.active)
            .cloned()
            .map(|report| {
                let pane = manager.inspector.findings_view.get_or_insert_with(|| {
                    cx.new(|_| FindingsPane {
                        report: report.clone(),
                        scroll: gpui::UniformListScrollHandle::new(),
                        language,
                        theme,
                    })
                });
                pane.update(cx, |pane, cx| {
                    let changed = !Arc::ptr_eq(&pane.report, &report)
                        || pane.language != language
                        || pane.theme.surface != theme.surface;
                    if !Arc::ptr_eq(&pane.report, &report) {
                        pane.report = report;
                        pane.scroll = gpui::UniformListScrollHandle::new();
                    }
                    pane.language = language;
                    pane.theme = theme;
                    if changed {
                        cx.notify();
                    }
                });
                pane.clone()
            })
    } else {
        None
    };
    let state = &manager.inspector;
    let mut body = div().id("inspector-content").flex().flex_col().gap_2();
    body = if state.tab == InspectorTab::Findings {
        body.px_3()
    } else {
        body.p_3()
    };
    if state.is_busy() {
        body = match state.tab {
            InspectorTab::Overview => body.child(overview_skeleton(theme)),
            InspectorTab::Entries => body,
            InspectorTab::Findings => body,
        };
    } else if let Some(report) = state.reports.get(state.active) {
        match state.tab {
            InspectorTab::Overview => {
                let expected = report
                    .expected
                    .map(|format| format.label().to_owned())
                    .unwrap_or_else(|| "-".into());
                let created = report.created.clone().unwrap_or_else(|| "-".into());
                let modified = report.modified.clone().unwrap_or_else(|| "-".into());
                let ratio = report
                    .ratio()
                    .map(|ratio| format!("{ratio:.2}:1"))
                    .unwrap_or_else(|| "-".into());
                let compression = language
                    .text(
                        if !report.entries.is_empty()
                            && report
                                .entries
                                .iter()
                                .all(|entry| entry.directory || entry.method == "Stored")
                        {
                            "No compression"
                        } else {
                            "Not recorded by this format"
                        },
                    )
                    .to_owned();
                let hash = report
                    .archive_sha256
                    .clone()
                    .unwrap_or_else(|| language.text("Not tested").into());
                let encrypted_count = report
                    .entries
                    .iter()
                    .filter(|entry| entry.encrypted)
                    .count();
                let password_requirement = match (report.integrity, encrypted_count) {
                    (Integrity::PasswordRequired, _) => "Password required",
                    (_, 0) => "Not required",
                    (Integrity::Passed, _) => "Password verified",
                    _ => "Required to verify",
                };
                let metadata = div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .gap_4()
                            .child(detail(
                                language.text("Detected format"),
                                language.text_data(&report.signature),
                                theme,
                            ))
                            .child(detail(language.text("Expected format"), expected, theme)),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_4()
                            .child(detail(
                                language.text("Archive size"),
                                crate::format_size(report.archive_size),
                                theme,
                            ))
                            .child(detail(
                                language.text("Uncompressed size"),
                                crate::format_size(report.unpacked_size()),
                                theme,
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_4()
                            .child(detail(language.text("Compression ratio"), ratio, theme))
                            .child(detail(
                                language.text("Entry count"),
                                report.entries.len().to_string(),
                                theme,
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_4()
                            .child(detail(language.text("Created"), created, theme))
                            .child(detail(language.text("Last modified"), modified, theme)),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_4()
                            .child(detail(
                                language.text("Compression level"),
                                compression,
                                theme,
                            ))
                            .child(detail("SHA-256", hash, theme)),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_4()
                            .child(detail(
                                language.text("Password requirement"),
                                language.text(password_requirement).to_owned(),
                                theme,
                            ))
                            .child(detail(
                                language.text("Encrypted entries"),
                                encrypted_count.to_string(),
                                theme,
                            )),
                    );
                body = body
                    .child(inspection_notice(report, language, theme, cx))
                    .child(metadata)
                    .child(archive_metadata_overview(report, language, theme));
                if report.detected == Some(crate::archive::ResolvedFormat::Tar) {
                    body = body.child(div().text_xs().text_color(rgb(theme.muted_text)).child(
                        language.text("TAR stores header checksums, not file content checksums"),
                    ));
                }
            }
            InspectorTab::Entries => {}
            InspectorTab::Findings => {}
        }
    }
    let overview_failed = state
        .reports
        .get(state.active)
        .is_some_and(|report| report.integrity == Integrity::Failed);
    let audit_findings_count = state
        .reports
        .get(state.active)
        .map_or(0, |report| audit_findings_count(report));
    let overview_color = if theme.window < 0x808080 {
        0xfca5a5
    } else {
        0xb91c1c
    };
    div().id("archive-inspector").debug_selector(|| "archive-inspector".into()).flex().flex_col()
        .h(height).min_h(px(0.0)).flex_shrink_0().border_t_1().border_color(rgb(theme.border)).bg(rgb(theme.surface))
        .child(target_tabs(manager, language, theme, cx))
        .child(div().flex().flex_wrap().items_center().gap_2().min_h(px(34.0)).flex_shrink_0().px_2().border_b_1().border_color(rgb(theme.border))
            .children([(InspectorTab::Overview, "Overview"), (InspectorTab::Entries, "Entries"), (InspectorTab::Findings, "Audit findings")].into_iter().map(|(tab, label)| {
                let text = if tab == InspectorTab::Findings && audit_findings_count > 0 {
                    format!("{} ({audit_findings_count})", language.text(label))
                } else {
                    language.text(label).to_owned()
                };
                let button = Button::new(label).ghost().xsmall().label(text);
                let button = if (tab == InspectorTab::Overview && overview_failed)
                    || (tab == InspectorTab::Findings && audit_findings_count > 0)
                {
                    button.text_color(rgb(overview_color))
                } else {
                    button
                };
                button.on_click(cx.listener(move |this, _, _, cx| {
                    this.inspector_focus = true;
                    this.inspector.tab = tab;
                    if tab == InspectorTab::Findings {
                        if let Some(pane) = &this.inspector.findings_view {
                            pane.update(cx, |pane, cx| {
                                pane.scroll = gpui::UniformListScrollHandle::new();
                                cx.notify();
                            });
                        }
                    }
                    cx.notify();
                }))
            }))
            .child(div().flex_1())
            .child(Button::new("copy-inspection-report").ghost().xsmall().icon(gpui::assets::IconName::Copy).tooltip(language.text("Copy report"))
                .disabled(state.reports.is_empty()).on_click(cx.listener(|this, _, _, cx| {
                    let data = report_bundle_json(this.inspector.mode, this.inspector.batch_limited, &this.inspector.reports);
                    if let Ok(text) = serde_json::to_string_pretty(&data) { cx.write_to_clipboard(gpui::ClipboardItem::new_string(text)); }
                })))
            .child(Button::new("save-inspection-report").ghost().xsmall().icon(gpui::assets::IconName::Save).tooltip(language.text("Save report"))
                .accessibility_label(language.text("Save report"))
                .disabled(state.reports.is_empty()).on_click(cx.listener(|this, _, _, cx| {
                    this.export_inspection_report(cx);
                })))
            .when(state.reports.len() > 1, |this| this
                .child(Button::new("previous-report").ghost().xsmall().icon(gpui::assets::IconName::ChevronLeft).tooltip(language.text("Previous archive"))
                    .disabled(state.active == 0).on_click(cx.listener(|this, _, _, cx| { this.inspector.active -= 1; this.inspector.selected_name = None; cx.notify(); })))
                .child(div().text_xs().child(format!("{}/{}", state.active + 1, state.reports.len())))
                .child(Button::new("next-report").ghost().xsmall().icon(gpui::assets::IconName::ChevronRight).tooltip(language.text("Next archive"))
                    .disabled(state.active + 1 >= state.reports.len()).on_click(cx.listener(|this, _, _, cx| { this.inspector.active += 1; this.inspector.selected_name = None; cx.notify(); }))))
            .child(Button::new("close-inspector").ghost().xsmall().icon(gpui::assets::IconName::X).tooltip(language.text("Close"))
                .on_click(cx.listener(|this, _, _, cx| { this.inspector.close(); cx.notify(); }))))
        .child(if state.is_busy() && state.tab == InspectorTab::Entries {
            entries_skeleton(language, theme).into_any_element()
        } else if state.is_busy() && state.tab == InspectorTab::Findings {
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(0.0))
                .child(body.flex_shrink_0())
                .child(findings_skeleton(theme))
                .into_any_element()
        } else if let Some(pane) = entries_view {
            pane.into_any_element()
        } else if state.tab == InspectorTab::Findings
            && state
                .reports
                .get(state.active)
                .is_some_and(|report| report.findings.is_empty())
        {
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(0.0))
                .child(body.flex_shrink_0())
                .child(findings_empty_state(
                    language,
                    theme,
                    state.mode.is_audit(),
                ))
                .into_any_element()
        } else if let Some(pane) = findings_view {
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(0.0))
                .overflow_hidden()
                .child(body.flex_shrink_0())
                .child(pane)
                .into_any_element()
        } else {
            div()
                .id("inspector-scroll")
                .flex_1()
                .min_h(px(0.0))
                .overflow_y_scroll()
                .child(body)
                .into_any_element()
            }).into_any_element()
}

fn findings_empty_state(
    language: crate::i18n::Language,
    theme: Theme,
    audit_completed: bool,
) -> impl IntoElement {
    let (title, description) = if audit_completed {
        (
            "No issues found",
            "The audit completed without finding any issues.",
        )
    } else {
        (
            "Audit not run",
            "Choose Inspect > Audit archive to check this archive for issues.",
        )
    };
    div()
        .id("inspector-findings-empty-state")
        .flex()
        .flex_col()
        .flex_1()
        .min_h(px(0.0))
        .items_center()
        .justify_center()
        .gap_2()
        .px_6()
        .child(
            div()
                .text_sm()
                .font_weight(gpui::FontWeight::MEDIUM)
                .child(language.text(title)),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(theme.muted_text))
                .text_center()
                .child(language.text(description)),
        )
}

use super::*;
use crate::{ArchiveManager, Theme};
use gpui::{App, Context as UiContext, IntoElement, Window, div, prelude::*, px, rgb};
use gpui::component::{Disableable, Sizable, button::{Button, ButtonVariants}, checkbox::Checkbox};
use std::sync::Arc;

pub(crate) struct State {
    source: Option<PathBuf>,
    target: usize,
    pub(crate) password: crate::secret::SecretInput,
    analysis: Option<Arc<Analysis>>,
    accepted: bool,
    task: Option<u64>,
    revision: u64,
    error: Option<String>,
    output: Option<PathBuf>,
    cancel: Arc<AtomicBool>,
}

impl State {
    pub(crate) fn new(cx: &mut App) -> Self {
        Self { source: None, target: 0, password: crate::secret::SecretInput::new(cx),
            analysis: None, accepted: false, task: None, revision: 0, error: None, output: None,
            cancel: Arc::new(AtomicBool::new(false)) }
    }
    pub(crate) fn target_index(&self) -> usize { self.target }
    pub(crate) fn set_target(&mut self, index: usize) {
        if self.task.is_none() && index < FORMATS.len() {
            self.target = index;
            self.accepted = false;
            self.error = None;
            self.output = None;
        }
    }
    fn ready(&self) -> bool {
        self.task.is_none() && self.accepted && self.analysis.as_ref().is_some_and(|analysis|
            analysis.assess(FORMATS[self.target]).blockers.is_empty())
    }
}

impl ArchiveManager {
    pub(crate) fn invalidate_conversion_analysis(&mut self) {
        self.conversion.analysis = None;
        self.conversion.accepted = false;
        self.conversion.error = None;
        self.conversion.output = None;
    }

    pub(crate) fn clear_conversion(&mut self, window: &mut Window, cx: &mut UiContext<Self>) {
        self.conversion.revision = self.conversion.revision.wrapping_add(1);
        self.conversion.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(task) = self.conversion.task.take() { crate::task_queue::cancel(self, task, window, cx); }
        self.conversion.analysis = None;
        self.conversion.password.clear();
        self.conversion.accepted = false;
        self.conversion.error = None;
        self.conversion.output = None;
    }

    pub(crate) fn open_conversion(&mut self, window: &mut Window, cx: &mut UiContext<Self>) {
        let target = self.inspection_target();
        self.clear_conversion(window, cx);
        self.conversion.source = target.as_ref().map(|(path, _)| path.clone());
        if let Some(password) = target.and_then(|(_, password)| password) {
            self.conversion.password.text.push_str(password.as_str());
        }
        self.show_modal(crate::Modal::Conversion, window, cx);
    }

    fn choose_conversion_source(&mut self, window: &mut Window, cx: &mut UiContext<Self>) {
        if self.conversion.task.is_some() { return; }
        if let Some(path) = rfd::FileDialog::new().set_title(self.language.text("Select archive to convert")).pick_file() {
            self.clear_conversion(window, cx);
            self.conversion.source = Some(path);
            cx.notify();
        }
    }

    pub(crate) fn analyze_conversion(&mut self, cx: &mut UiContext<Self>) {
        if self.conversion.task.is_some() { return; }
        let Some(source) = self.conversion.source.clone() else { return; };
        self.conversion.analysis = None;
        self.conversion.accepted = false;
        self.conversion.error = None;
        self.conversion.revision = self.conversion.revision.wrapping_add(1);
        self.conversion.output = None;
        let revision = self.conversion.revision;
        let password = (!self.conversion.password.text.is_empty()).then(|| self.conversion.password.text.clone());
        let cancel = Arc::new(AtomicBool::new(false));
        self.conversion.cancel = cancel.clone();
        let resources = vec![source.clone(), source.parent().unwrap_or_else(|| Path::new(".")).to_owned()];
        let task = self.background_queue.enqueue("Analyzing conversion...",
            crate::task_queue::TaskControl::with_cancel(cancel.clone()).for_paths(resources),
            move |control, reporter| async move {
                reporter.phase("Analyzing conversion...");
                if !control.checkpoint() { anyhow::bail!("Task cancelled"); }
                let analysis = analyze(&source, password.as_ref().map(|password| password.as_str()), &cancel)?;
                Ok(Arc::new(analysis))
            },
            move |manager, result, _, cx| {
                if manager.conversion.revision != revision { return; }
                manager.conversion.task = None;
                match result {
                    Ok(analysis) => manager.conversion.analysis = Some(analysis),
                    Err(crate::task_queue::TaskFailure::Failed(error)) => manager.conversion.error = Some(format!("{error:#}")),
                    Err(crate::task_queue::TaskFailure::Cancelled) => {}
                }
                cx.notify();
            }, cx);
        self.conversion.task = Some(task);
        cx.notify();
    }

    pub(crate) fn submit_conversion(&mut self, cx: &mut UiContext<Self>) {
        if !self.conversion.ready() { return; }
        let format = FORMATS[self.conversion.target];
        let Some(destination) = rfd::FileDialog::new().set_title(self.language.text("Save converted archive"))
            .set_file_name(format!("converted.{}", format.extension())).add_filter(format.label(), &[format.extension()]).save_file()
        else { return; };
        self.start_conversion(destination, cx);
    }

    fn start_conversion(&mut self, destination: PathBuf, cx: &mut UiContext<Self>) {
        if !self.conversion.ready() { return; }
        let Some(analysis) = self.conversion.analysis.clone() else { return; };
        let format = FORMATS[self.conversion.target];
        let password = (!self.conversion.password.text.is_empty()).then(|| self.conversion.password.text.clone());
        let revision = self.conversion.revision;
        let cancel = Arc::new(AtomicBool::new(false));
        self.conversion.cancel = cancel.clone();
        self.conversion.error = None;
        self.conversion.output = None;
        let resources = vec![analysis.source.clone(), destination.clone(), analysis.source.parent().unwrap_or_else(|| Path::new(".")).to_owned()];
        let task = self.background_queue.enqueue("Converting archive...",
            crate::task_queue::TaskControl::with_cancel(cancel.clone()).for_paths(resources),
            move |control, reporter| async move {
                reporter.phase("Converting archive...");
                if !control.checkpoint() { anyhow::bail!("Task cancelled"); }
                convert(&analysis, format, password.as_ref().map(|password| password.as_str()), &destination, &cancel)?;
                Ok(destination)
            },
            move |manager, result, _, cx| {
                if manager.conversion.revision != revision { return; }
                manager.conversion.task = None;
                match result {
                    Ok(path) => {
                        manager.status_message = Some(format!("{} {}", manager.language.text("Converted archive saved"), path.display()));
                        manager.conversion.output = Some(path);
                        manager.conversion.accepted = false;
                    }
                    Err(crate::task_queue::TaskFailure::Failed(error)) => manager.conversion.error = Some(format!("{error:#}")),
                    Err(crate::task_queue::TaskFailure::Cancelled) => {}
                }
                cx.notify();
            }, cx);
        self.conversion.task = Some(task);
        cx.notify();
    }
}

pub(crate) fn view(manager: &ArchiveManager, theme: Theme, cx: &mut UiContext<ArchiveManager>) -> impl IntoElement {
    let language = manager.language;
    let state = &manager.conversion;
    let busy = state.task.is_some();
    div().id("conversion-content").debug_selector(|| "conversion-content".into())
        .flex().flex_col().gap_3().w_full().min_w(px(0.0)).whitespace_normal()
        .child(div().flex().flex_wrap().items_center().gap_2()
            .child(div().flex_1().min_w(px(0.0)).text_sm().truncate().child(state.source.as_ref()
                .map_or_else(|| language.text("No archive selected").to_owned(), |path| path.display().to_string())))
            .child(Button::new("choose-conversion-source").ghost().small()
                .icon(gpui::assets::IconName::FolderOpen).label(language.text("Choose archive..."))
                .disabled(busy).on_click(cx.listener(|manager, _, window, cx| manager.choose_conversion_source(window, cx)))))
        .child(language.text("Target format"))
        .child(crate::ui::select(manager, crate::ui::Selector::ConversionFormat))
        .child(language.text("Source password"))
        .child(crate::secret::field("conversion-password", &state.password, crate::secret::SecretField::Conversion,
            !busy, language, cx))
        .child(div().text_xs().text_color(rgb(theme.muted_text)).child(language.text("The source stays unchanged; conversion creates a new archive")))
        .when_some(state.analysis.as_ref(), |this, analysis| {
            let assessment = analysis.assess(FORMATS[state.target]);
            this.child(div().text_sm().child(format!("{} → {} · {} {}", analysis.source_format().label(),
                FORMATS[state.target].label(), analysis.entry_count(), language.text("Entries"))))
                .child(div().id("conversion-warnings").debug_selector(|| "conversion-warnings".into())
                    .flex().flex_col().gap_2().max_h(px(120.0)).overflow_y_scroll()
                    .children(assessment.blockers.into_iter().map(|blocker| div().text_sm()
                        .text_color(rgb(if theme.window < 0x808080 { 0xfca5a5 } else { 0xb91c1c })).child(language.text(blocker))))
                    .children(assessment.warnings.into_iter().map(|(warning, count)| div().text_xs()
                        .child(count.map_or_else(|| language.text(warning).to_owned(), |count| format!("{} ({count})", language.text(warning)))))))
                .child(Checkbox::new("accept-conversion-losses").debug_selector(|| "accept-conversion-losses".into())
                    .flex_shrink_0()
                    .label(language.text("I accept the listed metadata losses and conversion consequences"))
                    .checked(state.accepted).disabled(busy)
                    .on_change(cx.listener(|manager, checked, _, cx| { manager.conversion.accepted = *checked; cx.notify(); })))
        })
        .when_some(state.output.as_ref(), |this, path| this.child(div().text_sm().truncate()
            .child(format!("{}: {}", language.text("Converted archive saved"), path.display()))))
        .when_some(state.error.as_ref(), |this, error| this.child(div().text_xs().child(language.error_message(error))))
}

pub(crate) fn footer(manager: &ArchiveManager, cx: &mut UiContext<ArchiveManager>) -> impl IntoElement {
    let language = manager.language;
    let state = &manager.conversion;
    div().id("conversion-actions").debug_selector(|| "conversion-actions".into())
        .flex().flex_wrap().justify_end().gap_2()
        .child(Button::new("close-conversion").ghost().small().label(language.text("Close"))
            .on_click(cx.listener(|manager, _, window, cx| manager.close_modal(window, cx))))
        .child(Button::new("analyze-conversion").debug_selector(|| "analyze-conversion".into())
            .ghost().small().icon(gpui::assets::IconName::FileSearch).label(language.text("Analyze conversion"))
            .disabled(state.task.is_some() || state.source.is_none())
            .on_click(cx.listener(|manager, _, _, cx| manager.analyze_conversion(cx))))
        .child(Button::new("convert-archive").debug_selector(|| "convert-archive".into())
            .primary().small().icon(gpui::assets::IconName::ArrowRightLeft).label(language.text("Convert archive"))
            .disabled(!state.ready()).on_click(cx.listener(|manager, _, _, cx| manager.submit_conversion(cx))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn complete(cx: &mut gpui::VisualTestContext, view: &gpui::Entity<ArchiveManager>) {
        for _ in 0..8 {
            cx.run_until_parked();
            cx.update(|window, cx| window.simulate_next_frame(cx));
            cx.run_until_parked();
            if cx.read(|cx| view.read(cx).conversion.task.is_none()) { break; }
        }
    }

    #[gpui::test]
    fn conversion_requires_acknowledgment_runs_in_background_and_clears_secrets(cx: &mut gpui::TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.zip");
        let file = root.path().join("file.txt");
        std::fs::write(&file, b"payload").unwrap();
        archive::create_archive(&source, &[file], Default::default()).unwrap();
        let (view, cx) = crate::tests::component_window(cx, |window, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.open_conversion(window, cx);
            manager.conversion.source = Some(source.clone());
            manager
        });
        view.update(cx, |manager, cx| manager.analyze_conversion(cx));
        complete(cx, &view);
        cx.read(|cx| {
            let state = &view.read(cx).conversion;
            assert!(state.analysis.is_some(), "{:?}", state.error);
            assert!(!state.ready());
        });
        let output = root.path().join("converted.tar");
        view.update(cx, |manager, cx| {
            manager.start_conversion(output.clone(), cx);
            assert!(manager.conversion.task.is_none());
            manager.conversion.accepted = true;
            manager.conversion.set_target(FORMATS.iter().position(|format| *format == ResolvedFormat::Tar).unwrap());
            assert!(!manager.conversion.ready());
            cx.notify();
        });
        complete(cx, &view);
        let accept = cx.debug_bounds("accept-conversion-losses").unwrap();
        cx.simulate_mouse_move(accept.center(), None, gpui::Modifiers::default());
        cx.simulate_click(accept.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        cx.read(|cx| assert!(view.read(cx).conversion.ready()));
        view.update(cx, |manager, cx| manager.start_conversion(output.clone(), cx));
        complete(cx, &view);
        cx.read(|cx| {
            let state = &view.read(cx).conversion;
            assert!(state.error.is_none(), "{:?}", state.error);
            assert_eq!(state.output.as_ref(), Some(&output));
            assert!(!state.accepted);
        });
        assert_eq!(archive::list_archive(&output).unwrap()[0].name, "file.txt");
        view.update(cx, |manager, _| {
            manager.conversion.accepted = true;
            manager.invalidate_conversion_analysis();
            assert!(!manager.conversion.ready());
            assert!(manager.conversion.analysis.is_none());
        });
        cx.update(|window, cx| view.update(cx, |manager, cx| {
            manager.conversion.password.text.push_str("secret");
            manager.close_modal(window, cx);
            assert!(manager.conversion.password.text.is_empty());
            assert!(manager.conversion.analysis.is_none());
            assert!(manager.conversion.cancel.load(std::sync::atomic::Ordering::Relaxed));
        }));
    }

    #[gpui::test]
    fn conversion_warnings_and_actions_fit_supported_locales_and_viewports(cx: &mut gpui::TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.zip");
        let file = root.path().join("file.txt");
        std::fs::write(&file, b"payload").unwrap();
        archive::create_archive(&source, &[file], Default::default()).unwrap();
        let plan = Arc::new(analyze(&source, None, &AtomicBool::new(false)).unwrap());
        let (view, cx) = crate::tests::component_window(cx, |window, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.open_conversion(window, cx);
            manager.conversion.source = Some(source.clone());
            manager.conversion.analysis = Some(plan);
            manager
        });
        for language in crate::i18n::Language::ALL {
            view.update(cx, |manager, cx| { manager.language = language; cx.notify(); });
            for viewport in [gpui::size(px(1100.0), px(900.0)), gpui::size(px(560.0), px(700.0))] {
                cx.simulate_resize(viewport);
                complete(cx, &view);
                let panel = cx.debug_bounds("dialog-0").unwrap();
                let actions = cx.debug_bounds("conversion-actions").unwrap();
                let warnings = cx.debug_bounds("conversion-warnings").unwrap();
                let accept = cx.debug_bounds("accept-conversion-losses").unwrap();
                assert!(panel.bottom() <= viewport.height, "{language:?}");
                assert!(panel.right() <= viewport.width, "{language:?}");
                assert!(actions.bottom() <= panel.bottom(), "{language:?}");
                assert!(warnings.bottom() <= accept.origin.y, "{language:?}");
                assert!(accept.bottom() <= actions.origin.y, "{language:?}: confirmation {accept:?}, actions {actions:?}");
            }
        }
    }
}
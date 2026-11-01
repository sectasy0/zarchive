use crate::{ArchiveManager, Theme};
use gpui::component::{
    Sizable,
    button::{Button, ButtonVariants},
    popover::Popover,
};
use gpui::{Context, IntoElement, SharedString, Window, div, prelude::*, px, rgb};
use std::{
    any::Any,
    cell::RefCell,
    collections::VecDeque,
    future::Future,
    io::{self, Read, Write},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

const MAX_WORKERS: usize = 3;
type JobValue = Box<dyn Any + Send>;
type JobResult = Result<JobValue, TaskFailure>;
type JobFuture = Pin<Box<dyn Future<Output = anyhow::Result<JobValue>> + Send>>;
type JobWorker = Box<dyn FnOnce(TaskControl, TaskReporter) -> JobFuture + Send>;
type JobCompletion =
    Box<dyn FnOnce(&mut ArchiveManager, JobResult, &mut Window, &mut Context<ArchiveManager>)>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TaskStatus {
    Queued,
    Running,
    Paused,
    Cancelling,
    Completed,
    Failed,
    Cancelled,
}

impl TaskStatus {
    fn is_active(self) -> bool {
        matches!(
            self,
            Self::Queued | Self::Running | Self::Paused | Self::Cancelling
        )
    }
}

#[derive(Debug)]
pub(crate) enum TaskFailure {
    Failed(anyhow::Error),
    Cancelled,
}

#[derive(Default)]
struct Progress {
    phase: String,
    current: u64,
    total: Option<u64>,
    bytes: u64,
    byte_total: Option<u64>,
}

thread_local! {
    static CURRENT_TASK: RefCell<Option<(TaskControl, TaskReporter)>> = const { RefCell::new(None) };
}

struct TaskScope(Option<(TaskControl, TaskReporter)>);

impl Drop for TaskScope {
    fn drop(&mut self) {
        CURRENT_TASK.with_borrow_mut(|current| *current = self.0.take());
    }
}

fn scoped<T>(control: TaskControl, reporter: TaskReporter, work: impl FnOnce() -> T) -> T {
    let previous = CURRENT_TASK.with_borrow_mut(|current| current.replace((control, reporter)));
    let _scope = TaskScope(previous);
    work()
}

pub(crate) fn checkpoint() -> io::Result<()> {
    let control =
        CURRENT_TASK.with_borrow(|current| current.as_ref().map(|(control, _)| control.clone()));
    if control.is_none_or(|control| control.checkpoint()) {
        Ok(())
    } else {
        Err(io::Error::other("Task cancelled"))
    }
}

pub(crate) fn report_bytes(amount: u64) {
    CURRENT_TASK.with_borrow(|current| {
        if let Some((_, reporter)) = current {
            let mut progress = reporter
                .progress
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            progress.bytes = progress.bytes.saturating_add(amount);
            if let Some(total) = progress.byte_total {
                progress.bytes = progress.bytes.min(total);
            }
        }
    });
}

pub(crate) fn report_byte_total(total: u64) {
    CURRENT_TASK.with_borrow(|context| {
        if let Some((_, reporter)) = context {
            let mut progress = reporter
                .progress
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            progress.bytes = 0;
            progress.byte_total = Some(total.max(1));
        }
    });
}

pub(crate) fn report_progress(current: u64, total: u64) {
    CURRENT_TASK.with_borrow(|context| {
        if let Some((_, reporter)) = context {
            reporter.set(current, total);
        }
    });
}

pub(crate) struct TaskReader<Reader>(pub Reader);

impl<Reader: Read> Read for TaskReader<Reader> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        checkpoint()?;
        let count = self.0.read(buffer)?;
        report_bytes(count as u64);
        Ok(count)
    }
}

pub(crate) struct TaskWriter<Writer>(pub Writer);

impl<Writer: Write> Write for TaskWriter<Writer> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        checkpoint()?;
        let count = self.0.write(buffer)?;
        report_bytes(count as u64);
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        checkpoint()?;
        self.0.flush()
    }
}

pub(crate) fn copy(
    reader: &mut (impl Read + ?Sized),
    writer: &mut (impl Write + ?Sized),
) -> io::Result<u64> {
    if CURRENT_TASK.with_borrow(Option::is_none) {
        return io::copy(reader, writer);
    }
    let mut buffer = [0; 64 * 1024];
    let mut total = 0;
    loop {
        checkpoint()?;
        let count = match reader.read(&mut buffer) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if count == 0 {
            return Ok(total);
        }
        writer.write_all(&buffer[..count])?;
        total += count as u64;
        report_bytes(count as u64);
    }
}

#[derive(Clone, Default)]
pub(crate) struct TaskControl {
    cancel: Arc<AtomicBool>,
    pause: Arc<AtomicBool>,
    resources: Arc<Vec<std::path::PathBuf>>,
    waker: Arc<std::sync::Mutex<Option<std::task::Waker>>>,
}

impl TaskControl {
    pub(crate) fn with_cancel(cancel: Arc<AtomicBool>) -> Self {
        Self {
            cancel,
            ..Self::default()
        }
    }

    pub(crate) fn for_paths(mut self, paths: impl IntoIterator<Item = std::path::PathBuf>) -> Self {
        self.resources = Arc::new(paths.into_iter().collect());
        self
    }

    pub(crate) fn checkpoint(&self) -> bool {
        while self.pause.load(Ordering::Acquire) && !self.cancel.load(Ordering::Acquire) {
            std::thread::park_timeout(Duration::from_millis(25));
        }
        !self.cancel.load(Ordering::Acquire)
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }

    pub(crate) fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
        self.pause.store(false, Ordering::Release);
        self.wake();
    }

    fn wake(&self) {
        let waker = self
            .waker
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct TaskReporter {
    progress: Arc<std::sync::Mutex<Progress>>,
}

impl TaskReporter {
    pub(crate) fn phase(&self, phase: impl Into<String>) {
        self.progress
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .phase = phase.into();
    }
    pub(crate) fn set(&self, current: u64, total: u64) {
        let mut progress = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        progress.current = current.min(total);
        progress.total = Some(total.max(1));
    }
    pub(crate) fn advance(&self, amount: u64) {
        let mut progress = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        progress.current = progress.current.saturating_add(amount);
        if let Some(total) = progress.total {
            progress.current = progress.current.min(total);
        }
    }
}

struct QueueTask {
    id: u64,
    label: &'static str,
    status: TaskStatus,
    control: TaskControl,
    reporter: TaskReporter,
    worker: Option<JobWorker>,
    completion: Option<JobCompletion>,
    error: Option<String>,
}

#[derive(Default)]
pub(crate) struct BackgroundQueue {
    tasks: VecDeque<QueueTask>,
    next_id: u64,
    running: usize,
    pub visible: bool,
    ticker_active: bool,
}

impl BackgroundQueue {
    pub(crate) fn has_running(&self) -> bool {
        self.tasks
            .iter()
            .any(|task| matches!(task.status, TaskStatus::Running | TaskStatus::Cancelling))
    }
    fn task_view(
        &self,
        language: crate::i18n::Language,
        theme: Theme,
        cx: &mut Context<ArchiveManager>,
    ) -> impl IntoElement {
        div()
            .id("background-task-list")
            .flex()
            .flex_col()
            .min_h(px(0.0))
            .flex_1()
            .w_full()
            .max_h(px(420.0))
            .overflow_y_scroll()
            .gap_1()
            .when(self.tasks.is_empty(), |list| {
                list.child(
                    div()
                        .py_3()
                        .text_sm()
                        .text_color(rgb(theme.muted_text))
                        .child(language.text("No background tasks")),
                )
            })
            .children(self.tasks.iter().rev().map(|task| {
                let id = task.id;
                let active = task.status.is_active();
                let controllable = matches!(
                    task.status,
                    TaskStatus::Queued | TaskStatus::Running | TaskStatus::Paused
                );
                let progress = task
                    .reporter
                    .progress
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let percent = progress
                    .byte_total
                    .map(|total| progress.bytes as f32 / total as f32)
                    .or_else(|| {
                        progress
                            .total
                            .map(|total| progress.current as f32 / total as f32)
                    })
                    .map(|percent| percent.clamp(0.0, 1.0));
                let phase = if progress.phase.is_empty() {
                    task.control
                        .resources
                        .first()
                        .map(|path| {
                            path.file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .into_owned()
                        })
                        .unwrap_or_default()
                } else {
                    language.text_data(&progress.phase)
                };
                let current = progress.current;
                let total = progress.total.filter(|_| progress.byte_total.is_none());
                let bytes = progress.bytes;
                let byte_total = progress.byte_total;
                let status = match task.status {
                    TaskStatus::Queued => language.text("Queued"),
                    TaskStatus::Running => language.text("Running"),
                    TaskStatus::Paused => language.text("Paused"),
                    TaskStatus::Cancelling => language.text("Cancelling..."),
                    TaskStatus::Completed => language.text("Completed"),
                    TaskStatus::Failed => language.text("Failed"),
                    TaskStatus::Cancelled => language.text("Cancelled"),
                };
                let mut row = div()
                    .id(SharedString::from(format!("background-task-{id}")))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_2()
                    .py_2()
                    .border_b_1()
                    .border_color(rgb(theme.border))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .min_w(px(0.0))
                                    .flex_1()
                                    .child(
                                        div().text_sm().truncate().child(language.text(task.label)),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_1()
                                            .when(task.status == TaskStatus::Completed, |status| {
                                                status.child(
                                                    gpui::component::Icon::new(
                                                        gpui::assets::IconName::Check,
                                                    )
                                                    .size(px(12.0))
                                                    .text_color(rgb(theme.accent)),
                                                )
                                            })
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(rgb(theme.muted_text))
                                                    .truncate()
                                                    .child(if phase.is_empty() {
                                                        status.to_owned()
                                                    } else {
                                                        format!("{status} - {phase}")
                                                    }),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .when(
                                        controllable && task.status != TaskStatus::Paused,
                                        |actions| {
                                            actions.child(
                                                Button::new(SharedString::from(format!(
                                                    "background-task-pause-{id}"
                                                )))
                                                .ghost()
                                                .xsmall()
                                                .icon(gpui::assets::IconName::Pause)
                                                .tooltip(language.text("Pause task"))
                                                .accessibility_label(language.text("Pause task"))
                                                .on_click(cx.listener(move |manager, _, _, cx| {
                                                    manager.background_queue.pause(id);
                                                    cx.notify();
                                                })),
                                            )
                                        },
                                    )
                                    .when(task.status == TaskStatus::Paused, |actions| {
                                        actions.child(
                                            Button::new(SharedString::from(format!(
                                                "background-task-resume-{id}"
                                            )))
                                            .ghost()
                                            .xsmall()
                                            .icon(gpui::assets::IconName::Play)
                                            .tooltip(language.text("Resume task"))
                                            .accessibility_label(language.text("Resume task"))
                                            .on_click(cx.listener(
                                                move |manager, _, window, cx| {
                                                    manager.background_queue.resume(id, window, cx);
                                                },
                                            )),
                                        )
                                    })
                                    .when(controllable, |actions| {
                                        actions.child(
                                            Button::new(SharedString::from(format!(
                                                "background-task-cancel-{id}"
                                            )))
                                            .ghost()
                                            .xsmall()
                                            .icon(gpui::assets::IconName::X)
                                            .tooltip(language.text("Cancel task"))
                                            .accessibility_label(language.text("Cancel task"))
                                            .on_click(cx.listener(
                                                move |manager, _, window, cx| {
                                                    cancel(manager, id, window, cx);
                                                },
                                            )),
                                        )
                                    }),
                            ),
                    )
                    .when_some(percent, |row, percent| {
                        row.child(
                            div().h(px(3.0)).w_full().bg(rgb(theme.border)).child(
                                div()
                                    .h(px(3.0))
                                    .w(gpui::relative(percent))
                                    .bg(rgb(theme.accent)),
                            ),
                        )
                    })
                    .when_some(total, |row, total| {
                        row.child(
                            div()
                                .text_xs()
                                .text_color(rgb(theme.muted_text))
                                .child(format!("{current} / {total}")),
                        )
                    })
                    .when(bytes > 0 || byte_total.is_some(), |row| {
                        row.child(div().text_xs().text_color(rgb(theme.muted_text)).child(
                            byte_total.map_or_else(
                                || crate::format_size(bytes),
                                |total| {
                                    format!(
                                        "{} / {}",
                                        crate::format_size(bytes),
                                        crate::format_size(total)
                                    )
                                },
                            ),
                        ))
                    })
                    .when_some(task.error.as_ref(), |row, error| {
                        row.child(
                            div()
                                .text_xs()
                                .text_color(rgb(0xef4444))
                                .child(language.error_message(error)),
                        )
                    });
                if !active {
                    row = row.text_color(rgb(theme.muted_text));
                }
                row
            }))
    }

    pub(crate) fn enqueue<T, W, F, C>(
        &mut self,
        label: &'static str,
        control: TaskControl,
        work: W,
        complete: C,
        cx: &mut Context<ArchiveManager>,
    ) -> u64
    where
        T: Any + Send + 'static,
        W: FnOnce(TaskControl, TaskReporter) -> F + Send + 'static,
        F: Future<Output = anyhow::Result<T>> + Send + 'static,
        C: FnOnce(
                &mut ArchiveManager,
                Result<T, TaskFailure>,
                &mut Window,
                &mut Context<ArchiveManager>,
            ) + 'static,
    {
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let id = self.next_id;
        let reporter = TaskReporter::default();
        let worker: JobWorker = Box::new(move |control, reporter| {
            Box::pin(async move {
                let scope_control = control.clone();
                let scope_reporter = reporter.clone();
                let mut future = Box::pin(work(control, reporter));
                std::future::poll_fn(move |task_cx| {
                    *scope_control
                        .waker
                        .lock()
                        .unwrap_or_else(|error| error.into_inner()) = Some(task_cx.waker().clone());
                    if scope_control.is_cancelled() {
                        return std::task::Poll::Ready(Err(anyhow::anyhow!("Task cancelled")));
                    }
                    if scope_control.pause.load(Ordering::Acquire) {
                        return std::task::Poll::Pending;
                    }
                    scoped(scope_control.clone(), scope_reporter.clone(), || {
                        future.as_mut().poll(task_cx)
                    })
                })
                .await
                .map(|value| Box::new(value) as JobValue)
            })
        });
        let completion: JobCompletion = Box::new(move |manager, result, window, cx| match result {
            Ok(value) => complete(
                manager,
                Ok(*value
                    .downcast::<T>()
                    .expect("queued job result type matches")),
                window,
                cx,
            ),
            Err(error) => complete(manager, Err(error), window, cx),
        });
        self.tasks.push_back(QueueTask {
            id,
            label,
            status: TaskStatus::Queued,
            control,
            reporter,
            worker: Some(worker),
            completion: Some(completion),
            error: None,
        });
        self.trim_history();
        cx.notify();
        id
    }

    fn trim_history(&mut self) {
        while self.tasks.len() > 40 {
            let Some(index) = self.tasks.iter().position(|task| !task.status.is_active()) else {
                break;
            };
            self.tasks.remove(index);
        }
    }

    pub(crate) fn pause(&mut self, id: u64) {
        if let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) {
            match task.status {
                TaskStatus::Queued => task.status = TaskStatus::Paused,
                TaskStatus::Running => {
                    task.control.pause.store(true, Ordering::Release);
                    task.status = TaskStatus::Paused;
                }
                _ => {}
            }
        }
    }

    pub(crate) fn resume(&mut self, id: u64, window: &Window, cx: &mut Context<ArchiveManager>) {
        if let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) {
            if task.status == TaskStatus::Paused {
                task.control.pause.store(false, Ordering::Release);
                task.control.wake();
                task.status = if task.worker.is_some() {
                    TaskStatus::Queued
                } else {
                    TaskStatus::Running
                };
            }
        }
        pump(self, window, cx);
        cx.notify();
    }

    fn cancel(&mut self, id: u64) -> Option<JobCompletion> {
        let completion = if let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) {
            if !task.status.is_active() {
                return None;
            }
            task.control.cancel();
            if matches!(task.status, TaskStatus::Queued | TaskStatus::Paused)
                && task.worker.is_some()
            {
                task.status = TaskStatus::Cancelled;
                task.worker = None;
                task.completion.take()
            } else {
                task.status = TaskStatus::Cancelling;
                None
            }
        } else {
            None
        };
        completion
    }

    fn finish(
        &mut self,
        id: u64,
        result: anyhow::Result<JobValue>,
    ) -> (Option<JobCompletion>, JobResult) {
        self.running = self.running.saturating_sub(1);
        let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) else {
            return (None, Err(TaskFailure::Cancelled));
        };
        let failure = if task.control.is_cancelled() && result.is_err() {
            task.status = TaskStatus::Cancelled;
            Some(TaskFailure::Cancelled)
        } else {
            match result {
                Ok(value) => {
                    task.status = TaskStatus::Completed;
                    return (task.completion.take(), Ok(value));
                }
                Err(error) => {
                    task.status = TaskStatus::Failed;
                    task.error = Some(format!("{error:#}"));
                    Some(TaskFailure::Failed(error))
                }
            }
        };
        (task.completion.take(), Err(failure.unwrap()))
    }
}

impl Drop for BackgroundQueue {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.control.cancel();
        }
    }
}

pub(crate) fn prepare(
    manager: &mut ArchiveManager,
    window: &mut Window,
    cx: &mut Context<ArchiveManager>,
) {
    let mut completions = Vec::new();
    for task in &mut manager.background_queue.tasks {
        if !task.status.is_active() || !task.control.is_cancelled() {
            continue;
        }
        task.control.cancel();
        if task.worker.take().is_some() {
            task.status = TaskStatus::Cancelled;
            if let Some(completion) = task.completion.take() {
                completions.push(completion);
            }
        } else {
            task.status = TaskStatus::Cancelling;
        }
    }
    for completion in completions {
        completion(manager, Err(TaskFailure::Cancelled), window, cx);
    }
    pump(&mut manager.background_queue, window, cx);
}

fn pump(queue: &mut BackgroundQueue, window: &Window, cx: &mut Context<ArchiveManager>) {
    while queue.running < MAX_WORKERS {
        let Some(index) = queue.tasks.iter().position(|task| {
            task.status == TaskStatus::Queued
                && task.worker.is_some()
                && !queue.tasks.iter().any(|running| {
                    running.worker.is_none()
                        && running.status.is_active()
                        && running.control.resources.iter().any(|path| {
                            task.control.resources.iter().any(|candidate| {
                                candidate.starts_with(path) || path.starts_with(candidate)
                            })
                        })
                })
        }) else {
            break;
        };
        let task = &mut queue.tasks[index];
        let (id, worker, control, reporter) = (
            task.id,
            task.worker.take().unwrap(),
            task.control.clone(),
            task.reporter.clone(),
        );
        task.status = TaskStatus::Running;
        queue.running += 1;
        let worker_task = cx.background_executor().spawn(worker(control, reporter));
        let weak = cx.entity().downgrade();
        let executor = cx.background_executor().clone();
        window
            .spawn(cx, async move |async_cx| {
                let mut result = Some(worker_task.await);
                loop {
                    let delivered = async_cx.update(|window, cx| {
                        let _ = weak.update(cx, |manager, cx| {
                            let (completion, result) = manager.background_queue.finish(
                                id,
                                result.take().unwrap_or_else(|| {
                                    Err(anyhow::anyhow!("task result was already consumed"))
                                }),
                            );
                            if let Some(completion) = completion {
                                completion(manager, result, window, cx);
                            }
                            pump(&mut manager.background_queue, window, cx);
                            cx.notify();
                        });
                    });
                    match delivered {
                        Ok(()) => break,
                        Err(error) if error.is::<std::cell::BorrowMutError>() => {
                            executor.timer(Duration::from_millis(16)).await
                        }
                        Err(_) => break,
                    }
                }
            })
            .detach();
    }
    if queue.has_running() && !queue.ticker_active {
        let weak = cx.entity().downgrade();
        let executor = cx.background_executor().clone();
        queue.ticker_active = true;
        window
            .spawn(cx, async move |async_cx| {
                loop {
                    executor.timer(Duration::from_millis(100)).await;
                    let keep_running = async_cx.update(|_, cx| {
                        weak.update(cx, |manager, cx| {
                            let running = manager.background_queue.has_running();
                            cx.notify();
                            if !running {
                                manager.background_queue.ticker_active = false;
                            }
                            running
                        })
                    });
                    match keep_running {
                        Ok(Ok(true)) => {}
                        Err(error) if error.is::<std::cell::BorrowMutError>() => continue,
                        _ => break,
                    }
                }
            })
            .detach();
    }
}

pub(crate) fn cancel(
    manager: &mut ArchiveManager,
    id: u64,
    window: &mut Window,
    cx: &mut Context<ArchiveManager>,
) {
    let completion = manager.background_queue.cancel(id);
    if let Some(completion) = completion {
        completion(manager, Err(TaskFailure::Cancelled), window, cx);
    }
    pump(&mut manager.background_queue, window, cx);
    cx.notify();
}

pub(crate) fn toggle(
    manager: &ArchiveManager,
    theme: Theme,
    cx: &mut Context<ArchiveManager>,
) -> impl IntoElement {
    let view = cx.entity();
    let language = manager.language;
    Popover::new("background-task-queue-popover")
        .anchor(gpui::Anchor::TopRight)
        .open(manager.background_queue.visible)
        .on_open_change(cx.listener(|manager, open: &bool, _, cx| {
            manager.background_queue.visible = *open;
            cx.notify();
        }))
        .trigger(
            Button::new("background-task-queue-trigger")
                .ghost()
                .xsmall()
                .debug_selector(|| "background-task-queue-trigger".to_owned())
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .when(manager.background_queue.has_running(), |content| {
                            content.child(
                                div()
                                    .id("background-task-spinner")
                                    .debug_selector(|| "background-task-spinner".to_owned())
                                    .size(px(12.0))
                                    .child(
                                        gpui::component::spinner::Spinner::new()
                                            .with_size(gpui::component::Size::Size(px(12.0)))
                                            .color(rgb(theme.accent).into()),
                                    ),
                            )
                        })
                        .when(!manager.background_queue.has_running(), |content| {
                            content.child(
                                gpui::component::Icon::new(gpui::assets::IconName::ListFilter)
                                    .size(px(13.0))
                                    .text_color(rgb(theme.muted_text)),
                            )
                        })
                        .child(
                            manager
                                .background_queue
                                .tasks
                                .iter()
                                .filter(|task| task.status.is_active())
                                .count()
                                .to_string(),
                        ),
                )
                .tooltip(language.text("Background tasks"))
                .accessibility_label(language.text("Background tasks")),
        )
        .content(move |_, window, cx| {
            view.update(cx, |manager, cx| {
                div()
                    .id("background-task-queue-popup")
                    .debug_selector(|| "background-task-queue-popup".to_owned())
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .w((window.viewport_size().width - px(32.0)).min(px(420.0)))
                    .max_h((window.viewport_size().height - px(100.0)).min(px(480.0)))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .pb_2()
                            .child(
                                div()
                                    .text_sm()
                                    .child(manager.language.text("Background tasks")),
                            )
                            .child(
                                Button::new("background-task-clear-completed")
                                    .ghost()
                                    .xsmall()
                                    .icon(gpui::assets::IconName::Trash)
                                    .tooltip(manager.language.text("Clear completed tasks"))
                                    .accessibility_label(
                                        manager.language.text("Clear completed tasks"),
                                    )
                                    .on_click(cx.listener(|manager, _, _, cx| {
                                        manager
                                            .background_queue
                                            .tasks
                                            .retain(|task| task.status.is_active());
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        manager
                            .background_queue
                            .task_view(manager.language, theme, cx),
                    )
                    .into_any_element()
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::Mutex,
        task::{Poll, Waker},
    };

    #[derive(Default)]
    struct Gate {
        ready: bool,
        waker: Option<Waker>,
    }

    async fn wait(gate: Arc<Mutex<Gate>>) {
        std::future::poll_fn(move |task_cx| {
            let mut gate = gate.lock().unwrap();
            if gate.ready {
                Poll::Ready(())
            } else {
                gate.waker = Some(task_cx.waker().clone());
                Poll::Pending
            }
        })
        .await
    }

    fn release(gate: &Arc<Mutex<Gate>>) {
        let waker = {
            let mut gate = gate.lock().unwrap();
            gate.ready = true;
            gate.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    #[gpui::test]
    fn pool_limits_parallel_work_and_serializes_shared_paths(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let gates: Vec<_> = (0..5)
            .map(|_| Arc::new(Mutex::new(Gate::default())))
            .collect();
        let outcomes = Arc::new(Mutex::new(Vec::new()));
        let (view, cx) = crate::tests::component_window(cx, |_, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.current_directory = None;
            for (index, gate) in gates.iter().cloned().enumerate() {
                let outcomes = outcomes.clone();
                let resource = if index < 2 {
                    "shared.zip".to_owned()
                } else {
                    format!("archive-{index}.zip")
                };
                manager.background_queue.enqueue(
                    "Extracting archive...",
                    TaskControl::default().for_paths([resource.into()]),
                    move |control, reporter| async move {
                        wait(gate).await;
                        if !control.checkpoint() {
                            anyhow::bail!("cancelled");
                        }
                        reporter.set(1, 1);
                        Ok(index)
                    },
                    move |_, result, _, _| outcomes.lock().unwrap().push((index, result.is_ok())),
                    cx,
                );
            }
            manager
        });
        cx.run_until_parked();
        cx.read(|cx| {
            let queue = &view.read(cx).background_queue;
            assert_eq!(queue.running, MAX_WORKERS);
            assert_eq!(queue.tasks[1].status, TaskStatus::Queued);
            assert_eq!(queue.tasks[4].status, TaskStatus::Queued);
        });
        assert!(cx.debug_bounds("background-task-spinner").is_some());
        assert!(cx.debug_bounds("status-loading-spinner").is_none());
        let toggle = cx.debug_bounds("background-task-queue-trigger").unwrap();
        cx.simulate_click(toggle.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert!(cx.debug_bounds("background-task-queue-popup").is_some());
        cx.update(|window, cx| {
            view.update(cx, |manager, cx| {
                manager.background_queue.pause(2);
                cancel(manager, 5, window, cx);
            })
        });
        release(&gates[0]);
        cx.run_until_parked();
        cx.read(|cx| {
            let queue = &view.read(cx).background_queue;
            assert_eq!(queue.running, 2);
            assert_eq!(queue.tasks[1].status, TaskStatus::Paused);
            assert_eq!(queue.tasks[4].status, TaskStatus::Cancelled);
        });
        cx.update(|window, cx| {
            view.update(cx, |manager, cx| {
                manager.background_queue.resume(2, window, cx)
            })
        });
        cx.run_until_parked();
        cx.read(|cx| assert_eq!(view.read(cx).background_queue.running, 3));
        for gate in &gates[1..4] {
            release(gate);
        }
        cx.run_until_parked();
        cx.read(|cx| {
            let queue = &view.read(cx).background_queue;
            assert_eq!(queue.running, 0);
            assert!(!queue.tasks.iter().any(|task| task.status.is_active()));
        });
        assert!(cx.debug_bounds("background-task-spinner").is_none());
        let outcomes = outcomes.lock().unwrap();
        assert_eq!(outcomes.len(), 5);
        assert_eq!(outcomes.iter().filter(|(_, success)| *success).count(), 4);
    }

    #[gpui::test]
    fn running_cancel_wakes_pending_worker_and_completes_once(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let gate = Arc::new(Mutex::new(Gate::default()));
        let worker_gate = gate.clone();
        let outcomes = Arc::new(Mutex::new(Vec::new()));
        let completed = outcomes.clone();
        let (view, cx) = crate::tests::component_window(cx, |_, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.current_directory = None;
            manager.background_queue.enqueue(
                "Extracting archive...",
                TaskControl::default(),
                move |control, _| async move {
                    wait(worker_gate).await;
                    if !control.checkpoint() {
                        anyhow::bail!("cancelled");
                    }
                    Ok(())
                },
                move |_, result, _, _| {
                    completed
                        .lock()
                        .unwrap()
                        .push(matches!(result, Err(TaskFailure::Cancelled)))
                },
                cx,
            );
            manager
        });
        cx.run_until_parked();
        cx.update(|window, cx| view.update(cx, |manager, cx| cancel(manager, 1, window, cx)));
        cx.read(|cx| {
            let queue = &view.read(cx).background_queue;
            assert_eq!(queue.running, 1);
            assert_eq!(queue.tasks[0].status, TaskStatus::Cancelling);
        });
        cx.run_until_parked();
        cx.read(|cx| {
            let queue = &view.read(cx).background_queue;
            assert_eq!(queue.running, 0);
            assert_eq!(queue.tasks[0].status, TaskStatus::Cancelled);
        });
        assert_eq!(*outcomes.lock().unwrap(), vec![true]);
        release(&gate);
        cx.run_until_parked();
        assert_eq!(*outcomes.lock().unwrap(), vec![true]);
    }

    #[gpui::test]
    fn paused_future_waits_for_resume(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let gate = Arc::new(Mutex::new(Gate::default()));
        let worker_gate = gate.clone();
        let (view, cx) = crate::tests::component_window(cx, |_, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.current_directory = None;
            manager.background_queue.enqueue(
                "Extracting archive...",
                TaskControl::default(),
                move |_, _| async move {
                    wait(worker_gate).await;
                    Ok(())
                },
                |_, _, _, _| {},
                cx,
            );
            manager
        });
        cx.run_until_parked();
        view.update(cx, |manager, cx| {
            manager.background_queue.pause(1);
            cx.notify();
        });
        release(&gate);
        cx.run_until_parked();
        cx.read(|cx| {
            assert_eq!(
                view.read(cx).background_queue.tasks[0].status,
                TaskStatus::Paused
            )
        });
        cx.update(|window, cx| {
            view.update(cx, |manager, cx| {
                manager.background_queue.resume(1, window, cx)
            })
        });
        cx.run_until_parked();
        cx.read(|cx| {
            assert_eq!(
                view.read(cx).background_queue.tasks[0].status,
                TaskStatus::Completed
            )
        });
    }

    #[test]
    fn cancellation_interrupts_copy_and_scope_is_restored() {
        struct CancellingReader(TaskControl);
        impl Read for CancellingReader {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                buffer.fill(1);
                self.0.cancel();
                Ok(buffer.len())
            }
        }
        let control = TaskControl::default();
        let reporter = TaskReporter::default();
        let mut output = Vec::new();
        let result = scoped(control.clone(), reporter.clone(), || {
            copy(&mut CancellingReader(control), &mut output)
        });
        assert!(result.is_err());
        assert_eq!(output.len(), 64 * 1024);
        assert_eq!(reporter.progress.lock().unwrap().bytes, output.len() as u64);
        assert!(checkpoint().is_ok());
        assert!(CURRENT_TASK.with_borrow(Option::is_none));
    }

    #[test]
    fn cancellation_releases_a_paused_checkpoint() {
        let control = TaskControl::default();
        control.pause.store(true, Ordering::Release);
        let worker_control = control.clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || sender.send(worker_control.checkpoint()).unwrap());
        assert!(receiver.recv_timeout(Duration::from_millis(20)).is_err());
        control.cancel();
        assert!(!receiver.recv_timeout(Duration::from_secs(1)).unwrap());
        worker.join().unwrap();
    }
}

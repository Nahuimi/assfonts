//! 后台任务：进度/日志消息、复制记录、`ProgressReporter` 适配与任务启动。

use crate::prelude::*;

/// 后台任务发回界面的消息：日志行、结构化进度，以及重命名任务结束时带回的汇总。
pub(crate) enum TaskMessage {
    /// 一行日志。
    Line(String),
    /// 结构化进度：阶段名、已完成项数、总项数（`total == 0` 表示总数未知）。
    Progress {
        stage: String,
        done: usize,
        total: usize,
    },
    /// 重命名/复制任务结束时带回的汇总与可撤销记录（库层任务不发送）。
    RenameFinished {
        done: usize,
        failed: usize,
        undo: Vec<CopyRecord>,
    },
}

/// 三页共用的进度卡状态。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TaskProgress {
    /// 任务标题，如「处理 3 个输入（mks 模式）」。
    pub(crate) title: String,
    /// 当前阶段，如「写入字体索引」。
    pub(crate) stage: String,
    /// 已完成项数。
    pub(crate) done: usize,
    /// 总项数；0 表示总数未知（进度条走不确定动画）。
    pub(crate) total: usize,
}

impl TaskProgress {
    /// 0-100 的百分比；总数未知时为 `None`（不确定进度）。
    pub(crate) fn percent(&self) -> Option<f32> {
        (self.total > 0).then(|| {
            (self.done.min(self.total) as f32 / self.total as f32 * 100.0).clamp(0.0, 100.0)
        })
    }

    /// 「12/300」计数文字；总数未知时为 `None`。
    pub(crate) fn counters(&self) -> Option<String> {
        (self.total > 0).then(|| format!("{}/{}", self.done.min(self.total), self.total))
    }
}

/// 复制模式下写出的一份文件；撤销时用 size/mtime 确认它还是我们复制的那份。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CopyRecord {
    pub(crate) path: PathBuf,
    pub(crate) size: u64,
    pub(crate) mtime: i64,
}

impl CopyRecord {
    /// 复制完成后记录目标文件的大小与 mtime。
    pub(crate) fn of(path: PathBuf) -> Option<Self> {
        let metadata = std::fs::metadata(&path).ok()?;
        let mtime = metadata
            .modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs() as i64;
        Some(Self {
            path,
            size: metadata.len(),
            mtime,
        })
    }

    /// 文件仍存在且大小/mtime 都没变（用户没改过、也没被替换）。
    pub(crate) fn still_ours(&self) -> bool {
        Self::of(self.path.clone())
            .is_some_and(|now| now.size == self.size && now.mtime == self.mtime)
    }
}

/// Forwards library progress lines into the GUI log channel and exposes
/// the cancel flag to the library's checkpoints.
pub(crate) struct ChannelReporter {
    pub(crate) tx: smol::channel::Sender<TaskMessage>,
    pub(crate) cancelled: Arc<AtomicBool>,
}

impl ProgressReporter for ChannelReporter {
    fn report(&self, message: String) {
        let _ = self.tx.send_blocking(TaskMessage::Line(message));
    }

    fn progress(&self, stage: &str, done: usize, total: usize) {
        let _ = self.tx.send_blocking(TaskMessage::Progress {
            stage: stage.to_string(),
            done,
            total,
        });
    }

    fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

impl AssfontsApp {
    /// Run one task on a background thread, streaming status lines into the
    /// shared log panel.
    /// 取消当前任务：库在各工作单元之间检查该标志。
    pub(crate) fn request_cancel(&mut self, cx: &mut Context<Self>) {
        self.cancel_flag.store(true, Ordering::Relaxed);
        self.push_log("[取消] 已请求取消，等待当前单元结束…".to_string());
        cx.notify();
    }

    pub(crate) fn spawn_task(
        &mut self,
        weak: WeakEntity<Self>,
        cx: &mut App,
        what: String,
        task: impl FnOnce(&dyn ProgressReporter) -> assfonts::error::Result<()> + Send + 'static,
    ) -> Option<String> {
        self.logs.clear();
        self.running = true;
        self.cancel_flag = Arc::new(AtomicBool::new(false));
        self.task_progress = Some(TaskProgress {
            title: what.clone(),
            stage: "准备中".to_string(),
            done: 0,
            total: 0,
        });
        let label = what.clone();

        let (tx, rx) = smol::channel::unbounded::<TaskMessage>();
        let reporter = ChannelReporter {
            tx: tx.clone(),
            cancelled: Arc::clone(&self.cancel_flag),
        };
        std::thread::spawn(move || {
            let _ = tx.send_blocking(TaskMessage::Line(format!("开始{what}…")));
            match task(&reporter) {
                Ok(()) => {
                    let _ = tx.send_blocking(TaskMessage::Line(format!("{what}完成。")));
                }
                Err(err) if err.to_string().contains("cancelled") => {
                    let _ = tx.send_blocking(TaskMessage::Line(format!("[取消] {err}")));
                    let _ = tx.send_blocking(TaskMessage::Line(format!("{what}已取消。")));
                }
                Err(err) => {
                    let _ = tx.send_blocking(TaskMessage::Line(format!("[错误] {err}")));
                    let _ = tx.send_blocking(TaskMessage::Line(format!("{what}失败。")));
                }
            }
        });

        cx.spawn(async move |cx| {
            while let Ok(message) = rx.recv().await {
                match message {
                    TaskMessage::Line(message) => {
                        let suffix_done = message.ends_with("完成。");
                        let suffix_failed = message.ends_with("失败。");
                        let suffix_cancelled = message.ends_with("已取消。");
                        let _ = weak.update(cx, |this, cx| {
                            this.push_log(message);
                            if suffix_done || suffix_failed || suffix_cancelled {
                                this.running = false;
                                this.task_progress = None;
                                let note = if suffix_done {
                                    Notification::success("完成").title(label.clone())
                                } else if suffix_failed {
                                    Notification::error("失败，详见日志").title(label.clone())
                                } else {
                                    Notification::warning("已取消").title(label.clone())
                                };
                                this.push_toast(note);
                            }
                            cx.notify();
                        });
                        if suffix_done || suffix_failed || suffix_cancelled {
                            break;
                        }
                    }
                    TaskMessage::Progress { stage, done, total } => {
                        let _ = weak.update(cx, |this, cx| {
                            if let Some(progress) = this.task_progress.as_mut() {
                                // `处理字幕` 走 par_iter，完成数可能乱序到达，取已见最大值。
                                let done = if progress.stage == stage {
                                    progress.done.max(done)
                                } else {
                                    done
                                };
                                progress.stage = stage;
                                progress.done = done;
                                progress.total = total;
                            }
                            cx.notify();
                        });
                    }
                    // 只有重命名任务会发送，这里不会收到。
                    TaskMessage::RenameFinished { .. } => {}
                }
            }
            let _ = weak.update(cx, |this, cx| {
                this.running = false;
                this.task_progress = None;
                cx.notify();
            });
        })
        .detach();

        None
    }

    /// 三页共用的进度卡：标题 + 阶段/计数 + 进度条（总数未知时走不确定动画）。
    pub(crate) fn task_progress_card(&self, cx: &Context<Self>) -> Option<Div> {
        let progress = self.task_progress.as_ref()?;
        let stage_text = match progress.counters() {
            Some(counters) => format!("{} {counters}", progress.stage),
            None => progress.stage.clone(),
        };
        let bar = Progress::new("task-progress")
            .w_full()
            .loading(progress.total == 0)
            .when_some(progress.percent(), |bar, percent| bar.value(percent));
        Some(
            div()
                .w_full()
                .flex_none()
                .rounded(px(10.))
                .border_1()
                .border_color(card_border(cx))
                .bg(card_background(cx))
                .p_3()
                .child(
                    v_flex()
                        .gap_2()
                        .child(
                            h_flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .text_size(px(12.5))
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(progress.title.clone()),
                                )
                                .child(div().flex_1())
                                .child(
                                    div()
                                        .text_size(px(12.))
                                        .text_color(muted_foreground(cx))
                                        .child(stage_text),
                                ),
                        )
                        .child(bar),
                ),
        )
    }
}

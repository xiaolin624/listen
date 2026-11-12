//! The expanded result panel's state model (展开结果面板), independent of Windows.
//!
//! The capsule deliberately shows no text at all; every character a user can read
//! lives in the panel. That makes the snapshot-to-text mapping the place worth unit
//! testing, together with the two decisions the window itself must not make on its
//! own: when to open without the user asking, and which buttons can be used.
//!
//! Nothing here adds state to the application. The panel is derived from the
//! existing [`Snapshot`], so it stays consistent with the capsule and the files.

use listen::{Delivery, Phase, Preparation, Refine, Snapshot, streaming::SAMPLE_RATE};

/// The panel's button availability. `refine` is only enabled when the application
/// reports a complete result together with a refine port that can run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Actions {
    /// Copy the complete current 转写文本. Enabled exactly when there is text.
    pub copy: bool,
    /// Retry local recognition on the audio retained from a failed capture.
    pub retry: bool,
    /// Delete that retained audio.
    pub cleanup: bool,
    /// 精修: correct the current result through the configured text port.
    pub refine: bool,
}

/// Whether the application is idle enough for retry and cleanup. The tray menu and
/// the panel buttons share this function so the two surfaces can never disagree.
///
/// A pending model preparation counts as busy: the old window's controls used the
/// same rule, and starting a recovery while the model is loading would only be
/// refused by the application afterwards.
pub fn idle(snapshot: &Snapshot) -> bool {
    !matches!(
        snapshot.phase,
        Phase::Capturing | Phase::Processing | Phase::Preparing
    )
}

/// Whether local retry and cleanup are offered right now.
pub fn recovery_available(snapshot: &Snapshot) -> bool {
    idle(snapshot) && snapshot.retry_available
}

/// Which panel buttons can be used for this snapshot. The 精修 entry follows the
/// application's refine state: only `Refine::Available` enables it, and while a
/// request is running (or after one has run) it is disabled so the same result
/// cannot be refined twice.
pub fn actions(snapshot: &Snapshot) -> Actions {
    let recovery = recovery_available(snapshot);
    Actions {
        copy: !snapshot.text.is_empty(),
        retry: recovery,
        cleanup: recovery,
        refine: matches!(snapshot.refine, Refine::Available),
    }
}

/// The 精修 button's label. The disabled label states the entry is unavailable
/// instead of looking like a broken button; while a request runs it says so.
pub fn refine_label(snapshot: &Snapshot) -> &'static str {
    match snapshot.refine {
        Refine::Available => "精修",
        Refine::Running => "精修中…",
        _ => "精修（不可用）",
    }
}

/// The 精修 line for the panel's status block, or `None` when refine is not worth
/// mentioning yet. `Unavailable` is only shown for a result (where the button would
/// otherwise matter); the other states are always shown so the outcome cannot be
/// missed.
pub fn refine_note(snapshot: &Snapshot) -> Option<String> {
    match &snapshot.refine {
        Refine::Available => {
            Some("可以精修：只把下方的文字发送到 DeepSeek API，不发送音频。".into())
        }
        Refine::Running => Some("正在精修：只发送文字，不发送音频；本地结果暂时保留。".into()),
        Refine::Applied => Some(match snapshot.delivery {
            Some(Delivery::Fallback) => {
                "精修完成：没有目标输入框可替换，精修结果已保存为当前结果。".into()
            }
            _ => "精修已应用：目标输入框中的本次转写已替换为精修结果，请检查后手动发送。".into(),
        }),
        Refine::NotApplied(reason) => Some(format!(
            "精修结果没有写回目标输入框：{reason}。精修文本在下方，你的编辑保持不变。"
        )),
        Refine::Failed(reason) => Some(format!("精修失败：{reason}。本地结果保持不变，仍可复制。")),
        Refine::Unavailable(reason) if snapshot.phase == Phase::Result => {
            Some(format!("精修不可用：{reason}"))
        }
        Refine::Unavailable(_) => None,
    }
}

/// One-sentence headline for the panel. It repeats the state the capsule shows with
/// colour alone, and it never hides a fallback delivery behind a plain "done".
fn headline(snapshot: &Snapshot) -> &'static str {
    match snapshot.phase {
        Phase::Result => match snapshot.delivery {
            Some(Delivery::Appended) => "已完成：结果已追加到目标输入框",
            Some(Delivery::Fallback) => "已完成：结果只在面板中",
            None => "已完成",
        },
        Phase::NoSpeech => "没有检测到语音",
        Phase::Failure => "失败",
        // For the remaining phases the capsule's visual state is the authority on
        // "preparing / ready / capturing / processing / failed preparation".
        _ => match crate::capsule::capsule_state(snapshot) {
            crate::capsule::CapsuleState::Preparing => "正在准备本地识别模型",
            crate::capsule::CapsuleState::Ready => "就绪",
            crate::capsule::CapsuleState::Capturing => "正在采集（实时进度）",
            crate::capsule::CapsuleState::Processing => "正在本机识别",
            crate::capsule::CapsuleState::Failure => "失败",
        },
    }
}

/// One sentence that says what the headline means for the user.
fn summary(snapshot: &Snapshot) -> &'static str {
    match snapshot.phase {
        Phase::Result => match snapshot.delivery {
            Some(Delivery::Appended) => {
                "完整结果已追加到目标输入框；请检查后手动发送。这里保留了一份副本。"
            }
            Some(Delivery::Fallback) => {
                "结果没有追加到目标输入框（没有有效目标，或写入未能确认）。完整文本在下方，请复制后手动粘贴。"
            }
            None => "本次没有向任何目标写入文字；当前完整结果在下方。",
        },
        Phase::NoSpeech => "这次没有交付任何文字；上一次的结果保持不变，仍可在下方复制。",
        Phase::Failure => {
            "失败原因和保留的音频路径如下；没有自动重试，也没有向任何目标写入或发送。"
        }
        Phase::Capturing => "正在持续采集并分段识别；再按一次 Ctrl+Alt+R 停止。",
        Phase::Processing => {
            "正在本机识别，不发送任何消息；完成后结果会追加到目标或只显示在本面板。"
        }
        Phase::Preparing => "本地模型在后台加载；可以先开始采集，加载期间的音频会先缓冲。",
        Phase::Ready => "等待快捷键或托盘操作；当前结果可以随时复制。",
    }
}

/// The live capture line. `Snapshot::detail` already carries the progress during a
/// streaming capture, but it is phrased for a status file; the panel states the
/// same numbers in one short, stable sentence so a user watching the panel sees the
/// capture moving.
fn capture_progress(snapshot: &Snapshot) -> String {
    let seconds = snapshot.progress.received_samples as f64 / SAMPLE_RATE as f64;
    format!(
        "已接收 {seconds:.1} 秒音频；识别窗口已完成 {} 个，待处理 {} 个。",
        snapshot.progress.recognized_segments, snapshot.progress.pending_segments
    )
}

/// Everything the panel's status area shows: state, meaning, the live capture line
/// while capturing, the application's own human-readable detail, and a note when a
/// failed preparation blocks new captures. The live line comes before the long
/// detail so a user watching the panel sees the capture moving without scrolling.
pub fn status_text(snapshot: &Snapshot) -> String {
    let mut lines = vec![
        headline(snapshot).to_string(),
        summary(snapshot).to_string(),
    ];
    if snapshot.phase == Phase::Capturing {
        lines.push(capture_progress(snapshot));
    }
    let detail = snapshot.detail.trim();
    if !detail.is_empty() {
        lines.push(detail.to_string());
    }
    // The refine state is an action line, not a phase: it says whether the 精修
    // button can act and, once it ran, whether the text was written back.
    if let Some(note) = refine_note(snapshot) {
        lines.push(note);
    }
    // A preparation that failed outside `Phase::Failure` (for example after a
    // result that was produced while the model was still loading) must not read as
    // "everything is ready for the next capture".
    if snapshot.preparation == Preparation::Failed && snapshot.phase != Phase::Failure {
        lines.push("本地识别模型准备失败，新的采集无法进行。".into());
    }
    lines.join("\n")
}

/// The complete current 转写文本, or a placeholder when there is none yet. When a
/// refine produced text that must not be written to the target, it follows the local
/// result with an explicit label: the panel never edits either string, and the copy
/// button copies exactly the local result.
pub fn result_text(snapshot: &Snapshot) -> String {
    let current = if snapshot.text.is_empty() {
        "（还没有可复制的转写文本）".to_string()
    } else {
        snapshot.text.clone()
    };
    match &snapshot.refined_text {
        Some(refined) => {
            format!("{current}\n\n【精修结果，未写回目标输入框；可手动复制】\n{refined}")
        }
        None => current,
    }
}

/// The panel's full text in reading order: status first, then the complete result
/// (and the unapplied refine result when there is one). This is exactly what
/// `runtime\panel.txt` contains, so automation and the visible controls are
/// generated from one source.
pub fn text(snapshot: &Snapshot) -> String {
    format!("{}\n\n{}", status_text(snapshot), result_text(snapshot))
}

/// Why (and when) the panel should open without the user clicking it.
///
/// The key is stable for one event, so the caller can remember the last key it
/// acted on: the same fallback result or the same failure never re-opens a panel the
/// user has just closed, while a new failure or result does. `None` means "this
/// state is not a reason to open". A successful append is never one: the text is
/// already in the target and the capsule stays calm.
pub fn auto_open_key(snapshot: &Snapshot) -> Option<String> {
    match snapshot.phase {
        Phase::Failure => Some(format!("failure:{}", snapshot.detail)),
        Phase::Result if snapshot.delivery == Some(Delivery::Fallback) => {
            Some(format!("fallback:{}", snapshot.text))
        }
        _ => None,
    }
}

/// A rectangle in physical pixels. Kept as four integers so the positioning rules
/// stay testable without a desktop.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub const fn width(self) -> i32 {
        self.right - self.left
    }
}

/// Logical (96 DPI) size of the panel.
pub const LOGICAL_WIDTH: f32 = 640.0;
pub const LOGICAL_HEIGHT: f32 = 480.0;
/// Logical distance between the capsule and the panel.
pub const LOGICAL_GAP: f32 = 8.0;

/// Device-pixel size for a monitor scale (`dpi / 96`).
pub fn size(scale: f32) -> (i32, i32) {
    (
        (LOGICAL_WIDTH * scale).round().max(1.0) as i32,
        (LOGICAL_HEIGHT * scale).round().max(1.0) as i32,
    )
}

/// Where the panel goes: horizontally centred on the capsule, below it when there
/// is room, otherwise above it, always fully inside `work` (the capsule monitor's
/// work area, so the taskbar is never covered). `gap` is already in device pixels.
pub fn position(capsule: Rect, work: Rect, width: i32, height: i32, gap: i32) -> (i32, i32) {
    let max_x = (work.right - width).max(work.left);
    let x = (capsule.left + (capsule.width() - width) / 2).clamp(work.left, max_x);
    let below = capsule.bottom + gap;
    let above = capsule.top - gap - height;
    let max_y = (work.bottom - height).max(work.top);
    let y = if below <= max_y {
        below
    } else if above >= work.top {
        above
    } else {
        // Neither side fits: keep the panel on the same monitor, still fully visible
        // when the work area is tall enough for it.
        max_y
    };
    (x, y.clamp(work.top, max_y))
}

#[cfg(test)]
mod tests {
    use super::*;
    use listen::{Phase, Preparation, Snapshot, Timing, streaming::CaptureProgress};

    fn snapshot(phase: Phase, preparation: Preparation) -> Snapshot {
        Snapshot {
            phase,
            preparation,
            detail: String::new(),
            text: String::new(),
            retained_audio: Vec::new(),
            progress: CaptureProgress::default(),
            delivery: None,
            retry_available: false,
            endpoint: None,
            backend: None,
            timing: Timing::default(),
            refine: Refine::Unavailable("No DeepSeek API key is configured".into()),
            refined_text: None,
        }
    }

    fn result(text: &str, delivery: Delivery) -> Snapshot {
        let mut state = snapshot(Phase::Result, Preparation::Ready);
        state.text = text.into();
        state.delivery = Some(delivery);
        state.detail =
            "The complete result was appended to the original target. Review and send it manually."
                .into();
        state
    }

    #[test]
    fn appended_and_fallback_results_are_distinguishable_in_the_panel() {
        let appended = result("你好，世界", Delivery::Appended);
        let appended_text = text(&appended);
        assert!(
            appended_text.starts_with("已完成：结果已追加到目标输入框"),
            "{appended_text}"
        );
        assert!(
            appended_text.contains("请检查后手动发送"),
            "{appended_text}"
        );
        assert!(appended_text.contains("你好，世界"), "{appended_text}");

        let fallback = result("没有目标时的完整结果", Delivery::Fallback);
        let fallback_text = text(&fallback);
        assert!(
            fallback_text.starts_with("已完成：结果只在面板中"),
            "{fallback_text}"
        );
        assert!(
            fallback_text.contains("请复制后手动粘贴"),
            "{fallback_text}"
        );
        assert!(
            fallback_text.contains("没有目标时的完整结果"),
            "{fallback_text}"
        );
        assert_ne!(appended_text, fallback_text);
    }

    #[test]
    fn no_speech_says_nothing_was_delivered_and_keeps_the_previous_result_visible() {
        let mut state = snapshot(Phase::NoSpeech, Preparation::Ready);
        state.text = "上一次的结果".into();
        state.detail = "No valid speech detected; nothing was delivered. The previous current result is unchanged.".into();
        let shown = text(&state);
        assert!(shown.starts_with("没有检测到语音"), "{shown}");
        assert!(shown.contains("没有交付任何文字"), "{shown}");
        assert!(shown.contains("上一次的结果"), "{shown}");
        assert!(shown.contains("No valid speech detected"), "{shown}");
    }

    #[test]
    fn capturing_shows_live_progress_and_processing_says_it_is_local() {
        let mut capturing = snapshot(Phase::Capturing, Preparation::Ready);
        capturing.progress = CaptureProgress {
            received_samples: 48_000,
            pending_segments: 2,
            recognized_segments: 1,
            ..CaptureProgress::default()
        };
        let shown = status_text(&capturing);
        assert!(shown.starts_with("正在采集（实时进度）"), "{shown}");
        assert!(shown.contains("已接收 3.0 秒音频"), "{shown}");
        assert!(shown.contains("已完成 1 个"), "{shown}");
        assert!(shown.contains("待处理 2 个"), "{shown}");

        let processing = snapshot(Phase::Processing, Preparation::Ready);
        let shown = status_text(&processing);
        assert!(shown.starts_with("正在本机识别"), "{shown}");
        assert!(shown.contains("不发送任何消息"), "{shown}");
    }

    #[test]
    fn a_failure_shows_the_reason_and_every_retained_audio_path() {
        // `Application::fail` puts the retained paths into `detail`; the panel must
        // show that text verbatim, including a path with spaces and backslashes.
        let mut state = snapshot(Phase::Failure, Preparation::Ready);
        state.retry_available = true;
        state.detail = "Local retry failed: the model is unavailable. Audio retained for local recovery: C:\\runtime\\session 1\\failed.wav".into();
        state.retained_audio = vec![std::path::PathBuf::from(
            "C:\\runtime\\session 1\\failed.wav",
        )];
        let shown = text(&state);
        assert!(shown.starts_with("失败"), "{shown}");
        assert!(shown.contains("失败原因"), "{shown}");
        assert!(shown.contains("the model is unavailable"), "{shown}");
        assert!(
            shown.contains("C:\\runtime\\session 1\\failed.wav"),
            "{shown}"
        );
    }

    #[test]
    fn a_model_preparation_failure_never_reads_as_ready() {
        let mut state = snapshot(Phase::Ready, Preparation::Failed);
        state.detail = "The local model is unavailable.".into();
        let shown = status_text(&state);
        assert!(shown.starts_with("失败"), "{shown}");
        assert!(shown.contains("新的采集无法进行"), "{shown}");

        let preparing = snapshot(Phase::Ready, Preparation::Pending);
        assert!(status_text(&preparing).starts_with("正在准备本地识别模型"));
    }

    #[test]
    fn panel_text_is_status_then_the_complete_result() {
        let mut state = result("第一行\n第二行", Delivery::Appended);
        state.detail = "Appended.".into();
        let shown = text(&state);
        let (status, result) = shown.split_once("\n\n").expect("status and result");
        assert!(
            status.contains("已完成：结果已追加到目标输入框"),
            "{status}"
        );
        assert!(status.contains("Appended."), "{status}");
        assert_eq!(result, "第一行\n第二行");
    }

    #[test]
    fn an_empty_result_gets_a_placeholder_but_the_status_stays_readable() {
        let state = snapshot(Phase::Ready, Preparation::Ready);
        assert_eq!(result_text(&state), "（还没有可复制的转写文本）");
        assert!(text(&state).ends_with("（还没有可复制的转写文本）"));
    }

    #[test]
    fn auto_open_fires_for_fallback_results_and_failures_only() {
        assert_eq!(
            auto_open_key(&result("完整文本", Delivery::Appended)),
            None,
            "a successful append must not steal the screen"
        );
        assert!(
            auto_open_key(&result("完整文本", Delivery::Fallback)).is_some(),
            "a fallback result must be reachable immediately"
        );

        let mut failure = snapshot(Phase::Failure, Preparation::Ready);
        failure.detail = "Recognition failed".into();
        let first = auto_open_key(&failure).expect("failure opens the panel");
        assert_eq!(
            auto_open_key(&failure),
            Some(first.clone()),
            "the same failure must not re-open a panel the user closed"
        );
        failure.detail = "Recognition failed differently".into();
        assert_ne!(auto_open_key(&failure), Some(first));

        for phase in [
            Phase::Preparing,
            Phase::Ready,
            Phase::Capturing,
            Phase::Processing,
            Phase::NoSpeech,
        ] {
            assert_eq!(
                auto_open_key(&snapshot(phase.clone(), Preparation::Ready)),
                None,
                "phase {phase:?} must not auto-open the panel"
            );
        }
    }

    #[test]
    fn every_new_fallback_or_failure_produces_a_new_auto_open_key() {
        let first = auto_open_key(&result("第一段", Delivery::Fallback));
        let second = auto_open_key(&result("第二段", Delivery::Fallback));
        assert!(first.is_some() && second.is_some());
        assert_ne!(first, second);
    }

    #[test]
    fn retry_and_cleanup_are_offered_together_and_only_while_idle_with_retained_audio() {
        let mut failed = snapshot(Phase::Failure, Preparation::Ready);
        failed.retry_available = true;
        let offered = actions(&failed);
        assert!(offered.retry && offered.cleanup);
        assert!(!offered.copy, "there is no result to copy");
        assert!(!offered.refine, "a failed capture has no result to refine");

        failed.text = "保留的部分结果".into();
        assert!(actions(&failed).copy);

        for busy in [Phase::Capturing, Phase::Processing, Phase::Preparing] {
            let mut state = snapshot(busy.clone(), Preparation::Ready);
            state.retry_available = true;
            let offered = actions(&state);
            assert!(
                !offered.retry && !offered.cleanup,
                "phase {busy:?} must not offer recovery"
            );
        }

        let idle_but_nothing_retained = snapshot(Phase::Ready, Preparation::Ready);
        let offered = actions(&idle_but_nothing_retained);
        assert!(!offered.retry && !offered.cleanup);
    }

    #[test]
    fn recovery_availability_matches_the_actions_the_panel_shows() {
        for phase in [
            Phase::Preparing,
            Phase::Ready,
            Phase::Capturing,
            Phase::Processing,
            Phase::Result,
            Phase::NoSpeech,
            Phase::Failure,
        ] {
            for retry_available in [false, true] {
                let mut state = snapshot(phase.clone(), Preparation::Ready);
                state.retry_available = retry_available;
                let shown = actions(&state);
                assert_eq!(shown.retry, recovery_available(&state));
                assert_eq!(shown.cleanup, recovery_available(&state));
            }
        }
    }

    #[test]
    fn refine_is_enabled_exactly_when_the_application_reports_available() {
        let mut state = result("完整结果", Delivery::Appended);
        assert!(!actions(&state).refine, "unavailable by default");
        state.refine = Refine::Available;
        assert!(actions(&state).refine);
        for done in [
            Refine::Running,
            Refine::Applied,
            Refine::NotApplied("edited".into()),
            Refine::Failed("network".into()),
        ] {
            state.refine = done;
            assert!(!actions(&state).refine, "only Available may enable 精修");
        }
    }

    #[test]
    fn the_refine_label_follows_the_refine_state() {
        let mut state = result("完整结果", Delivery::Appended);
        state.refine = Refine::Available;
        assert_eq!(refine_label(&state), "精修");
        state.refine = Refine::Running;
        assert_eq!(refine_label(&state), "精修中…");
        state.refine = Refine::Failed("no network".into());
        assert_eq!(refine_label(&state), "精修（不可用）");
    }

    #[test]
    fn a_refine_result_that_must_not_be_written_is_shown_below_the_local_result() {
        let mut state = result("本地完整结果", Delivery::Appended);
        state.refine = Refine::NotApplied(
            "the original target was edited after delivery, so it was not overwritten".into(),
        );
        state.refined_text = Some("精修后的文本".into());

        assert_eq!(state.text, "本地完整结果");
        let result_part = result_text(&state);
        assert!(result_part.starts_with("本地完整结果"), "{result_part}");
        assert!(result_part.contains("未写回目标输入框"), "{result_part}");
        assert!(result_part.contains("精修后的文本"), "{result_part}");

        let shown = text(&state);
        assert!(shown.contains("精修结果没有写回目标输入框"), "{shown}");
        assert!(shown.contains("你的编辑保持不变"), "{shown}");
        assert!(shown.contains("精修后的文本"), "{shown}");
    }

    #[test]
    fn an_applied_refine_says_it_was_written_back_and_does_not_duplicate_the_text() {
        let mut state = result("精修后的完整结果", Delivery::Appended);
        state.refine = Refine::Applied;
        let shown = text(&state);
        assert!(shown.contains("精修已应用"), "{shown}");
        assert!(shown.contains("请检查后手动发送"), "{shown}");
        assert_eq!(result_text(&state), "精修后的完整结果");
        assert!(!shown.contains("未写回目标输入框"), "{shown}");
    }

    #[test]
    fn refine_unavailability_is_explained_where_the_button_would_matter() {
        let ready = snapshot(Phase::Ready, Preparation::Ready);
        assert!(
            !status_text(&ready).contains("精修不可用"),
            "an idle panel does not need a refine explanation yet"
        );

        let mut no_key = result("完整结果", Delivery::Fallback);
        no_key.refine = Refine::Unavailable("No DeepSeek API key is configured".into());
        let shown = status_text(&no_key);
        assert!(
            shown.contains("精修不可用：No DeepSeek API key is configured"),
            "{shown}"
        );
        assert!(!actions(&no_key).refine);
    }

    #[test]
    fn panel_size_scales_with_the_monitor_dpi() {
        assert_eq!(size(1.0), (640, 480));
        assert_eq!(size(1.25), (800, 600));
        assert_eq!(size(1.5), (960, 720));
        assert_eq!(size(2.0), (1280, 960));
    }

    fn capsule(left: i32, top: i32) -> Rect {
        Rect::new(left, top, left + 275, top + 33)
    }

    const WORK: Rect = Rect::new(0, 0, 1920, 1040);

    #[test]
    fn the_panel_opens_below_the_capsule_when_there_is_room() {
        let (x, y) = position(capsule(822, 48), WORK, 640, 480, 8);
        assert_eq!(x, 822 + (275 - 640) / 2);
        assert_eq!(x, 640);
        assert_eq!(y, 48 + 33 + 8);
    }

    #[test]
    fn the_panel_moves_above_the_capsule_when_below_does_not_fit() {
        let low = Rect::new(0, 900, 275, 933);
        let (_, y) = position(low, WORK, 640, 480, 8);
        assert_eq!(y, 900 - 8 - 480);
        assert!(y >= WORK.top);
    }

    #[test]
    fn the_panel_is_clamped_inside_the_work_area_on_every_edge() {
        // Near the left edge the centred panel would start at a negative x.
        let (x, _) = position(capsule(0, 48), WORK, 640, 480, 8);
        assert_eq!(x, 0);
        // Near the right edge it would run past the work area.
        let (x, _) = position(capsule(WORK.right - 275, 48), WORK, 640, 480, 8);
        assert_eq!(x, WORK.right - 640);
        // A work area shorter than "panel + gaps" has no room on either side; the
        // panel is still kept fully visible by clamping to the bottom edge.
        let short = Rect::new(0, 0, 1920, 500);
        let (_, y) = position(Rect::new(100, 240, 375, 273), short, 640, 480, 8);
        assert_eq!(y, short.bottom - 480);
        assert_eq!(y, 20);
    }

    #[test]
    fn the_panel_stays_on_the_capsule_monitor() {
        // A second monitor to the right with its own work area and a capsule on it.
        let second = Rect::new(1920, 0, 3840, 1040);
        let on_second = Rect::new(2400, 40, 2675, 73);
        let (x, y) = position(on_second, second, 640, 480, 8);
        assert!((second.left..=second.right - 640).contains(&x), "x={x}");
        assert!((second.top..=second.bottom - 480).contains(&y), "y={y}");
        // And a capsule at the very right edge of the second monitor.
        let (x, _) = position(Rect::new(3600, 40, 3875, 73), second, 640, 480, 8);
        assert_eq!(x, second.right - 640);
    }

    #[test]
    fn every_capsule_position_keeps_the_panel_inside_the_work_area() {
        for left in (0..1920).step_by(160) {
            for top in (0..1040).step_by(120) {
                let (x, y) = position(capsule(left, top), WORK, 640, 480, 8);
                assert!(
                    (WORK.left..=WORK.right - 640).contains(&x),
                    "capsule at {left},{top} produced x={x}"
                );
                assert!(
                    (WORK.top..=WORK.bottom - 480).contains(&y),
                    "capsule at {left},{top} produced y={y}"
                );
            }
        }
    }
}

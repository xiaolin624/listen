//! The expanded result panel's state model (展开结果面板), independent of Windows.
//!
//! The capsule deliberately shows no text at all; every character a user can read
//! lives in the panel. That makes the snapshot-to-text mapping the place worth unit
//! testing, together with the two decisions the window itself must not make on its
//! own: when to open without the user asking, and which buttons can be used.
//!
//! Nothing here adds state to the application. The panel is derived from the
//! existing [`Snapshot`], so it stays consistent with the capsule and the files.

use listen::{
    Delivery, Phase, Preparation, Refine, SegmentState, SegmentSummary, Snapshot, UsageSummary,
    storage::StorageLevel, streaming::SAMPLE_RATE,
};

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
        Refine::Running => Some("正在精修：只发送文字，不发送音频；当前结果暂时保留。".into()),
        Refine::Applied => Some(match snapshot.delivery {
            Some(Delivery::Fallback) | Some(Delivery::Uncertain) => {
                "精修完成：没有目标输入框可替换，精修结果已保存为当前结果。".into()
            }
            _ => "精修已应用：目标输入框中的本次转写已替换为精修结果，请检查后手动发送。".into(),
        }),
        Refine::NotApplied(reason) => Some(format!(
            "精修结果没有写回目标输入框：{reason}。精修文本在下方，你的编辑保持不变。"
        )),
        Refine::Failed(reason) => Some(format!("精修失败：{reason}。当前结果保持不变，仍可复制。")),
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
            Some(Delivery::Uncertain) => "已完成：是否已写入目标无法确认，结果只在面板中",
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
            crate::capsule::CapsuleState::Processing => {
                if snapshot.cloud_trial() {
                    "正在云端识别"
                } else {
                    "正在本机识别"
                }
            }
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
            Some(Delivery::Uncertain) => {
                "是否已写入原目标无法确认（上一进程的写入结果不确定，或响应丢失）；没有再次写入。完整文本在下方，请先检查原目标再复制处理。"
            }
            None => "本次没有向任何目标写入文字；当前完整结果在下方。",
        },
        Phase::NoSpeech => "这次没有交付任何文字；上一次的结果保持不变，仍可在下方复制。",
        Phase::Failure => {
            "失败原因和保留的音频路径如下；没有自动重试，也没有向任何目标写入或发送。"
        }
        Phase::Capturing if snapshot.cloud_trial() => {
            "正在保存并逐步发送本次耳机音频；再按一次 Ctrl+Alt+R 停止，完整确认后交付。"
        }
        Phase::Capturing => "正在持续采集并分段识别；再按一次 Ctrl+Alt+R 停止。",
        Phase::Processing if snapshot.cloud_trial() => {
            "正在等待云端整段完成确认；音频保留在本机，确认并保存完整文字后再安全回填。"
        }
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
    if snapshot.cloud_trial() {
        return format!(
            "已接收 {seconds:.1} 秒连续音频；{} 字节已保存。稳定分句不等于完整结果。",
            snapshot.progress.spool_bytes
        );
    }
    format!(
        "已接收 {seconds:.1} 秒音频；识别窗口已完成 {} 个，待处理 {} 个。",
        snapshot.progress.recognized_segments, snapshot.progress.pending_segments
    )
}

/// The panel's label for one segment's state. It is a user-facing fact, never a
/// number the capsule shows. The tray's per-segment cleanup submenu reuses it, so
/// both surfaces describe a segment the same way.
pub fn segment_label(segment: &SegmentSummary) -> &'static str {
    match segment.state {
        SegmentState::Capturing => "正在采集",
        SegmentState::WaitingForNetwork => "等待联网，已收到的声音保存在这台电脑上",
        SegmentState::Processing => "处理中，等待完整结果",
        SegmentState::Result => match segment.delivery {
            Some(Delivery::Appended) => "已完成，结果已追加到目标输入框",
            Some(Delivery::Fallback) => "已完成，结果只在面板中，尚未写入任何输入框",
            Some(Delivery::Uncertain) => {
                "已完成，是否已写入原目标无法确认；结果保留在面板中，没有再次写入"
            }
            None => "已完成",
        },
        SegmentState::NoSpeech => "没有检测到语音",
        SegmentState::Failed => "失败，录音仍保留在本机",
    }
}

/// One complete result that is still waiting to be taken, oldest first. The current
/// result is shown by [`result_text`] itself, so only the other pending results are
/// listed here; a later segment never replaces an earlier entry. A delivery whose
/// write may have happened is still a result the user must take or check.
pub fn pending_results(snapshot: &Snapshot) -> Vec<&SegmentSummary> {
    snapshot
        .segments
        .iter()
        .filter(|segment| {
            !segment.text.is_empty()
                && segment.state == SegmentState::Result
                && matches!(
                    segment.delivery,
                    Some(Delivery::Fallback) | Some(Delivery::Uncertain)
                )
                && snapshot.text != segment.text
        })
        .collect()
}

/// The multi-segment block for the panel's status area: how many older segments
/// still wait, how many complete results are waiting to be taken, and each
/// segment's own state in capture order. A single delivered capture reads exactly
/// as before, so the ordinary flow is unchanged.
pub fn segments_note(snapshot: &Snapshot) -> Option<String> {
    if snapshot.segments.is_empty() {
        return None;
    }
    let pending = pending_results(snapshot).len();
    if snapshot.segments.len() == 1 && pending == 0 {
        return None;
    }
    // The newest segment is the one the headline already describes; the count is
    // the older backlog that must not be lost behind it.
    let latest = snapshot.segments.last().map(|segment| segment.sequence);
    let waiting = snapshot
        .segments
        .iter()
        .filter(|segment| segment.state.in_flight() && Some(segment.sequence) != latest)
        .count();
    let mut lines = vec![format!(
        "多段记录（按采集先后）：等待 {waiting} 段，待取结果 {pending} 段。"
    )];
    for segment in &snapshot.segments {
        let mut line = format!("第 {} 段：{}", segment.sequence, segment_label(segment));
        let detail = segment.detail.trim().replace(['\r', '\n'], " ");
        if !detail.is_empty() && snapshot.segments.len() > 1 {
            line.push_str(&format!("（{detail}）"));
        }
        lines.push(line);
    }
    Some(lines.join("\n"))
}

/// One amount in CNY. Amounts are exact integer 纳元 in the ledger and become
/// readable here only; this rounding is for display and is never used as a cost.
fn cny(nanoyuan: u64) -> String {
    format!("{:.2}", nanoyuan as f64 / 1_000_000_000.0)
}

/// A whole-CNY policy figure (the trial cap and the monthly reminder line are
/// stated in whole yuan, so they are never shown with misleading decimals).
fn whole_cny(nanoyuan: u64) -> String {
    (nanoyuan / 1_000_000_000).to_string()
}

/// The 云端试用 usage lines: what this tool spent, kept apart from the unknown
/// part, and the two independent limits. It is derived from the durable ledger, so
/// it remains visible in every phase, including before any capture starts.
fn usage_lines(usage: &UsageSummary) -> Vec<String> {
    if usage.unreadable {
        return vec![
            "云端用量暂时无法读取本机账本，因此不能当作 0 元；已确认、保守估算和未知都无法计算。本机账本可读前不会发出新的付费请求。"
                .into(),
        ];
    }
    let mut counts = format!("共 {} 次请求", usage.requests);
    if usage.retried_segments > 0 {
        // A rebuilt or repeated request is a new paid request; the panel never
        // implies that de-duplication makes it free.
        counts.push_str(&format!(
            "（含 {} 段重试请求，重试同样计费）",
            usage.retried_segments
        ));
    }
    counts.push_str(&format!(
        "；已取得最终用量 {} 次，保守估算 {} 次，未知 {} 次（实际用量或扣款未核实，不按 0 元计）。",
        usage.settled_requests,
        usage.requests.saturating_sub(usage.settled_requests),
        usage.unknown_requests
    ));
    let mut money = format!(
        "累计费用估计 {} 元 = 已确认 {} 元 + 保守预留 {} 元。",
        cny(usage.lifetime_nanoyuan()),
        cny(usage.confirmed_nanoyuan),
        cny(usage.estimated_nanoyuan)
    );
    if usage.confirmed_input_tokens > 0 || usage.confirmed_output_tokens > 0 {
        money.push_str(&format!(
            "已确认用量：输入 {} tokens、输出 {} tokens。",
            usage.confirmed_input_tokens, usage.confirmed_output_tokens
        ));
    }
    if usage.undated_requests > 0 {
        money.push_str(&format!(
            "另有 {} 次旧记录无法确定月份，已计入累计、未计入本月。",
            usage.undated_requests
        ));
    }
    let month = format!(
        "本月（UTC）合计 {} 元，未接近每月 {} 元提醒线（达到 90% 即提醒）；首轮 {} 元试用上限独立生效，提醒不会停用功能或增加额度。",
        cny(usage.month_nanoyuan()),
        whole_cny(UsageSummary::MONTH_REMINDER_NANOYUAN),
        whole_cny(usage.trial_cap_nanoyuan)
    );
    let month = if usage.reminder_reached() {
        if usage.month_nanoyuan() >= UsageSummary::MONTH_REMINDER_NANOYUAN {
            format!(
                "本月（UTC）合计 {} 元，已达到每月 {} 元提醒线；提醒不会改变采集、额度或套餐。",
                cny(usage.month_nanoyuan()),
                whole_cny(UsageSummary::MONTH_REMINDER_NANOYUAN)
            )
        } else {
            format!(
                "本月（UTC）合计 {} 元，已接近每月 {} 元提醒线（达到 90% 即提醒）；提醒不会改变采集、额度或套餐。",
                cny(usage.month_nanoyuan()),
                whole_cny(UsageSummary::MONTH_REMINDER_NANOYUAN)
            )
        }
    } else {
        month
    };
    vec![
        format!("云端用量（仅统计本工具发出的识别请求，不含同一云账号的其他应用或套餐）：{counts}"),
        money,
        month,
    ]
}

/// One amount of bytes for the storage lines. Storage is displayed as a rough size
/// only; every decision is made in exact bytes by `listen::storage`.
fn storage_size(bytes: u64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / MIB)
    } else if bytes >= 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} bytes")
    }
}

/// Whether this segment still has a pending recording or pending result the user may
/// delete explicitly (#36). The live capture is stopped first, so it is never offered;
/// a result that was already appended to its target has nothing pending left.
fn deletable(segment: &SegmentSummary) -> bool {
    match segment.state {
        SegmentState::Capturing | SegmentState::NoSpeech => false,
        SegmentState::WaitingForNetwork | SegmentState::Processing | SegmentState::Failed => true,
        SegmentState::Result => matches!(
            segment.delivery,
            Some(Delivery::Fallback) | Some(Delivery::Uncertain)
        ),
    }
}

/// The capture orders whose pending recording and/or pending result the user may
/// delete right now, in capture order. This is what the tray's per-segment cleanup
/// submenu offers; the application repeats the safety checks before deleting.
pub fn deletable_segments(snapshot: &Snapshot) -> Vec<u64> {
    snapshot
        .segments
        .iter()
        .filter(|segment| deletable(segment))
        .map(|segment| segment.sequence)
        .collect()
}

/// The storage block for the panel's status area (#36): the warning before the
/// budget, the two hard stops, and a real write/save failure. A normal measurement
/// adds no line, so the ordinary flow reads exactly as before.
pub fn storage_note(snapshot: &Snapshot) -> Option<String> {
    let status = snapshot.storage.as_ref()?;
    let mut lines = Vec::new();
    match status.level() {
        StorageLevel::Ok => {}
        StorageLevel::Warning => lines.push(format!(
            "存储接近待处理音频上限：已用 {}，上限 {}（跨全部段统计，含临时上传、重叠分片和修复文件）。达到上限后会停止开始新的采集，已收到的声音和旧段都会保留；可清理不再需要的段或调整预算。",
            storage_size(status.pending_audio_bytes),
            storage_size(status.budget_bytes)
        )),
        StorageLevel::Full => lines.push(format!(
            "待处理音频已达上限：已用 {}，上限 {}。不会再开始新的采集，正在采集的段也会停止；已经收到的声音和旧段都保留在本机，不会被覆盖。清理不需要的段或调高预算后可继续。",
            storage_size(status.pending_audio_bytes),
            storage_size(status.budget_bytes)
        )),
        StorageLevel::DiskLow => lines.push(format!(
            "运行目录所在磁盘只剩 {}，低于 {} 底线。不会再开始新的采集；已经收到的声音和旧段都保留在本机。请先释放磁盘空间。这是本机存储问题，不是联网等待或播放设备中断。",
            status
                .free_disk_bytes
                .map(storage_size)
                .unwrap_or_else(|| "未知".into()),
            storage_size(status.free_disk_floor_bytes)
        )),
    }
    if let Some(fault) = &status.fault {
        lines.push(format!(
            "存储写入失败：{fault}\n这不是联网等待，也不是播放设备中断；录音和已收到的结果仍保留在本机。"
        ));
    }
    if !lines.is_empty() {
        if snapshot.segments.iter().any(deletable) {
            lines.push(
                "可逐段清理：托盘菜单「清理待处理段」，或运行 listen.exe --control delete-segment:<段号>。只删除选定的段，不影响其他未完成内容。"
                    .into(),
            );
        }
    }
    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
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
    if snapshot.cloud_trial() {
        if let Some(backend) = &snapshot.backend {
            lines.push(backend.clone());
        }
    }
    // The cumulative cloud cost is a fact about this tool's own usage, so it is
    // shown wherever a cloud adapter reports it — before the long detail, and
    // without ever hiding the three cost categories behind one rounded number.
    if let Some(usage) = &snapshot.usage {
        lines.extend(usage_lines(usage));
    }
    if snapshot.phase == Phase::Capturing {
        lines.push(capture_progress(snapshot));
    }
    let detail = snapshot.detail.trim();
    if !detail.is_empty() {
        lines.push(detail.to_string());
    }
    // The storage bounds are a fact about this machine's pending audio, shown with
    // their consequence before any older segment's own line.
    if let Some(note) = storage_note(snapshot) {
        lines.push(note);
    }
    // The multi-segment block comes after the current operation's detail: it never
    // hides what is happening now, and each older segment keeps its own state.
    if let Some(note) = segments_note(snapshot) {
        lines.push(note);
    }
    // A condition outside the capture flow (a failed copy, a capsule the platform
    // refused to hide from screen capture) is shown after the phase detail so the
    // user reads what happened and what it means without opening a log file.
    if let Some(notice) = &snapshot.notice {
        lines.push(notice.text());
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
        lines.push(if snapshot.cloud_trial() {
            "云端试用准备未完成，新的云端请求未发送。".into()
        } else {
            "本地识别模型准备失败，新的采集无法进行。".into()
        });
    }
    lines.join("\n")
}

/// The complete current 转写文本, or a placeholder when there is none yet. When a
/// refine produced text that must not be written to the target, it follows the local
/// result with an explicit label. Older complete results that still wait to be taken
/// follow in capture order, each in full and with its own label, so a later segment
/// can never replace an earlier pending result. The copy button copies exactly the
/// current result; every pending result is selectable and copyable by hand here.
pub fn result_text(snapshot: &Snapshot) -> String {
    let mut shown = if snapshot.text.is_empty() {
        "（还没有可复制的转写文本）".to_string()
    } else {
        snapshot.text.clone()
    };
    if let Some(refined) = &snapshot.refined_text {
        shown.push_str(&format!(
            "\n\n【精修结果，未写回目标输入框；可手动复制】\n{refined}"
        ));
    }
    for segment in pending_results(snapshot) {
        shown.push_str(&format!(
            "\n\n【第 {} 段 · 待取结果；可手动复制】\n{}",
            segment.sequence, segment.text
        ));
    }
    shown
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
/// Only a fallback delivery qualifies: issue #20 asks for click-to-toggle and its
/// story 5 wants a result with no valid target to be reachable without the user
/// discovering the capsule. A failure does not open the panel — the capsule turns
/// red and the user clicks it to read the reason (#20's story 10) — so a failure
/// never steals the screen.
///
/// The key is stable for one event, so the caller can remember the last key it
/// acted on: the same fallback result never re-opens a panel the user has just
/// closed, while a new fallback result does. `None` means "this state is not a
/// reason to open". A successful append is never one: the text is already in the
/// target and the capsule stays calm.
pub fn auto_open_key(snapshot: &Snapshot) -> Option<String> {
    match snapshot.phase {
        Phase::Result if snapshot.delivery == Some(Delivery::Fallback) => {
            Some(format!("fallback:{}", snapshot.text))
        }
        // A result whose write may have happened still needs the user: it is not a
        // calm "already appended" and it must not be discovered only by accident.
        Phase::Result if snapshot.delivery == Some(Delivery::Uncertain) => {
            Some(format!("uncertain:{}", snapshot.text))
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
            recognition: None,
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
            notice: None,
            usage: None,
            segments: Vec::new(),
            storage: None,
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
    fn a_recovered_result_whose_write_may_have_happened_is_not_shown_as_a_plain_panel_fallback() {
        // #35: after a restart an unknown delivery must never read like "nothing was
        // written" and never like "already appended".
        let uncertain = result("重启后恢复的完整结果", Delivery::Uncertain);
        let shown = text(&uncertain);
        assert!(
            shown.starts_with("已完成：是否已写入目标无法确认"),
            "{shown}"
        );
        assert!(shown.contains("没有再次写入"), "{shown}");
        assert!(shown.contains("请先检查原目标"), "{shown}");
        assert!(
            !shown.contains("尚未写入任何输入框"),
            "an unknown write is not a proven fallback: {shown}"
        );
        assert_ne!(
            shown,
            text(&result("重启后恢复的完整结果", Delivery::Fallback))
        );

        // It still counts as a result the user must take or check, and it is offered
        // for explicit cleanup like any other pending result.
        let mut state = result("重启后恢复的完整结果", Delivery::Uncertain);
        state.segments = vec![
            segment(1, SegmentState::Result, "旧结果", Some(Delivery::Uncertain)),
            segment(
                2,
                SegmentState::Result,
                "当前结果",
                Some(Delivery::Appended),
            ),
        ];
        state.text = "当前结果".into();
        assert_eq!(pending_results(&state).len(), 1);
        assert_eq!(deletable_segments(&state), vec![1]);
        assert_eq!(
            auto_open_key(&state).as_deref(),
            Some("uncertain:当前结果"),
            "an uncertain result is not hidden behind a calm appended delivery"
        );
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
    fn a_notice_is_rendered_in_the_status_block_and_never_hides_the_result() {
        let mut state = result("完整结果", Delivery::Fallback);
        state.notice = Some(listen::Notice::CopyFailed(
            "the clipboard is locked by another application".into(),
        ));
        let shown = text(&state);
        assert!(
            shown.contains(
                "Could not copy the result: the clipboard is locked by another application"
            ),
            "{shown}"
        );
        assert!(shown.contains("The clipboard was not replaced"), "{shown}");
        assert!(shown.ends_with("完整结果"), "{shown}");

        state.notice = Some(listen::Notice::CaptureExcluded("Access is denied".into()));
        let shown = text(&state);
        assert!(
            shown.contains("could not be hidden from screen capture"),
            "{shown}"
        );
        assert!(shown.contains("Access is denied"), "{shown}");
    }

    #[test]
    fn an_empty_result_gets_a_placeholder_but_the_status_stays_readable() {
        let state = snapshot(Phase::Ready, Preparation::Ready);
        assert_eq!(result_text(&state), "（还没有可复制的转写文本）");
        assert!(text(&state).ends_with("（还没有可复制的转写文本）"));
    }

    #[test]
    fn cloud_cost_is_shown_as_confirmed_conservative_and_unknown_never_as_zero() {
        let mut state = result("完整结果", Delivery::Appended);
        state.backend = Some(
            "Cloud trial: Alibaba Model Studio; model=qwen-audio-3.1-asr-flash-message; \
             region=cn-beijing; shared single-vendor trial cap=CNY 50"
                .into(),
        );
        state.usage = Some(listen::UsageSummary {
            requests: 3,
            settled_requests: 1,
            confirmed_nanoyuan: 420_000_000,
            estimated_nanoyuan: 28_500_000_000,
            unknown_requests: 2,
            retried_segments: 1,
            undated_requests: 0,
            month_confirmed_nanoyuan: 420_000_000,
            month_estimated_nanoyuan: 28_500_000_000,
            month_unknown_requests: 2,
            trial_cap_nanoyuan: 50_000_000_000,
            confirmed_input_tokens: 70_000,
            confirmed_output_tokens: 1_000,
            unreadable: false,
        });
        let shown = status_text(&state);
        // Coverage is this tool's own usage, never the whole cloud account.
        assert!(shown.contains("仅统计本工具"), "{shown}");
        assert!(shown.contains("不含同一云账号"), "{shown}");
        // Confirmed, conservative and unknown stay three separate figures.
        assert!(shown.contains("已确认 0.42 元"), "{shown}");
        assert!(shown.contains("保守预留 28.50 元"), "{shown}");
        assert!(shown.contains("未知 2 次"), "{shown}");
        assert!(shown.contains("不按 0 元计"), "{shown}");
        // Retries are visible as requests, not silently merged away.
        assert!(shown.contains("含 1 段重试"), "{shown}");
        // The 50 CNY trial cap and the 100 CNY reminder line are separate facts.
        assert!(shown.contains("首轮 50 元试用上限独立生效"), "{shown}");
        assert!(shown.contains("每月 100 元提醒线"), "{shown}");
        assert!(shown.contains("达到 90% 即提醒"), "{shown}");
        assert!(!shown.contains("已达到每月 100 元提醒线"), "{shown}");
        // The complete result is still rendered after the status block.
        assert!(text(&state).ends_with("完整结果"));
    }

    #[test]
    fn approaching_the_monthly_reminder_line_is_rendered_as_a_reminder() {
        let mut state = snapshot(Phase::Result, Preparation::Ready);
        state.backend = Some("Cloud trial: Alibaba Model Studio".into());
        state.usage = Some(listen::UsageSummary {
            requests: 2,
            settled_requests: 2,
            confirmed_nanoyuan: 95_000_000_000,
            estimated_nanoyuan: 0,
            month_confirmed_nanoyuan: 95_000_000_000,
            trial_cap_nanoyuan: 50_000_000_000,
            unreadable: false,
            ..listen::UsageSummary::default()
        });
        let shown = status_text(&state);
        assert!(shown.contains("已接近每月 100 元提醒线"), "{shown}");
        assert!(shown.contains("达到 90% 即提醒"), "{shown}");
        assert!(
            !shown.contains("已达到每月 100 元提醒线"),
            "95 元 is below the line, not at it: {shown}"
        );
        // Approaching the line still never changes the trial cap or the feature.
        assert!(shown.contains("提醒不会改变采集、额度或套餐"), "{shown}");
    }

    #[test]
    fn reaching_the_monthly_reminder_line_is_a_reminder_not_a_stop() {
        let mut state = snapshot(Phase::Capturing, Preparation::Ready);
        state.backend = Some("Cloud trial: Alibaba Model Studio".into());
        state.usage = Some(listen::UsageSummary {
            requests: 6,
            settled_requests: 3,
            confirmed_nanoyuan: 60_000_000_000,
            estimated_nanoyuan: 60_000_000_000,
            unknown_requests: 3,
            retried_segments: 1,
            undated_requests: 2,
            month_confirmed_nanoyuan: 60_000_000_000,
            month_estimated_nanoyuan: 60_000_000_000,
            month_unknown_requests: 3,
            trial_cap_nanoyuan: 50_000_000_000,
            confirmed_input_tokens: 1_000,
            confirmed_output_tokens: 2_000,
            unreadable: false,
        });
        let shown = status_text(&state);
        // The reminder is visible while the capture keeps running, and the panel
        // states that the feature is unaffected.
        assert!(shown.starts_with("正在采集（实时进度）"), "{shown}");
        assert!(shown.contains("已达到每月 100 元提醒线"), "{shown}");
        assert!(shown.contains("提醒不会改变采集、额度或套餐"), "{shown}");
        // Undated history is stated instead of being folded into this month.
        assert!(shown.contains("另有 2 次旧记录"), "{shown}");
        assert!(shown.contains("未计入本月"), "{shown}");

        // The application raises this one-shot notice; the panel renders its full
        // promise next to the numbers, so it can never read as a stop or a charge.
        state.notice = Some(listen::Notice::UsageReminder);
        let shown = status_text(&state);
        assert!(shown.contains("这只是提醒"), "{shown}");
        assert!(shown.contains("照常继续"), "{shown}");
        assert!(shown.contains("不会自动停用"), "{shown}");
        assert!(shown.contains("取消当前录音"), "{shown}");
        assert!(shown.contains("购买套餐"), "{shown}");
        assert!(shown.contains("提高额度"), "{shown}");
    }

    #[test]
    fn a_zero_cash_request_with_unverified_usage_is_not_shown_as_no_spend() {
        // The free-only policy reserves zero cash, but neither the usage nor the
        // actual bill is verified: the panel must not read as "nothing was spent".
        let mut state = snapshot(Phase::Result, Preparation::Ready);
        state.usage = Some(listen::UsageSummary {
            requests: 1,
            settled_requests: 1,
            confirmed_nanoyuan: 0,
            estimated_nanoyuan: 0,
            unknown_requests: 1,
            retried_segments: 0,
            undated_requests: 0,
            month_confirmed_nanoyuan: 0,
            month_estimated_nanoyuan: 0,
            month_unknown_requests: 1,
            trial_cap_nanoyuan: 50_000_000_000,
            confirmed_input_tokens: 0,
            confirmed_output_tokens: 0,
            unreadable: false,
        });
        let shown = status_text(&state);
        assert!(shown.contains("未知 1 次"), "{shown}");
        assert!(shown.contains("不按 0 元计"), "{shown}");
        assert!(
            shown.contains("累计费用估计 0.00 元 = 已确认 0.00 元 + 保守预留 0.00 元"),
            "{shown}"
        );
    }

    #[test]
    fn an_unreadable_usage_ledger_is_not_reported_as_zero() {
        let mut state = snapshot(Phase::Ready, Preparation::Ready);
        state.usage = Some(listen::UsageSummary::unreadable(50_000_000_000));
        let shown = status_text(&state);
        assert!(shown.contains("无法读取"), "{shown}");
        assert!(shown.contains("不能当作 0 元"), "{shown}");
        assert!(!shown.contains("累计费用估计 0.00 元"), "{shown}");
    }

    #[test]
    fn auto_open_fires_only_for_a_fallback_result() {
        assert_eq!(
            auto_open_key(&result("完整文本", Delivery::Appended)),
            None,
            "a successful append must not steal the screen"
        );
        assert!(
            auto_open_key(&result("完整文本", Delivery::Fallback)).is_some(),
            "a fallback result must be reachable immediately"
        );

        // Issue #20 asks for a click-to-toggle panel; story 5 only wants the
        // no-target fallback to be reachable without discovery. A failure already
        // turns the capsule red, and the user opens the panel to read why.
        let mut failure = snapshot(Phase::Failure, Preparation::Ready);
        failure.detail = "Recognition failed".into();
        assert_eq!(
            auto_open_key(&failure),
            None,
            "a failure must not open the panel by itself"
        );
        failure.retry_available = true;
        assert_eq!(auto_open_key(&failure), None);

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
    fn every_new_fallback_result_produces_a_new_auto_open_key() {
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

    fn segment(
        sequence: u64,
        state: SegmentState,
        text: &str,
        delivery: Option<Delivery>,
    ) -> SegmentSummary {
        SegmentSummary {
            sequence,
            state,
            detail: String::new(),
            text: text.into(),
            delivery,
            usage: None,
        }
    }

    #[test]
    fn a_multi_segment_panel_counts_waiting_segments_and_keeps_every_pending_result_in_full() {
        let mut state = snapshot(Phase::Capturing, Preparation::Ready);
        state.detail = "Capturing the default multimedia endpoint.".into();
        let long = "第一段很长很长的完整结果。".repeat(400);
        state.segments = vec![
            segment(1, SegmentState::Result, &long, Some(Delivery::Fallback)),
            segment(2, SegmentState::Processing, "", None),
            segment(3, SegmentState::Capturing, "", None),
        ];
        let status = status_text(&state);
        assert!(status.contains("多段记录"), "{status}");
        assert!(
            status.contains("等待 1 段，待取结果 1 段"),
            "the panel distinguishes the older waiting segment from the pending result: {status}"
        );
        let first = status.find("第 1 段").expect("first segment line");
        let second = status.find("第 2 段").expect("second segment line");
        let third = status.find("第 3 段").expect("third segment line");
        assert!(first < second && second < third, "{status}");

        let shown = result_text(&state);
        assert!(
            shown.contains(&long),
            "a pending result must be shown complete, not truncated"
        );
        assert!(
            shown.contains("【第 1 段 · 待取结果；可手动复制】"),
            "{shown}"
        );
        assert!(!shown.contains("【第 2 段"), "{shown}");
    }

    #[test]
    fn pending_results_stay_in_capture_order_and_never_replace_the_current_result() {
        let mut state = result("第三段当前结果", Delivery::Appended);
        state.segments = vec![
            segment(
                1,
                SegmentState::Result,
                "第一段待取结果",
                Some(Delivery::Fallback),
            ),
            segment(
                2,
                SegmentState::Result,
                "第二段待取结果",
                Some(Delivery::Fallback),
            ),
            segment(
                3,
                SegmentState::Result,
                "第三段当前结果",
                Some(Delivery::Appended),
            ),
        ];
        let shown = result_text(&state);
        assert!(shown.starts_with("第三段当前结果"), "{shown}");
        let first = shown.find("第一段待取结果").expect("first pending");
        let second = shown.find("第二段待取结果").expect("second pending");
        assert!(first < second, "older pending results come first: {shown}");
        assert_eq!(state.text, "第三段当前结果");

        let pending = pending_results(&state);
        assert_eq!(
            pending
                .iter()
                .map(|segment| segment.sequence)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        let status = status_text(&state);
        assert!(status.contains("待取结果 2 段"), "{status}");
    }

    #[test]
    fn a_single_delivered_capture_reads_exactly_as_before() {
        let mut state = result("单段完整结果", Delivery::Appended);
        state.segments = vec![segment(
            1,
            SegmentState::Result,
            "单段完整结果",
            Some(Delivery::Appended),
        )];
        assert_eq!(segments_note(&state), None);
        assert!(!status_text(&state).contains("多段记录"));
        assert_eq!(result_text(&state), "单段完整结果");
    }

    fn storage(
        pending: u64,
        free: Option<u64>,
        fault: Option<&str>,
    ) -> listen::storage::StorageStatus {
        listen::storage::StorageStatus {
            pending_audio_bytes: pending,
            budget_bytes: 1_000,
            warning_at_bytes: 800,
            free_disk_bytes: free,
            free_disk_floor_bytes: 500,
            text_bytes: 0,
            text_budget_bytes: 4_000,
            fault: fault.map(str::to_string),
        }
    }

    #[test]
    fn storage_lines_warn_before_the_limit_and_name_both_hard_stops_and_a_real_fault() {
        let mut state = snapshot(Phase::Ready, Preparation::Ready);
        assert_eq!(storage_note(&state), None, "a normal measurement is quiet");
        state.storage = Some(storage(100, Some(5_000), None));
        assert_eq!(storage_note(&state), None);

        state.storage = Some(storage(850, Some(5_000), None));
        let warning = storage_note(&state).expect("a warning line");
        assert!(warning.contains("接近待处理音频上限"), "{warning}");
        assert!(warning.contains("停止开始新的采集"), "{warning}");

        state.storage = Some(storage(1_000, Some(5_000), None));
        let full = storage_note(&state).expect("a full line");
        assert!(full.contains("已达上限"), "{full}");
        assert!(full.contains("已经收到的声音和旧段都保留"), "{full}");

        state.storage = Some(storage(10, Some(500), None));
        let disk_low = storage_note(&state).expect("a disk line");
        assert!(disk_low.contains("磁盘"), "{disk_low}");
        assert!(disk_low.contains("低于"), "{disk_low}");
        assert!(disk_low.contains("不是联网等待"), "{disk_low}");

        state.storage = Some(storage(
            10,
            Some(5_000),
            Some("Storage fault: Could not save the complete result (disk full)."),
        ));
        let faulted = storage_note(&state).expect("a fault line even at a normal level");
        assert!(faulted.contains("存储写入失败"), "{faulted}");
        assert!(faulted.contains("不是联网等待"), "{faulted}");
        assert!(faulted.contains("播放设备中断"), "{faulted}");
    }

    #[test]
    fn only_segments_that_still_have_pending_work_are_offered_for_explicit_deletion() {
        let mut state = snapshot(Phase::Capturing, Preparation::Ready);
        state.segments = vec![
            segment(1, SegmentState::Result, "已交付", Some(Delivery::Appended)),
            segment(2, SegmentState::Result, "待取", Some(Delivery::Fallback)),
            segment(3, SegmentState::WaitingForNetwork, "", None),
            segment(4, SegmentState::Processing, "", None),
            segment(5, SegmentState::Failed, "", None),
            segment(6, SegmentState::NoSpeech, "", None),
            segment(7, SegmentState::Capturing, "", None),
        ];
        assert_eq!(deletable_segments(&state), vec![2, 3, 4, 5]);

        // The cleanup hint only appears where it can act.
        state.storage = Some(storage(850, Some(5_000), None));
        let note = storage_note(&state).expect("a warning line");
        assert!(note.contains("delete-segment:"), "{note}");
        let no_pending = snapshot(Phase::Result, Preparation::Ready);
        assert!(deletable_segments(&no_pending).is_empty());
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

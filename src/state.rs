use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};

use serde::{Deserialize, Serialize};

use crate::persist;

/// 本插件服务手环 10/11 与 9 Pro 两个游戏包；启动时自动注册这两个包名的 interconnect 接收回调。
pub const TARGET_PKG: &str = "com.pkr.band1011";
pub const HANDHELD_PKG: &str = "com.pkr.handheld";

/// 旧占位包名，仅在信封缺少 pkgName 时作为兜底提示。
pub const HISTORICAL_PKG_NAME: &str = TARGET_PKG;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppEntry {
    pub pkg_name: String,
    pub enabled: bool,
    #[serde(default)]
    pub request_count: u64,
    #[serde(default)]
    pub success_count: u64,
    #[serde(default)]
    pub error_count: u64,
    #[serde(default)]
    pub last_seen_unix_ms: Option<u128>,
    #[serde(default)]
    pub last_addr: Option<String>,
    #[serde(default)]
    pub last_status: Option<String>,
    #[serde(default)]
    pub last_url: Option<String>,
    #[serde(default)]
    pub synced_files: Vec<String>,
}

impl AppEntry {
    pub fn new(pkg_name: &str) -> Self {
        Self {
            pkg_name: pkg_name.to_string(),
            enabled: true,
            request_count: 0,
            success_count: 0,
            error_count: 0,
            last_seen_unix_ms: None,
            last_addr: None,
            last_status: None,
            last_url: None,
            synced_files: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct InstalledApp {
    pub addr: String,
    pub device_name: String,
    pub package_name: String,
    pub app_name: String,
    pub version_code: u32,
}

/// 主动下发到手表的命令（备份 / 恢复）执行状态。借鉴可靠同步的回执模型：
/// 命令发出只是 pending，必须等手表落地后回 ack 才算 Ok / Failed。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandStatus {
    Pending,
    Ok,
    Failed,
}

#[derive(Clone, Debug)]
pub struct PendingCommand {
    pub cmd_id: String,
    pub action: String,
    pub status: CommandStatus,
    pub message: String,
    pub sent_unix_ms: u128,
}

/// 等待手表回执超过该时长仍未回 ack，按失败兜底，避免 UI 一直停在「等待」。
pub const COMMAND_ACK_TIMEOUT_MS: u128 = 15_000;

/// 手表通过独立状态通道(tag=pkr-stat)推来的实时进度：不经过文件传输队列，
/// 几十字节即时到达，用于「正在传哪张 / 已就绪几张」的实时展示。
#[derive(Clone, Debug, Default)]
pub struct LiveStatus {
    pub ready: usize,
    pub total: usize,
    pub cur: String,
    pub busy: bool,
    pub ts_ms: u128,
}

#[derive(Default)]
pub struct PluginState {
    pub apps: HashMap<String, AppEntry>,
    pub root_element_id: Option<String>,
    pub last_notice: Option<String>,
    pub pending_add_pkg: String,
    /// Cached list of (addr, name) for connected devices, refreshed lazily.
    pub connected_devices: Vec<(String, String)>,
    /// All third-party apps the host currently reports installed on each
    /// connected device. Refreshed lazily during UI render.
    pub installed_apps: Vec<InstalledApp>,
    /// Unix-ms timestamp of the last auto-refresh cycle. Used to throttle
    /// repeated render-driven refreshes so rapid UI interactions don't spam
    /// the host.
    pub last_auto_refresh_ms: u128,
    /// Sequence number to make sure UI re-renders reflect outside changes.
    pub render_tick: u64,
    /// 主动下发命令的登记表：cmdId → 命令执行状态，等手表 ack 回填。
    pub pending_commands: HashMap<String, PendingCommand>,
    /// 命令 ID 自增序列。
    pub cmd_seq: u64,
    /// 手表独立通道推来的实时图集进度（最新一条）。
    pub live: Option<LiveStatus>,
    /// 在线榜缓存（Upstash），由定时任务刷新；UI 只读，绝不阻塞渲染。
    pub cloud_classic: Vec<crate::scores::ScoreEntry>,
    pub cloud_endless: Vec<crate::scores::ScoreEntry>,
    pub cloud_fastest: Vec<crate::scores::ScoreEntry>,
    /// 上次在线榜刷新时刻（unix ms）。0 表示立即需要刷新。
    pub cloud_ms: u128,
    pub cloud_err: Option<String>,
    /// 上次自动让手表补传积压成绩（score-flush）的时刻，unix ms；0 表示还没跑过。
    pub auto_flush_ms: u128,
    /// 昵称输入框里正在编辑的文字（未落盘）。保存时写进 scores 的昵称文件。
    pub nick_draft: String,
    /// 排行榜分榜视图：0 在线 / 1 本机。
    pub board_source: u8,
    /// 榜种：0 经典最高层 / 1 无尽最高层 / 2 最快通关。
    pub board_kind: u8,
    /// 正在编辑昵称：编辑期间整页停止重渲染，避免重建输入框打断中文输入法。
    pub nick_editing: bool,
}

pub fn nick_editing() -> bool {
    with_state(|s| s.nick_editing)
}

pub fn set_nick_editing(on: bool) {
    with_state(|s| s.nick_editing = on);
}

pub fn board_view() -> (u8, u8) {
    with_state(|s| (s.board_source, s.board_kind))
}

pub fn set_board_view(source: Option<u8>, kind: Option<u8>) {
    with_state(|s| {
        if let Some(v) = source {
            s.board_source = v.min(1);
        }
        if let Some(v) = kind {
            s.board_kind = v.min(2);
        }
    });
}

/// 在线榜缓存快照（clone 出去，避免渲染时长时间持锁）。
pub fn cloud_snapshot() -> (Vec<crate::scores::ScoreEntry>, Vec<crate::scores::ScoreEntry>, Vec<crate::scores::ScoreEntry>, u128, Option<String>) {
    with_state(|s| {
        (
            s.cloud_classic.clone(),
            s.cloud_endless.clone(),
            s.cloud_fastest.clone(),
            s.cloud_ms,
            s.cloud_err.clone(),
        )
    })
}

pub fn cloud_store(
    classic: Vec<crate::scores::ScoreEntry>,
    endless: Vec<crate::scores::ScoreEntry>,
    fastest: Vec<crate::scores::ScoreEntry>,
    err: Option<String>,
) {
    let ts = now_unix_ms();
    with_state(|s| {
        if err.is_none() {
            s.cloud_classic = classic;
            s.cloud_endless = endless;
            s.cloud_fastest = fastest;
        }
        s.cloud_err = err;
        s.cloud_ms = ts;
    });
}

/// 在线榜是否到了该刷新的时候（首次 / 超过 60s / 被新成绩标记为脏）。
pub fn cloud_due() -> bool {
    with_state(|s| s.cloud_ms == 0 || now_unix_ms().saturating_sub(s.cloud_ms) > 60_000)
}

/// 有新成绩上报时把刷新时刻清零，让下一拍定时任务立刻拉最新榜。
pub fn cloud_invalidate() {
    with_state(|s| s.cloud_ms = 0);
}

/// 是否到了该自动让手表补传一次积压成绩的时候（首次 / 距上次超过 120s）。
pub fn auto_flush_due() -> bool {
    with_state(|s| s.auto_flush_ms == 0 || now_unix_ms().saturating_sub(s.auto_flush_ms) > 120_000)
}

/// 记下本次自动补传时刻，作为节流基准。
pub fn mark_auto_flush() {
    with_state(|s| s.auto_flush_ms = now_unix_ms());
}

static STATE: OnceLock<Mutex<PluginState>> = OnceLock::new();

pub fn with_state<R>(f: impl FnOnce(&mut PluginState) -> R) -> R {
    let mutex = STATE.get_or_init(|| Mutex::new(PluginState::default()));
    let mut guard = mutex.lock().unwrap_or_else(|p| p.into_inner());
    f(&mut guard)
}

pub fn ensure_app(pkg_name: &str) {
    let inserted = with_state(|state| {
        let before = state.apps.contains_key(pkg_name);
        state
            .apps
            .entry(pkg_name.to_string())
            .or_insert_with(|| AppEntry::new(pkg_name));
        !before
    });
    if inserted {
        persist_now();
    }
}

/// Bulk-load app entries from disk. Used at startup before any UI render.
pub fn install_loaded_apps(entries: Vec<AppEntry>) {
    with_state(|state| {
        for entry in entries {
            state.apps.insert(entry.pkg_name.clone(), entry);
        }
        state.render_tick = state.render_tick.wrapping_add(1);
    });
}

pub fn is_enabled(pkg_name: &str) -> bool {
    with_state(|state| state.apps.get(pkg_name).map(|e| e.enabled).unwrap_or(true))
}

pub fn record_request(pkg_name: &str, addr: &str, url: Option<&str>) {
    let now_ms = now_unix_ms();
    with_state(|state| {
        let entry = state
            .apps
            .entry(pkg_name.to_string())
            .or_insert_with(|| AppEntry::new(pkg_name));
        entry.request_count = entry.request_count.saturating_add(1);
        entry.last_seen_unix_ms = Some(now_ms);
        entry.last_addr = Some(addr.to_string());
        if let Some(url) = url {
            entry.last_url = Some(url.to_string());
        }
        state.render_tick = state.render_tick.wrapping_add(1);
    });
    persist_now();
}

pub fn record_result(pkg_name: &str, ok: bool, status: Option<String>) {
    with_state(|state| {
        let entry = state
            .apps
            .entry(pkg_name.to_string())
            .or_insert_with(|| AppEntry::new(pkg_name));
        if ok {
            entry.success_count = entry.success_count.saturating_add(1);
        } else {
            entry.error_count = entry.error_count.saturating_add(1);
        }
        if let Some(status) = status {
            entry.last_status = Some(status);
        }
        state.render_tick = state.render_tick.wrapping_add(1);
    });
    persist_now();
}

pub fn record_synced_file(pkg_name: &str, file: &str) {
    if file.is_empty() {
        return;
    }
    with_state(|state| {
        let entry = state
            .apps
            .entry(pkg_name.to_string())
            .or_insert_with(|| AppEntry::new(pkg_name));
        if !entry.synced_files.iter().any(|existing| existing == file) {
            entry.synced_files.push(file.to_string());
        }
        state.render_tick = state.render_tick.wrapping_add(1);
    });
    persist_now();
}

/// 用手表端全量校验后的已有图集清单**覆盖**该包的已同步记录。
///
/// 与单张 `record_synced_file` 的增量追加不同：全量清单以手表沙箱真实落盘为准，
/// 因此先清空旧记录再填入上报清单，纠正「插件数据被清但手表图还在」或「手表图被
/// 删但插件仍记旧记录」两类偏差。文件名由调用方完成 basename 与安全校验。
pub fn replace_synced_files(pkg_name: &str, files: Vec<String>) {
    with_state(|state| {
        let entry = state
            .apps
            .entry(pkg_name.to_string())
            .or_insert_with(|| AppEntry::new(pkg_name));
        entry.synced_files = files;
        // 全量清单是手表沙箱的权威快照：它一到就说明当前真实就绪状态已知，
        // 历史累计失败数（只增不清）必须一起归零，否则界面会永远挂着旧失败。
        entry.error_count = 0;
        state.render_tick = state.render_tick.wrapping_add(1);
    });
    persist_now();
}

/// 跨所有受监控包合并去重后的已有图集文件名清单（basename），供界面逐张标绿/灰。
/// 独立状态通道更新：仅运行时展示用，不落盘（重启后由手表 5s 心跳重推）。
pub fn set_live_status(ready: usize, total: usize, cur: String, busy: bool) {
    let now_ms = now_unix_ms();
    with_state(|state| {
        state.live = Some(LiveStatus {
            ready,
            total,
            cur,
            busy,
            ts_ms: now_ms,
        });
        state.render_tick = state.render_tick.wrapping_add(1);
    });
}

pub fn live_status() -> Option<LiveStatus> {
    with_state(|state| state.live.clone())
}

pub fn all_synced_files() -> Vec<String> {
    with_state(|state| {
        let mut set: std::collections::HashSet<String> = std::collections::HashSet::new();
        for entry in state.apps.values() {
            for file in &entry.synced_files {
                set.insert(file.clone());
            }
        }
        let mut list: Vec<String> = set.into_iter().collect();
        list.sort();
        list
    })
}

pub fn synced_file_count() -> u64 {
    with_state(|state| {
        state
            .apps
            .values()
            .map(|entry| entry.synced_files.len() as u64)
            .sum()
    })
}

/// Snapshot all monitored apps and ask the persist layer to flush them to
/// disk. Called whenever the monitored-app set changes.
pub fn persist_now() {
    let entries = snapshot_apps();
    persist::save_apps(&entries);
}

pub fn set_enabled(pkg_name: &str, enabled: bool) {
    with_state(|state| {
        let entry = state
            .apps
            .entry(pkg_name.to_string())
            .or_insert_with(|| AppEntry::new(pkg_name));
        entry.enabled = enabled;
        state.render_tick = state.render_tick.wrapping_add(1);
    });
    persist_now();
}

pub fn remove_app(pkg_name: &str) {
    let removed = with_state(|state| {
        let removed = state.apps.remove(pkg_name).is_some();
        state.render_tick = state.render_tick.wrapping_add(1);
        removed
    });
    if removed {
        persist_now();
    }
}

pub fn snapshot_apps() -> Vec<AppEntry> {
    with_state(|state| {
        let mut list: Vec<AppEntry> = state.apps.values().cloned().collect();
        list.sort_by(|a, b| a.pkg_name.cmp(&b.pkg_name));
        list
    })
}

pub fn pkg_names() -> Vec<String> {
    with_state(|state| {
        let mut list: Vec<String> = state.apps.keys().cloned().collect();
        list.sort();
        list
    })
}

pub fn set_notice(msg: impl Into<String>) {
    with_state(|state| {
        state.last_notice = Some(msg.into());
        state.render_tick = state.render_tick.wrapping_add(1);
    });
}

pub fn clear_notice() {
    with_state(|state| {
        state.last_notice = None;
    });
}

pub fn set_pending_add(value: String) {
    with_state(|state| state.pending_add_pkg = value);
}

pub fn take_pending_add() -> String {
    with_state(|state| std::mem::take(&mut state.pending_add_pkg))
}

pub fn set_root(element_id: &str) {
    with_state(|state| state.root_element_id = Some(element_id.to_string()));
}

pub fn root() -> Option<String> {
    with_state(|state| state.root_element_id.clone())
}

pub fn set_connected_devices(devices: Vec<(String, String)>) {
    with_state(|state| state.connected_devices = devices);
}

pub fn connected_devices() -> Vec<(String, String)> {
    with_state(|state| state.connected_devices.clone())
}

pub fn device_name_for(addr: &str) -> String {
    with_state(|state| {
        state
            .connected_devices
            .iter()
            .find(|(existing, _)| existing == addr)
            .map(|(_, name)| name.clone())
            .unwrap_or_default()
    })
}

pub fn set_installed_apps(apps: Vec<InstalledApp>) {
    with_state(|state| state.installed_apps = apps);
}

pub fn installed_apps() -> Vec<InstalledApp> {
    with_state(|state| state.installed_apps.clone())
}

pub fn transfer_progress() -> (u64, u64, Option<String>) {
    with_state(|state| {
        let mut success = 0u64;
        let mut error = 0u64;
        let mut last_url = None;
        for entry in state.apps.values() {
            success = success.saturating_add(entry.success_count);
            error = error.saturating_add(entry.error_count);
            if entry.last_url.is_some() {
                last_url = entry.last_url.clone();
            }
        }
        (success, error, last_url)
    })
}

pub fn is_monitored(pkg: &str) -> bool {
    with_state(|state| state.apps.contains_key(pkg))
}

/// Returns `true` and updates the timestamp when the caller is allowed to run
/// an auto-refresh cycle. We throttle to once per `min_interval_ms` so rapid
/// UI interactions don't spam the host with device / app queries.
pub fn try_claim_auto_refresh(min_interval_ms: u128) -> bool {
    let now = now_unix_ms();
    with_state(|state| {
        if now.saturating_sub(state.last_auto_refresh_ms) < min_interval_ms {
            return false;
        }
        state.last_auto_refresh_ms = now;
        true
    })
}

pub fn first_device_addr_for(pkg_name: &str) -> Option<String> {
    with_state(|state| {
        if let Some(entry) = state.apps.get(pkg_name) {
            if let Some(addr) = entry.last_addr.clone() {
                return Some(addr);
            }
        }
        state.connected_devices.first().map(|(a, _)| a.clone())
    })
}

/// 返回一个 `(addr, pkg)` 绑定对：只采用某快应用在会话里真实上报过的地址
/// (`last_addr`)，避免把设备兜底地址安到另一个包名上导致主动下行被宿主拒绝。
/// 优先手持包，其次历史包；都没有在手会话时返回 `None`。
pub fn first_bound_target() -> Option<(String, String)> {
    with_state(|state| {
        for pkg in [HANDHELD_PKG, TARGET_PKG] {
            if let Some(entry) = state.apps.get(pkg) {
                if let Some(addr) = entry.last_addr.clone() {
                    return Some((addr, pkg.to_string()));
                }
            }
        }
        None
    })
}

/// 是否存在任一已建立互连会话（带真实地址）的受监控快应用。
pub fn has_active_session() -> bool {
    with_state(|state| {
        state
            .apps
            .values()
            .any(|entry| entry.last_addr.as_ref().map(|a| !a.is_empty()).unwrap_or(false))
    })
}

pub fn now_unix_ms() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// 生成下一个命令 ID（短序列，便于回执帧配对）。
pub fn next_cmd_id() -> String {
    with_state(|state| {
        state.cmd_seq = state.cmd_seq.wrapping_add(1);
        format!("c{}", state.cmd_seq)
    })
}

/// 登记一条待手表回执的命令。
pub fn register_pending_command(cmd_id: &str, action: &str) {
    with_state(|state| {
        state.pending_commands.insert(
            cmd_id.to_string(),
            PendingCommand {
                cmd_id: cmd_id.to_string(),
                action: action.to_string(),
                status: CommandStatus::Pending,
                message: String::new(),
                sent_unix_ms: now_unix_ms(),
            },
        );
        state.render_tick = state.render_tick.wrapping_add(1);
    });
}

/// 按 cmdId 回填手表回执。返回 `Some((action, ok))`；找不到对应命令返回 `None`。
pub fn resolve_command(cmd_id: &str, ok: bool, message: &str) -> Option<(String, bool)> {
    with_state(|state| {
        let entry = state.pending_commands.get_mut(cmd_id)?;
        entry.status = if ok {
            CommandStatus::Ok
        } else {
            CommandStatus::Failed
        };
        entry.message = message.to_string();
        state.render_tick = state.render_tick.wrapping_add(1);
        Some((entry.action.clone(), ok))
    })
}

/// 取最近一条命令（按发送时间），供 UI 展示回执状态。
pub fn latest_command() -> Option<PendingCommand> {
    with_state(|state| {
        state
            .pending_commands
            .values()
            .max_by_key(|c| c.sent_unix_ms)
            .cloned()
    })
}

/// 把超时仍未回执的 pending 命令置为失败兜底，避免界面一直停在「等待」。
/// 返回是否有状态变更（用于决定是否需要重渲染）。
pub fn prune_stale_commands() -> bool {
    let now = now_unix_ms();
    let mut changed = false;
    with_state(|state| {
        for entry in state.pending_commands.values_mut() {
            if entry.status == CommandStatus::Pending
                && now.saturating_sub(entry.sent_unix_ms) > COMMAND_ACK_TIMEOUT_MS
            {
                entry.status = CommandStatus::Failed;
                entry.message = "手表长时间未响应，请确认游戏在前台后重试".to_string();
                changed = true;
            }
        }
        if changed {
            state.render_tick = state.render_tick.wrapping_add(1);
        }
    });
    changed
}

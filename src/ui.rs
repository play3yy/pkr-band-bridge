use crate::astrobox::psys_host_v4::ui;
use crate::backup;
use crate::fetch;
use crate::interconnect;
use crate::state;

const EVENT_REFRESH_DEVICES: &str = "action:devices.refresh";
const EVENT_CLEAR_NOTICE: &str = "action:notice.clear";
const EVENT_BACKUP_NOW: &str = "action:backup.now";
const EVENT_SYNC_ATLAS: &str = "action:atlas.sync";
const EVENT_BACKUP_RESTORE_PREFIX: &str = "action:backup.restore:";
const EVENT_BACKUP_DELETE_PREFIX: &str = "action:backup.delete:";

// —— 配色：暗色卡片 + 精灵球暖红 / 琥珀金强调 ——
const TEXT_PRIMARY: &str = "#f4f4f5";
const TEXT_SECONDARY: &str = "#a1a1aa";
const TEXT_MUTED: &str = "#71717a";
const TEXT_ON_ACCENT: &str = "#ffffff";

const ACCENT: &str = "#f5a623";
const ACCENT_SOFT: &str = "#3a2f16";
const PRIMARY_BG: &str = "#e8542f";

const SUCCESS: &str = "#4ade80";
const SUCCESS_SOFT: &str = "#163824";
const DANGER: &str = "#f87171";

const CARD_BG: &str = "#1f1f23";
const CARD_BG_2: &str = "#28282d";
const BORDER: &str = "#303036";
const BAR_BG: &str = "#2c2c31";

pub async fn render_main_ui(element_id: &str) {
    state::set_root(element_id);
    rerender().await;
}

pub async fn ui_event_processor(_evtype: ui::Event, event_id: &str, _payload_raw: &str) {
    match event_id {
        EVENT_REFRESH_DEVICES => {
            interconnect::refresh_connected_devices().await;
            interconnect::refresh_installed_apps().await;
            let pkgs = state::pkg_names();
            for pkg in &pkgs {
                interconnect::register_for_all_devices(pkg).await;
            }
            let device_count = state::connected_devices().len();
            state::set_notice(format!("已刷新：连接 {} 台设备，等待游戏请求图集", device_count));
            render_without_auto_refresh();
        }
        EVENT_CLEAR_NOTICE => {
            state::clear_notice();
            rerender().await;
        }
        EVENT_BACKUP_NOW => {
            send_sync_command("backup", None).await;
        }
        EVENT_SYNC_ATLAS => {
            send_sync_command("sync-atlas", None).await;
        }
        other => {
            if let Some(index) = other.strip_prefix(EVENT_BACKUP_RESTORE_PREFIX) {
                if let Ok(idx) = index.parse::<usize>() {
                    restore_backup_by_index(idx).await;
                }
            } else if let Some(index) = other.strip_prefix(EVENT_BACKUP_DELETE_PREFIX) {
                if let Ok(idx) = index.parse::<usize>() {
                    delete_backup_by_index(idx).await;
                }
            }
        }
    }
}

pub async fn rerender() {
    interconnect::auto_refresh().await;
    render_without_auto_refresh();
}

pub fn render_without_auto_refresh() {
    let Some(root) = state::root() else {
        return;
    };
    ui::render(&root, build_root());
}

fn build_root() -> ui::Element {
    state::prune_stale_commands();
    let devices = state::connected_devices();
    let (success, error, last_url) = state::transfer_progress();
    let notice = command_or_notice();
    let notice_kind = current_notice_kind();

    let mut root = ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Column)
        .width_full()
        .padding(22)
        .gap(16);

    root = root.child(header_section(devices.len()));
    root = root.child(device_section(&devices));
    root = root.child(progress_section(success, error, last_url.as_deref()));
    root = root.child(backup_section());

    if let Some(text) = notice {
        root = root.child(notice_line(&text, notice_kind));
    }

    root
}

// —— 头部品牌区 ——

fn header_section(device_count: usize) -> ui::Element {
    let title = ui::Element::new(ui::ElementType::P, Some("宝可梦肉鸽"))
        .size(22)
        .text_color(TEXT_PRIMARY);
    let subtitle = ui::Element::new(ui::ElementType::P, Some("同步器 · 图集与本地进度"))
        .size(12)
        .text_color(TEXT_SECONDARY);

    let brand = ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Column)
        .gap(4)
        .child(title)
        .child(subtitle);

    let status_badge = status_badge(device_count);

    ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Row)
        .align_center()
        .justify_start()
        .gap(12)
        .child(brand)
        .child(
            ui::Element::new(ui::ElementType::Div, None)
                .flex_grow(1.0)
                .flex(),
        )
        .child(status_badge)
}

fn status_badge(device_count: usize) -> ui::Element {
    let (text, dot_color, soft_bg) = if device_count == 0 {
        ("未连接", TEXT_MUTED, CARD_BG_2)
    } else {
        ("在线", SUCCESS, SUCCESS_SOFT)
    };

    let dot = ui::Element::new(ui::ElementType::Div, None)
        .width(8)
        .height(8)
        .radius(4)
        .bg(dot_color);

    let label = ui::Element::new(ui::ElementType::P, Some(text))
        .size(12)
        .text_color(if device_count == 0 { TEXT_MUTED } else { SUCCESS });

    ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Row)
        .align_center()
        .gap(6)
        .bg(soft_bg)
        .radius(999)
        .padding_top(5)
        .padding_bottom(5)
        .padding_left(12)
        .padding_right(12)
        .child(dot)
        .child(label)
}

// —— 通用卡片 ——

fn card() -> ui::Element {
    ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Column)
        .width_full()
        .gap(12)
        .bg(CARD_BG)
        .radius(16)
        .border(1, BORDER)
        .padding(16)
}

fn card_title(text: &str) -> ui::Element {
    let bar = ui::Element::new(ui::ElementType::Div, None)
        .width(4)
        .height(16)
        .radius(2)
        .bg(ACCENT);

    let label = ui::Element::new(ui::ElementType::P, Some(text))
        .size(15)
        .text_color(TEXT_PRIMARY);

    ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Row)
        .align_center()
        .gap(8)
        .child(bar)
        .child(label)
}

// —— 设备区 ——

fn device_section(devices: &[(String, String)]) -> ui::Element {
    let refresh = ghost_button("刷新").on(ui::Event::Click, EVENT_REFRESH_DEVICES);

    let head = ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Row)
        .align_center()
        .gap(8)
        .child(card_title("设备连接"))
        .child(ui::Element::new(ui::ElementType::Div, None).flex_grow(1.0).flex())
        .child(refresh);

    let mut col = card().child(head);

    if devices.is_empty() {
        col = col.child(hint("还没有连接的设备。在 AstroBox 里连上手表后点「刷新」。"));
    } else {
        for (addr, name) in devices {
            col = col.child(device_row(name, addr));
        }
    }

    col
}

fn device_row(name: &str, addr: &str) -> ui::Element {
    let name_el = ui::Element::new(
        ui::ElementType::P,
        Some(if name.is_empty() { "未命名设备" } else { name }),
    )
    .size(14)
    .text_color(TEXT_PRIMARY);

    let addr_el = ui::Element::new(ui::ElementType::P, Some(addr))
        .size(12)
        .text_color(TEXT_MUTED);

    ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Column)
        .gap(3)
        .bg(CARD_BG_2)
        .radius(12)
        .padding(12)
        .child(name_el)
        .child(addr_el)
}

// —— 图集进度区 ——

fn progress_section(_success: u64, _error: u64, last_url: Option<&str>) -> ui::Element {
    let synced: std::collections::HashSet<String> = state::all_synced_files().into_iter().collect();
    let groups = fetch::atlas_groups();
    let cfg_total: usize = groups.iter().map(|(_, files)| files.len()).sum();

    let live = state::live_status();
    let (ready, total, busy, cur) = match live.as_ref() {
        Some(l) if l.total > 0 => (
            l.ready.min(l.total),
            l.total,
            l.busy,
            l.cur.clone(),
        ),
        _ => (synced.len().min(cfg_total), cfg_total, false, String::new()),
    };

    let percent = if total == 0 {
        0
    } else {
        ((ready as f32 / total as f32) * 100.0).round() as u32
    };
    let pending = total.saturating_sub(ready);

    let summary = ui::Element::new(
        ui::ElementType::P,
        Some(&format!("已就绪 {}/{}", ready, total)),
    )
    .size(15)
    .text_color(TEXT_PRIMARY);

    let pending_el = ui::Element::new(
        ui::ElementType::P,
        Some(&if pending > 0 { format!("待补 {}", pending) } else { "全部就绪".to_string() }),
    )
    .size(12)
    .text_color(if pending > 0 { TEXT_SECONDARY } else { SUCCESS });

    let percent_badge = ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .align_center()
        .bg(ACCENT_SOFT)
        .radius(999)
        .padding_top(5)
        .padding_bottom(5)
        .padding_left(12)
        .padding_right(12)
        .child(
            ui::Element::new(ui::ElementType::P, Some(&format!("{}%", percent)))
                .size(13)
                .text_color(ACCENT),
        );

    let head = ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Row)
        .align_center()
        .gap(8)
        .child(card_title("图集进度"))
        .child(ui::Element::new(ui::ElementType::Div, None).flex_grow(1.0).flex())
        .child(percent_badge);

    let summary_row = ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Row)
        .align_center()
        .gap(10)
        .child(summary)
        .child(pending_el);

    let bar_track = build_bar(ready, total);

    let last_text = if busy && !cur.is_empty() {
        format!("正在传输：{}", cur)
    } else if !cur.is_empty() {
        format!("最近：{}", cur)
    } else {
        format!(
            "最近：{}",
            last_url
                .map(file_name_from_url)
                .unwrap_or_else(|| "等待游戏请求图集".to_string())
        )
    };
    let last_el = ui::Element::new(ui::ElementType::P, Some(&last_text))
        .size(12)
        .text_color(TEXT_MUTED);

    let sync_btn = secondary_button("同步图集").on(ui::Event::Click, EVENT_SYNC_ATLAS);
    let sync_row = ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Row)
        .child(sync_btn);

    let mut col = card()
        .child(head)
        .child(summary_row)
        .child(bar_track)
        .child(sync_row);

    for (label, files) in groups {
        col = col.child(atlas_group_row(label, files, &synced));
    }

    col.child(last_el)
}

fn build_bar(ready: usize, total: usize) -> ui::Element {
    let mut track = ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Row)
        .width_full()
        .height(10)
        .radius(5)
        .bg(BAR_BG);

    if total > 0 {
        let fill_w = ready as f32;
        let rest_w = total.saturating_sub(ready) as f32;
        if fill_w > 0.0 {
            track = track.child(
                ui::Element::new(ui::ElementType::Div, None)
                    .flex_grow(fill_w)
                    .height(10)
                    .radius(5)
                    .bg(ACCENT),
            );
        }
        if rest_w > 0.0 {
            track = track.child(
                ui::Element::new(ui::ElementType::Div, None)
                    .flex_grow(rest_w)
                    .height(10),
            );
        }
    }
    track
}

fn atlas_group_row(
    label: &str,
    files: &[&str],
    synced: &std::collections::HashSet<String>,
) -> ui::Element {
    let have = files.iter().filter(|file| synced.contains(**file)).count();
    let missing = files.len() - have;

    let name_el = ui::Element::new(ui::ElementType::P, Some(label))
        .size(13)
        .text_color(TEXT_PRIMARY);

    let count_el = ui::Element::new(
        ui::ElementType::P,
        Some(&format!("{}/{}", have, files.len())),
    )
    .size(12)
    .text_color(TEXT_SECONDARY);

    let head = ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Row)
        .align_center()
        .gap(10)
        .child(name_el)
        .child(ui::Element::new(ui::ElementType::Div, None).flex_grow(1.0).flex())
        .child(count_el);

    let mut col = ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Column)
        .gap(3)
        .child(head);

    if missing > 0 {
        col = col.child(
            ui::Element::new(ui::ElementType::P, Some(&format!("缺 {} 张", missing)))
                .size(11)
                .text_color(TEXT_MUTED),
        );
    }

    col
}

// —— 备份区 ——

fn backup_section() -> ui::Element {
    let backups = backup::list_backups(None);
    let mut col = card()
        .child(card_title("本地备份"))
        .child(hint("含精灵解锁、糖果、图鉴统计与历史记录；不含存档槽，保存在插件本地。"));

    let backup_button = primary_button("备份当前进度").on(ui::Event::Click, EVENT_BACKUP_NOW);
    col = col.child(
        ui::Element::new(ui::ElementType::Div, None)
            .flex()
            .flex_direction(ui::FlexDirection::Row)
            .child(backup_button),
    );

    if backups.is_empty() {
        col = col.child(hint("暂无备份。连接手表后点上面的按钮。"));
    } else {
        for (index, meta) in backups.iter().enumerate() {
            col = col.child(backup_row(index, meta));
        }
    }

    col
}

fn backup_row(index: usize, meta: &backup::BackupMeta) -> ui::Element {
    let title = ui::Element::new(
        ui::ElementType::P,
        Some(&format!("{} · {}", meta.device_name, format_time(meta.saved_at))),
    )
    .size(14)
    .text_color(TEXT_PRIMARY);

    let summary = ui::Element::new(
        ui::ElementType::P,
        Some(&format!(
            "解锁 {} · 糖果 {} · 历史 {}",
            meta.summary.unlocks, meta.summary.candies, meta.summary.history
        )),
    )
    .size(12)
    .text_color(TEXT_SECONDARY);

    let restore = text_button("恢复", ACCENT)
        .on(ui::Event::Click, &format!("{EVENT_BACKUP_RESTORE_PREFIX}{index}"));
    let delete = text_button("删除", TEXT_MUTED)
        .on(ui::Event::Click, &format!("{EVENT_BACKUP_DELETE_PREFIX}{index}"));

    let actions = ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Row)
        .gap(18)
        .child(restore)
        .child(delete);

    ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Column)
        .gap(6)
        .bg(CARD_BG_2)
        .radius(12)
        .padding(12)
        .child(title)
        .child(summary)
        .child(actions)
}

// —— 通知条 ——

enum NoticeKind {
    Pending,
    Ok,
    Failed,
    Plain,
}

fn current_notice_kind() -> NoticeKind {
    match state::latest_command() {
        Some(cmd) => match cmd.status {
            state::CommandStatus::Pending => NoticeKind::Pending,
            state::CommandStatus::Ok => NoticeKind::Ok,
            state::CommandStatus::Failed => NoticeKind::Failed,
        },
        None => NoticeKind::Plain,
    }
}

fn notice_line(text: &str, kind: NoticeKind) -> ui::Element {
    let (tone, soft_bg) = match kind {
        NoticeKind::Pending => (ACCENT, ACCENT_SOFT),
        NoticeKind::Ok => (SUCCESS, SUCCESS_SOFT),
        NoticeKind::Failed => (DANGER, "#3a1d1d"),
        NoticeKind::Plain => (TEXT_SECONDARY, CARD_BG_2),
    };

    let dot = ui::Element::new(ui::ElementType::Div, None)
        .width(8)
        .height(8)
        .radius(4)
        .bg(tone);

    let label = ui::Element::new(ui::ElementType::P, Some(text))
        .size(13)
        .text_color(tone)
        .flex_grow(1.0);

    let close_btn = text_button("收起", TEXT_MUTED).on(ui::Event::Click, EVENT_CLEAR_NOTICE);

    ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Row)
        .align_center()
        .gap(10)
        .bg(soft_bg)
        .radius(12)
        .padding(12)
        .child(dot)
        .child(label)
        .child(close_btn)
}

// —— 按钮与基础件 ——

fn primary_button(text: &str) -> ui::Element {
    ui::Element::new(ui::ElementType::Button, Some(text))
        .width_full()
        .padding_top(13)
        .padding_bottom(13)
        .radius(12)
        .bg(PRIMARY_BG)
        .text_color(TEXT_ON_ACCENT)
        .size(14)
}

fn ghost_button(text: &str) -> ui::Element {
    ui::Element::new(ui::ElementType::Button, Some(text))
        .without_default_styles()
        .border(1, BORDER)
        .radius(10)
        .padding_top(7)
        .padding_bottom(7)
        .padding_left(14)
        .padding_right(14)
        .text_color(TEXT_SECONDARY)
        .size(13)
}

fn secondary_button(text: &str) -> ui::Element {
    ui::Element::new(ui::ElementType::Button, Some(text))
        .width_full()
        .without_default_styles()
        .border(1, ACCENT)
        .radius(12)
        .padding_top(12)
        .padding_bottom(12)
        .text_color(ACCENT)
        .size(14)
}

fn text_button(text: &str, color: &str) -> ui::Element {
    ui::Element::new(ui::ElementType::Button, Some(text))
        .without_default_styles()
        .text_color(color)
        .size(13)
}

fn hint(text: &str) -> ui::Element {
    ui::Element::new(ui::ElementType::P, Some(text))
        .size(12)
        .text_color(TEXT_MUTED)
}

// —— 命令转发 ——

async fn send_sync_command(action: &str, backup_id: Option<&str>) {
    let result = interconnect::dispatch_watch_command(action, backup_id).await;
    let notice = match result {
        Ok(()) => match action {
            "sync-atlas" => "已拉起手表，正在同步图集…".to_string(),
            _ => "命令已发送，等待手表完成备份或恢复".to_string(),
        },
        Err(e) => e,
    };
    state::set_notice(notice);
    rerender().await;
}

async fn restore_backup_by_index(index: usize) {
    let backups = backup::list_backups(None);
    let Some(meta) = backups.get(index) else { return };
    send_sync_command("restore", Some(&meta.id)).await;
}

async fn delete_backup_by_index(index: usize) {
    let backups = backup::list_backups(None);
    let Some(meta) = backups.get(index) else { return };
    let deleted = backup::delete_backup(&meta.id);
    state::set_notice(if deleted { "本地备份已删除" } else { "备份不存在或已被删除" });
    rerender().await;
}

fn format_time(ms: u128) -> String {
    let total_secs = (ms / 1000) as i64;
    let day = total_secs / 86400;
    let secs = total_secs % 86400;
    let hour = secs / 3600;
    let minute = (secs % 3600) / 60;

    let mut year = 1970i64;
    let month_days = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut remaining_days = day;
    loop {
        let year_days = if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 { 366 } else { 365 };
        if remaining_days < year_days { break; }
        remaining_days -= year_days;
        year += 1;
    }
    let mut month = 1usize;
    for (index, &days) in month_days.iter().enumerate() {
        let limit = days + if month == 2 && ((year % 4 == 0 && year % 100 != 0) || year % 400 == 0) { 1 } else { 0 };
        if remaining_days < limit { month = index + 1; break; }
        remaining_days -= limit;
    }
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        year,
        month,
        remaining_days + 1,
        hour,
        minute
    )
}

fn file_name_from_url(url: &str) -> String {
    url.rsplit('/').next().unwrap_or(url).to_string()
}

fn state_notice() -> Option<String> {
    state::with_state(|s| s.last_notice.clone())
}

/// 提示优先反映最近一条备份/恢复命令的回执状态；没有命令时回落普通提示。
fn command_or_notice() -> Option<String> {
    if let Some(cmd) = state::latest_command() {
        return Some(match cmd.status {
            state::CommandStatus::Pending => match cmd.action.as_str() {
                "sync-atlas" => "正在同步图集…".to_string(),
                _ => "等待手表执行备份/恢复…".to_string(),
            },
            state::CommandStatus::Ok => match cmd.action.as_str() {
                "restore" => "恢复完成，进度已写回手表".to_string(),
                "sync-atlas" => "图集同步完成".to_string(),
                _ => "备份已保存到手机".to_string(),
            },
            state::CommandStatus::Failed => {
                if cmd.message.is_empty() {
                    "手表执行失败，请重试".to_string()
                } else {
                    cmd.message
                }
            }
        });
    }
    state_notice()
}

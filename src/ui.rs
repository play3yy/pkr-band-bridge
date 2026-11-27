use crate::astrobox::psys_host_v4::ui;
use crate::backup;
use crate::fetch;
use crate::interconnect;
use crate::state;

const EVENT_REFRESH_DEVICES: &str = "action:devices.refresh";
const EVENT_CLEAR_NOTICE: &str = "action:notice.clear";
const EVENT_BACKUP_NOW: &str = "action:backup.now";
const EVENT_SYNC_ATLAS: &str = "action:atlas.sync";
const EVENT_CLOUD_SYNC: &str = "action:cloud.sync";
const EVENT_NICK_INPUT: &str = "ui:nick.input";
const EVENT_NICK_SAVE: &str = "action:nick.save";
const EVENT_NICK_EDIT: &str = "action:nick.edit";
const EVENT_NICK_CANCEL: &str = "action:nick.cancel";
const EVENT_NICK_RANDOM: &str = "action:nick.random";
const EVENT_BOARD_SRC_PREFIX: &str = "action:board.src:";
const EVENT_BOARD_KIND_PREFIX: &str = "action:board.kind:";
const EVENT_BACKUP_RESTORE_PREFIX: &str = "action:backup.restore:";
const EVENT_BACKUP_DELETE_PREFIX: &str = "action:backup.delete:";

// —— 配色：暗色卡片 + 精灵球暖红 / 琥珀金强调 ——
const TEXT_PRIMARY: &str = "#f2f2f4";
const TEXT_SECONDARY: &str = "#9a9aa3";
const TEXT_MUTED: &str = "#6d6d77";

const ACCENT: &str = "#e9a83c";
const ACCENT_SOFT: &str = "#33280f";

const SUCCESS: &str = "#4ec98a";
const SUCCESS_SOFT: &str = "#12291f";
const DANGER: &str = "#f87171";

const CARD_BG: &str = "#15151a";
const CARD_BG_2: &str = "#1c1c22";
const BORDER: &str = "#25252c";
const BAR_BG: &str = "#26262d";

pub async fn render_main_ui(element_id: &str) {
    state::set_root(element_id);
    rerender().await;
}

pub async fn ui_event_processor(_evtype: ui::Event, event_id: &str, _payload_raw: &str) {
    // 昵称编辑期间：只放行输入与保存/取消，其它交互一律忽略。
    // 任何一次整树重建都会让输入框重新创建，中文输入法的候选组字随即中断。
    if state::nick_editing()
        && !matches!(
            event_id,
            EVENT_NICK_INPUT | EVENT_NICK_SAVE | EVENT_NICK_CANCEL
        )
    {
        return;
    }
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
        other if other.starts_with(EVENT_BOARD_SRC_PREFIX) => {
            if let Ok(v) = other[EVENT_BOARD_SRC_PREFIX.len()..].parse::<u8>() {
                state::set_board_view(Some(v), None);
                render_without_auto_refresh();
            }
        }
        other if other.starts_with(EVENT_BOARD_KIND_PREFIX) => {
            if let Ok(v) = other[EVENT_BOARD_KIND_PREFIX.len()..].parse::<u8>() {
                state::set_board_view(None, Some(v));
                render_without_auto_refresh();
            }
        }
        EVENT_NICK_INPUT => {
            // 每次敲键都来：只留在内存，不重渲染（重渲染会丢焦点）。
            let value = extract_event_value(_payload_raw);
            state::with_state(|s| s.nick_draft = value);
        }
        EVENT_NICK_EDIT => {
            state::with_state(|s| s.nick_draft = crate::scores::nickname());
            state::set_nick_editing(true);
            state::set_notice("编辑昵称：输入完点保存");
            render_without_auto_refresh();
        }
        EVENT_NICK_CANCEL => {
            state::set_nick_editing(false);
            state::with_state(|s| s.nick_draft = crate::scores::nickname());
            render_without_auto_refresh();
        }
        EVENT_NICK_SAVE => {
            let draft = state::with_state(|s| s.nick_draft.clone());
            let value = draft.trim().to_string();
            if value.is_empty() {
                state::set_notice("昵称不能为空");
            } else {
                crate::scores::set_nickname(&value);
                state::with_state(|s| s.nick_draft = crate::scores::nickname());
                state::set_nick_editing(false);
                let synced = crate::scores::push_nick_to_cloud().await;
                state::set_notice(if synced {
                    "昵称已保存，并同步到在线榜（重装插件也不会丢）"
                } else {
                    "昵称已保存到本机；接上手表后会同步到在线榜"
                });
                state::cloud_invalidate();
            }
            render_without_auto_refresh();
        }
        EVENT_NICK_RANDOM => {
            let name = crate::scores::random_nickname();
            crate::scores::set_nickname(&name);
            state::with_state(|s| s.nick_draft = name.clone());
            crate::scores::push_nick_to_cloud().await;
            state::set_notice(&format!("已设为「{name}」并同步到在线榜"));
            state::cloud_invalidate();
            render_without_auto_refresh();
        }
        EVENT_CLOUD_SYNC => {
            // 一次点击做两件事：让手表把积压成绩推上来，再把云端榜拉一遍。
            // waki 的 HTTP 是阻塞的，先渲染一帧「正在同步」让用户有反馈。
            state::set_notice("正在让手表补传成绩，并同步在线榜…");
            render_without_auto_refresh();

            let watch = interconnect::dispatch_watch_command("score-flush", None).await;
            let outcome = crate::scores::force_refresh_cloud().await;
            let notice = match (watch, outcome) {
                (Ok(()), Ok((classic, endless, fastest))) => {
                    state::cloud_store(classic, endless, fastest, None);
                    "已同步：手表正在补传成绩，在线榜已刷新".to_string()
                }
                (Ok(()), Err(err)) => {
                    state::cloud_store(Vec::new(), Vec::new(), Vec::new(), Some(err.clone()));
                    format!("手表补传已启动，但在线榜拉取失败：{err}")
                }
                (Err(watch_err), Ok((classic, endless, fastest))) => {
                    state::cloud_store(classic, endless, fastest, None);
                    format!("在线榜已刷新；手表未响应：{watch_err}")
                }
                (Err(watch_err), Err(err)) => {
                    state::cloud_store(Vec::new(), Vec::new(), Vec::new(), Some(err.clone()));
                    format!("手表未响应：{watch_err}；在线榜拉取失败：{err}")
                }
            };
            state::set_notice(notice);
            render_without_auto_refresh();
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
    // 昵称编辑中：一切自动刷新都暂停。整树重建会把输入框重建成已保存的值，
    // 中文输入法的候选组字会被打断——这是之前「输入法出问题」的根因。
    if state::nick_editing() {
        return;
    }
    crate::scores::maybe_refresh_cloud().await;
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
    root = root.child(leaderboard_section());
    root = root.child(backup_section());

    if let Some(text) = notice {
        root = root.child(notice_line(&text, notice_kind));
    }

    root
}

// —— 头部品牌区 ——

fn header_section(device_count: usize) -> ui::Element {
    let eyebrow = ui::Element::new(ui::ElementType::P, Some("POKÉROGUE · RW6"))
        .size(10)
        .text_color(TEXT_MUTED);
    let title = ui::Element::new(ui::ElementType::P, Some("宝可梦肉鸽"))
        .size(23)
        .text_color(TEXT_PRIMARY);
    let subtitle = ui::Element::new(ui::ElementType::P, Some("同步器 · 图集与本地进度"))
        .size(12)
        .text_color(TEXT_SECONDARY);

    let brand = ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Column)
        .gap(5)
        .child(eyebrow)
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
        .padding(14)
}

fn card_title(text: &str) -> ui::Element {
    ui::Element::new(ui::ElementType::P, Some(text))
        .size(15)
        .text_color(TEXT_PRIMARY)
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

    col = col.child(atlas_stats_grid(groups, &synced));

    col.child(last_el)
}

/// 图集分类统计：一行四格，数字在上、标签在下，比四条「名字 — 计数」更紧凑。
fn atlas_stats_grid(
    groups: &[(&'static str, &'static [&'static str])],
    synced: &std::collections::HashSet<String>,
) -> ui::Element {
    let mut row = ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Row)
        .width_full()
        .gap(8);

    for (label, files) in groups {
        let have = files.iter().filter(|f| synced.contains(**f)).count();
        let missing = files.len().saturating_sub(have);
        let value = ui::Element::new(ui::ElementType::P, Some(&have.to_string()))
            .size(15)
            .text_color(TEXT_PRIMARY);
        let caption = if missing > 0 {
            format!("{} 缺{}", label, missing)
        } else {
            label.to_string()
        };
        let label_el = ui::Element::new(ui::ElementType::P, Some(&caption))
            .size(10)
            .text_color(if missing > 0 { ACCENT } else { TEXT_MUTED });
        let cell = ui::Element::new(ui::ElementType::Div, None)
            .flex()
            .flex_direction(ui::FlexDirection::Column)
            .flex_grow(1.0)
            .gap(2)
            .bg(CARD_BG_2)
            .radius(10)
            .padding_top(9)
            .padding_bottom(9)
            .padding_left(10)
            .padding_right(6)
            .child(value)
            .child(label_el);
        row = row.child(cell);
    }
    row
}

fn build_bar(ready: usize, total: usize) -> ui::Element {
    let mut track = ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Row)
        .width_full()
        .height(5)
        .radius(3)
        .bg(BAR_BG);

    if total > 0 {
        let fill_w = ready as f32;
        let rest_w = total.saturating_sub(ready) as f32;
        if fill_w > 0.0 {
            track = track.child(
                ui::Element::new(ui::ElementType::Div, None)
                    .flex_grow(fill_w)
                    .height(5)
                    .radius(3)
                    .bg(ACCENT),
            );
        }
        if rest_w > 0.0 {
            track = track.child(
                ui::Element::new(ui::ElementType::Div, None)
                    .flex_grow(rest_w)
                    .height(5),
            );
        }
    }
    track
}

// —— 备份区 ——

fn backup_section() -> ui::Element {
    let backups = backup::list_backups(None);
    let head = ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Row)
        .align_center()
        .gap(8)
        .child(card_title("本地备份"))
        .child(ui::Element::new(ui::ElementType::Div, None).flex_grow(1.0).flex())
        .child(gold_button("立即备份").on(ui::Event::Click, EVENT_BACKUP_NOW));

    let mut col = card()
        .child(head)
        .child(hint("含精灵解锁、糖果、图鉴统计与历史记录；不含存档槽。"));

    if backups.is_empty() {
        col = col.child(hint("暂无备份。连上手表后点右上角「立即备份」。"));
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

// —— 排行榜 ——

#[derive(Clone, Copy, PartialEq, Eq)]
enum BoardKind {
    Floor,
    Duration,
}



/// 单只精灵：宿主 UI 只认 data: / http(s)，这里用离线生成的最短 data URL。
fn mon_cell(id: u32) -> ui::Element {
    match crate::mon_data::data_url(id) {
        Some(url) => ui::Element::new(ui::ElementType::Image, Some(url))
            .width(24)
            .height(24)
            .radius(4)
            .prop("src", url),
        None => ui::Element::new(ui::ElementType::Div, None)
            .width(24)
            .height(24)
            .radius(4)
            .bg(CARD_BG_2),
    }
}

fn leaderboard_section() -> ui::Element {
    let (cloud_classic, cloud_endless, cloud_fastest, cloud_ms, cloud_err) =
        crate::state::cloud_snapshot();
    let local_classic = crate::scores::board("classic", 5);
    let local_endless = crate::scores::board("endless", 5);
    let local_fast = crate::scores::fastest(3);
    let total = crate::scores::total_count();
    let (source, kind) = crate::state::board_view();

    // 两个维度分榜：数据源（在线/本机）× 榜种（经典/无尽/最快通关）。
    // 一次只渲染一个榜，最多 5 条 —— 三榜堆卡片会让页面长到失去重点。
    let use_cloud = source == 0;
    let (rows, board_kind, kind_caption) = match kind {
        0 => (
            if use_cloud { cloud_classic } else { local_classic },
            BoardKind::Floor,
            "经典 · 最高层",
        ),
        1 => (
            if use_cloud { cloud_endless } else { local_endless },
            BoardKind::Floor,
            "无尽 · 最高层",
        ),
        _ => (
            if use_cloud { cloud_fastest } else { local_fast },
            BoardKind::Duration,
            "经典 · 最快通关",
        ),
    };
    let rows: Vec<crate::scores::ScoreEntry> = rows.into_iter().take(5).collect();

    let head = ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Row)
        .align_center()
        .gap(8)
        .child(card_title("排行榜"))
        .child(ui::Element::new(ui::ElementType::Div, None).flex_grow(1.0).flex())
        .child(ghost_button("同步").on(ui::Event::Click, EVENT_CLOUD_SYNC));

    let mut col = card().child(head);

    // 昵称：输入框独占一行给满宽度（flex_grow 在输入框上会塌成 0 宽，点不进去）。
    // 昵称区：平时只显示一行 + 「改昵称」，点开才出现输入框。
    // 编辑期间 rerender() 直接返回，页面静止，输入法不受打扰。
    let nickname = crate::scores::nickname();
    let editing = state::nick_editing();
    let nick_block = if editing {
        let draft = state::with_state(|s| s.nick_draft.clone());
        let input = ui::Element::new(ui::ElementType::Input, None)
            .width_full()
            .padding_top(9)
            .padding_bottom(9)
            .padding_left(11)
            .padding_right(11)
            .radius(10)
            .border(1, BORDER)
            .bg(CARD_BG_2)
            .text_color(TEXT_PRIMARY)
            .size(13)
            .content(Some(&draft))
            .prop("placeholder", "输入昵称，最多 12 字")
            .prop("maxlength", "12")
            .prop("autocomplete", "off")
            .autofocus()
            .on(ui::Event::Input, EVENT_NICK_INPUT)
            .on(ui::Event::Change, EVENT_NICK_INPUT);
        let actions = ui::Element::new(ui::ElementType::Div, None)
            .flex()
            .flex_direction(ui::FlexDirection::Row)
            .align_center()
            .justify_end()
            .gap(8)
            .width_full()
            .child(ghost_button("取消").on(ui::Event::Click, EVENT_NICK_CANCEL))
            .child(gold_button("保存昵称").on(ui::Event::Click, EVENT_NICK_SAVE));
        ui::Element::new(ui::ElementType::Div, None)
            .flex()
            .flex_direction(ui::FlexDirection::Column)
            .width_full()
            .gap(8)
            .child(input)
            .child(actions)
    } else {
        let shown = if nickname.trim().is_empty() {
            "未设置".to_string()
        } else {
            nickname.clone()
        };
        let actions = ui::Element::new(ui::ElementType::Div, None)
            .flex()
            .flex_direction(ui::FlexDirection::Row)
            .align_center()
            .gap(8)
            .child(
                ui::Element::new(ui::ElementType::P, Some(&shown))
                    .size(13)
                    .text_color(if nickname.trim().is_empty() {
                        TEXT_MUTED
                    } else {
                        TEXT_PRIMARY
                    }),
            )
            .child(ui::Element::new(ui::ElementType::Div, None).flex_grow(1.0).flex())
            .child(ghost_button("随机").on(ui::Event::Click, EVENT_NICK_RANDOM))
            .child(ghost_button("改昵称").on(ui::Event::Click, EVENT_NICK_EDIT));
        ui::Element::new(ui::ElementType::Div, None)
            .flex()
            .flex_direction(ui::FlexDirection::Row)
            .width_full()
            .gap(8)
            .bg(CARD_BG_2)
            .radius(10)
            .padding_top(8)
            .padding_bottom(8)
            .padding_left(11)
            .padding_right(11)
            .child(
                ui::Element::new(ui::ElementType::P, Some("昵称"))
                    .size(11)
                    .text_color(TEXT_MUTED),
            )
            .child(actions)
    };
    col = col.child(nick_block);

    // 分段：在线 / 本机
    let source_seg = ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Row)
        .width_full()
        .gap(6)
        .child(seg_button(
            "在线 · 全球",
            use_cloud,
            &format!("{EVENT_BOARD_SRC_PREFIX}0"),
        ))
        .child(seg_button(
            "本机",
            !use_cloud,
            &format!("{EVENT_BOARD_SRC_PREFIX}1"),
        ));
    col = col.child(source_seg);

    // 分榜：三个榜种
    let kind_tabs = ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Row)
        .width_full()
        .gap(6)
        .child(seg_button("经典", kind == 0, &format!("{EVENT_BOARD_KIND_PREFIX}0")))
        .child(seg_button("无尽", kind == 1, &format!("{EVENT_BOARD_KIND_PREFIX}1")))
        .child(seg_button("最快通关", kind == 2, &format!("{EVENT_BOARD_KIND_PREFIX}2")));
    col = col.child(kind_tabs);

    // 我的排名：无论是否在列表里都给一条明确反馈
    let my_addr = crate::state::connected_devices()
        .into_iter()
        .next()
        .map(|(addr, _)| addr)
        .unwrap_or_default();
    let mine = rows
        .iter()
        .position(|e| !my_addr.is_empty() && e.addr == my_addr);
    let my_text = match mine {
        Some(i) => {
            let e = &rows[i];
            let value = match board_kind {
                BoardKind::Floor => format!("{} 层", e.floor),
                BoardKind::Duration => format_duration(e.dur_ms),
            };
            format!("{} · {} · 第 {} 名", kind_caption, value, i + 1)
        }
        None => format!("{} · 未上榜", kind_caption),
    };
    col = col.child(
        ui::Element::new(ui::ElementType::Div, None)
            .flex()
            .flex_direction(ui::FlexDirection::Row)
            .align_center()
            .width_full()
            .gap(8)
            .bg(CARD_BG_2)
            .radius(10)
            .padding_top(9)
            .padding_bottom(9)
            .padding_left(11)
            .padding_right(11)
            .child(
                ui::Element::new(ui::ElementType::P, Some("我的最佳"))
                    .size(11)
                    .text_color(TEXT_MUTED),
            )
            .child(ui::Element::new(ui::ElementType::Div, None).flex_grow(1.0).flex())
            .child(
                ui::Element::new(ui::ElementType::P, Some(&my_text))
                    .size(12)
                    .text_color(if mine.is_some() { ACCENT } else { TEXT_SECONDARY }),
            ),
    );

    // 列表
    if let Some(err) = cloud_err.as_ref().filter(|_| use_cloud) {
        col = col.child(hint(&format!("同步失败：{err}")));
    } else if rows.is_empty() {
        let msg = if use_cloud {
            if cloud_ms == 0 {
                "正在同步在线榜…"
            } else {
                "这个榜还没有记录。"
            }
        } else {
            "这台手机还没有这个榜的成绩。"
        };
        col = col.child(hint(msg));
    } else {
        let mut list = ui::Element::new(ui::ElementType::Div, None)
            .flex()
            .flex_direction(ui::FlexDirection::Column)
            .width_full()
            .gap(8);
        for (index, entry) in rows.iter().enumerate() {
            list = list.child(board_row(index, entry, board_kind));
        }
        col = col.child(list);
    }

    // 排序口径 + 更新时间：把规则写在榜下，别让用户猜
    let rule = match board_kind {
        BoardKind::Floor => "排序：层数降序 · 同层比用时 · 再比日期",
        BoardKind::Duration => "排序：通关局用时升序",
    };
    let foot = if use_cloud {
        format!("{rule}　·　{}", relative_time(cloud_ms))
    } else {
        format!("{rule}　·　本机共 {total} 条")
    };
    col.child(
        ui::Element::new(ui::ElementType::P, Some(&foot))
            .size(10)
            .text_color(TEXT_MUTED),
    )
}

/// 分段/胶囊按钮：选中给金色描边 + 淡金底，未选保持中性灰。
fn seg_button(text: &str, active: bool, event: &str) -> ui::Element {
    let mut btn = ui::Element::new(ui::ElementType::Button, Some(text))
        .without_default_styles()
        .flex_grow(1.0)
        .radius(999)
        .padding_top(7)
        .padding_bottom(7)
        .size(11)
        .text_color(if active { ACCENT } else { TEXT_MUTED });
    btn = if active {
        btn.border(1, "#5a4520").bg(ACCENT_SOFT)
    } else {
        btn.border(1, BORDER)
    };
    btn.on(ui::Event::Click, event)
}

/// 云端榜的「多久前更新」：0 表示还没同步过。
fn relative_time(ms: u128) -> String {
    if ms == 0 {
        return "尚未同步".to_string();
    }
    let secs = crate::state::now_unix_ms().saturating_sub(ms) / 1000;
    if secs < 60 {
        "刚刚更新".to_string()
    } else if secs < 3600 {
        format!("{} 分钟前", secs / 60)
    } else if secs < 86_400 {
        format!("{} 小时前", secs / 3600)
    } else {
        format!("{} 天前", secs / 86_400)
    }
}

/// 队伍项：上报格式 "编号:名字:等级"（老数据是 "名字:等级" 或只有名字）。
struct PartyMon {
    id: u32,
    name: String,
    level: String,
}

fn parse_party(party: &[String]) -> Vec<PartyMon> {
    let mut out: Vec<PartyMon> = Vec::new();
    for raw in party.iter().take(6) {
        let item = raw.trim();
        if item.is_empty() {
            continue;
        }
        let fields: Vec<&str> = item.split(':').collect();
        let mon = match fields.as_slice() {
            [id, name, lv, ..] => PartyMon {
                id: id.chars().filter(|c| c.is_ascii_digit()).collect::<String>().parse().unwrap_or(0),
                name: name.to_string(),
                level: lv.chars().filter(|c| c.is_ascii_digit()).collect(),
            },
            [name, lv] => PartyMon {
                id: 0,
                name: name.to_string(),
                level: lv.chars().filter(|c| c.is_ascii_digit()).collect(),
            },
            _ => PartyMon {
                id: 0,
                name: item.to_string(),
                level: String::new(),
            },
        };
        out.push(mon);
    }
    out
}

/// 队伍网格：每行 3 只，6 只两行排完，每只 24px 小图 + 名字等级。
fn party_grid(party: &[String]) -> ui::Element {
    let mons = parse_party(party);
    let mut col = ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Column)
        .width_full();
    if mons.is_empty() {
        return col;
    }

    for chunk in mons.chunks(3) {
        let mut row = ui::Element::new(ui::ElementType::Div, None)
            .flex()
            .flex_direction(ui::FlexDirection::Row)
            .width_full()
            .padding_top(3)
            .padding_bottom(3);
        for mon in chunk {
            let resolved = if mon.id == 0 {
                crate::mon_data::id_by_name(&mon.name).unwrap_or(0)
            } else {
                mon.id
            };
            let art = mon_cell(resolved);
            let label = if mon.level.is_empty() {
                mon.name.clone()
            } else {
                format!("{} {}", mon.name, mon.level)
            };
            let text = ui::Element::new(ui::ElementType::P, Some(&label))
                .size(10)
                .text_color(TEXT_MUTED);
            let cell = ui::Element::new(ui::ElementType::Div, None)
                .flex()
                .flex_direction(ui::FlexDirection::Row)
                .flex_grow(1.0)
                .padding_right(4)
                .child(art)
                .child(
                    ui::Element::new(ui::ElementType::Div, None)
                        .padding_left(5)
                        .child(text),
                );
            row = row.child(cell);
        }
        col = col.child(row);
    }
    col
}

/// 从 UI 事件 payload 里抠出输入框文本：宿主可能给 {"value":"x"}、纯字符串或原文。
fn extract_event_value(raw: &str) -> String {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) {
        for key in ["value", "content", "text"] {
            if let Some(s) = v.get(key).and_then(|x| x.as_str()) {
                return s.to_string();
            }
        }
        if let Some(s) = v.as_str() {
            return s.to_string();
        }
    }
    raw.to_string()
}

fn board_row(index: usize, entry: &crate::scores::ScoreEntry, kind: BoardKind) -> ui::Element {
    let (badge_bg, badge_fg) = match index {
        0 => ("#3a2d10", "#f0b552"),
        1 => ("#2b2f36", "#cfd4dc"),
        2 => ("#33241a", "#c48354"),
        _ => ("#212128", "#7a7a84"),
    };

    let rank = ui::Element::new(ui::ElementType::Div, None)
        .width(22)
        .height(22)
        .radius(6)
        .bg(badge_bg)
        .flex()
        .align_center()
        .justify_center()
        .child(
            ui::Element::new(ui::ElementType::P, Some(&format!("{}", index + 1)))
                .size(11)
                .text_color(badge_fg),
        );

    let name = crate::scores::display_name(entry);
    let suffix = match kind {
        BoardKind::Floor => format!("· {} 层", entry.floor),
        BoardKind::Duration => format!("· {}", format_duration(entry.dur_ms)),
    };
    let main_row = ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Row)
        .align_center()
        .gap(8)
        .child(
            ui::Element::new(ui::ElementType::P, Some(&name))
                .size(14)
                .text_color(TEXT_PRIMARY),
        )
        .child(
            ui::Element::new(ui::ElementType::P, Some(&suffix))
                .size(12)
                .text_color(TEXT_MUTED),
        );

    let meta_text = {
        let mut parts: Vec<String> = Vec::new();
        if entry.win {
            parts.push("通关".to_string());
        }
        if !entry.rules.is_empty() {
            parts.push("挑战词条".to_string());
        }
        if kind == BoardKind::Floor {
            parts.push(format_duration(entry.dur_ms));
        }
        if entry.money > 0 {
            parts.push(format!("₽{}", entry.money));
        }
        parts.join(" · ")
    };

    let mut body = ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Column)
        .flex_grow(1.0)
        .gap(5)
        .child(main_row);
    if !meta_text.is_empty() {
        body = body.child(
            ui::Element::new(ui::ElementType::P, Some(&meta_text))
                .size(11)
                .text_color(TEXT_MUTED),
        );
    }
    if !entry.party.is_empty() {
        body = body.child(party_grid(&entry.party));
    }

    ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Row)
        .width_full()
        .gap(11)
        .bg(CARD_BG_2)
        .radius(12)
        .padding_top(11)
        .padding_bottom(11)
        .padding_left(12)
        .padding_right(12)
        .child(rank)
        .child(body)
}

fn format_duration(ms: u64) -> String {
    let total_secs = ms / 1000;
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    if hours > 0 {
        format!("{}小时{:02}分", hours, minutes)
    } else if minutes > 0 {
        format!("{}分{}秒", minutes, total_secs % 60)
    } else {
        format!("{}秒", total_secs)
    }
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

fn ghost_button(text: &str) -> ui::Element {
    ui::Element::new(ui::ElementType::Button, Some(text))
        .without_default_styles()
        .border(1, BORDER)
        .radius(9)
        .padding_top(6)
        .padding_bottom(6)
        .padding_left(12)
        .padding_right(12)
        .text_color(TEXT_SECONDARY)
        .size(12)
}

/// 主行动按钮的金色描边变体：比 ghost 更显眼，但不如实心按钮抢。
fn gold_button(text: &str) -> ui::Element {
    ui::Element::new(ui::ElementType::Button, Some(text))
        .without_default_styles()
        .border(1, "#5a4520")
        .radius(9)
        .padding_top(6)
        .padding_bottom(6)
        .padding_left(12)
        .padding_right(12)
        .text_color(ACCENT)
        .size(12)
}

fn secondary_button(text: &str) -> ui::Element {
    ui::Element::new(ui::ElementType::Button, Some(text))
        .width_full()
        .without_default_styles()
        .border(1, "#5a4520")
        .radius(11)
        .padding_top(10)
        .padding_bottom(10)
        .text_color(ACCENT)
        .size(13)
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

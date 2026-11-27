use std::collections::HashSet;

use serde_json::{Map, Value};

use crate::astrobox::psys_host_v4::{device, interconnect, register, thirdpartyapp};
use crate::state::{self, InstalledApp, HANDHELD_PKG, TARGET_PKG};

/// Minimum interval between automatic render-driven refreshes (devices +
/// installed apps + re-register). Click-heavy users won't hammer the host.
const AUTO_REFRESH_THROTTLE_MS: u128 = 1500;

/// AstroBox NG wraps each interconnect-message event payload in a JSON envelope
/// before handing it to the plugin's `on_event`:
///
/// ```json
/// { "addr": "...", "pkgName": "...", "payloadHex": "...", "payloadText": "..." }
/// ```
///
/// The host already knows the exact `(addr, pkgName)` the message came from, so
/// we read both straight off the envelope. That's what lets several QuickApps
/// talk over fetch at the same time without their messages — or, worse, their
/// responses — being misattributed to a single package.
///
/// The legacy heuristic (most-recently-active enabled package) is kept only as
/// a fallback for older hosts whose envelope omits these fields, or for raw
/// payloads that aren't wrapped at all. That path can't disambiguate concurrent
/// QuickApps, which is exactly the bug the explicit fields fix.
pub struct ParsedMessage {
    pub addr: String,
    pub pkg_name: String,
    pub data: String,
}

pub fn parse_message(payload: &str) -> ParsedMessage {
    let envelope: Option<Value> = serde_json::from_str(payload).ok();

    let data = envelope
        .as_ref()
        .and_then(payload_text_from_envelope)
        .unwrap_or_else(|| payload.to_string());

    // Prefer the host-supplied package name; only fall back to the heuristic
    // when the envelope omits it (older host) so we never collapse concurrent
    // QuickApps onto one package.
    let pkg_name = envelope
        .as_ref()
        .and_then(|v| v.get("pkgName"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| guess_pkg_name(&data));

    // Same for the originating device address — authoritative when present.
    let addr = envelope
        .as_ref()
        .and_then(|v| v.get("addr"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| state::first_device_addr_for(&pkg_name).unwrap_or_default());

    ParsedMessage {
        addr,
        pkg_name,
        data,
    }
}

/// Peel the inner message text out of the host envelope: prefer `payloadText`,
/// fall back to a `payload` field (string or stringified object). Returns
/// `None` when neither key is present so the caller can use the raw payload
/// (older hosts / unwrapped messages).
fn payload_text_from_envelope(json: &Value) -> Option<String> {
    if let Some(text) = json.get("payloadText").and_then(|v| v.as_str()) {
        return Some(text.to_string());
    }
    if let Some(payload_value) = json.get("payload") {
        if let Some(text) = payload_value.as_str() {
            return Some(text.to_string());
        }
        return Some(payload_value.to_string());
    }
    None
}

/// Choose which tracked package this message should be attributed to. We
/// prefer the most-recently-active enabled package, then any enabled package,
/// and finally fall back to the legacy `com.fetch` name so the JS protocol
/// still has somewhere to land if the user hasn't picked a package yet.
fn guess_pkg_name(_data: &str) -> String {
    let apps = state::snapshot_apps();
    let mut enabled: Vec<_> = apps.iter().filter(|a| a.enabled).collect();
    enabled.sort_by(|a, b| b.last_seen_unix_ms.cmp(&a.last_seen_unix_ms));
    if let Some(top) = enabled.first() {
        return top.pkg_name.clone();
    }
    state::HISTORICAL_PKG_NAME.to_string()
}

/// Run the full render-driven refresh: pull the device list, ask each device
/// for its installed third-party apps, and re-register every monitored
/// package on every device. Throttled to avoid spamming the host when the UI
/// rerenders rapidly. Returns `true` when it actually did work.
pub async fn auto_refresh() -> bool {
    if !state::try_claim_auto_refresh(AUTO_REFRESH_THROTTLE_MS) {
        return false;
    }
    refresh_connected_devices().await;
    refresh_installed_apps().await;
    for pkg in state::pkg_names() {
        register_for_all_devices(&pkg).await;
    }
    // 首次刷新（手表在线）时主动让手表补传离线积压成绩；与 DeviceAction 共用同一节流基准。
    if state::auto_flush_due() {
        state::mark_auto_flush();
        let _ = dispatch_watch_command("score-flush", None).await;
    }
    true
}

/// Refresh the cached list of installed third-party apps across every
/// connected device. Apps with the same package name across devices show up
/// once per device so the user can tell where they come from.
pub async fn refresh_installed_apps() {
    let devices = state::connected_devices();
    let mut out: Vec<InstalledApp> = Vec::new();
    let mut queried_addrs: HashSet<String> = HashSet::new();
    for (addr, device_name) in devices {
        let result = thirdpartyapp::get_thirdparty_app_list(addr.clone()).await;
        match result {
            Ok(apps) => {
                queried_addrs.insert(addr.clone());
                for app in apps {
                    out.push(InstalledApp {
                        addr: addr.clone(),
                        device_name: device_name.clone(),
                        package_name: app.package_name,
                        app_name: app.app_name,
                        version_code: app.version_code,
                    });
                }
            }
            Err(reason) => {
                tracing::warn!("failed to list third-party apps on {}: {}", addr, reason);
            }
        }
    }
    out.sort_by(|a, b| {
        a.device_name
            .cmp(&b.device_name)
            .then(a.app_name.cmp(&b.app_name))
            .then(a.package_name.cmp(&b.package_name))
    });
    tracing::info!("refreshed installed apps: total={}", out.len());
    state::set_installed_apps(out.clone());

    prune_uninstalled(&out, &queried_addrs);
}

/// Drop monitored packages whose owning device is currently connected but
/// no longer has the app installed. We deliberately leave entries whose
/// `last_addr` is not connected — that device may simply be off, and we
/// shouldn't forget the user's authorization just because of a power-down.
/// Entries with no `last_addr` (manually added, never seen) are kept until
/// they either get a message or are confirmed missing on all connected
/// devices.
fn prune_uninstalled(installed: &[InstalledApp], queried_addrs: &HashSet<String>) {
    if queried_addrs.is_empty() {
        return;
    }

    let installed_pairs: HashSet<(String, String)> = installed
        .iter()
        .map(|app| (app.addr.clone(), app.package_name.clone()))
        .collect();

    let to_prune: Vec<String> = state::snapshot_apps()
        .into_iter()
        .filter(|entry| match &entry.last_addr {
            // Ground truth available for this device: prune if the app is
            // no longer there.
            Some(addr) if queried_addrs.contains(addr) => {
                !installed_pairs.contains(&(addr.clone(), entry.pkg_name.clone()))
            }
            // Device is offline OR entry was manually added without ever
            // receiving a message — keep the user's authorization either way.
            _ => false,
        })
        .map(|entry| entry.pkg_name)
        .collect();

    for pkg in to_prune {
        tracing::info!("auto-prune monitored pkg {pkg}: no longer installed on its device");
        state::remove_app(&pkg);
    }
}

/// Refresh the cached list of connected device addresses.
pub async fn refresh_connected_devices() {
    let devices = device::get_connected_device_list()
        .await
        .into_iter()
        .map(|info| (info.addr, info.name))
        .collect::<Vec<_>>();
    if devices.is_empty() {
        tracing::warn!("no connected devices available");
    } else {
        tracing::info!(
            "refreshed connected devices: count={}, sample={:?}",
            devices.len(),
            devices.first()
        );
    }
    state::set_connected_devices(devices);
}

/// Register a single (addr, pkg_name) interconnect-recv pair with the host.
async fn register_pair(addr: &str, pkg_name: &str) -> bool {
    let result =
        register::register_interconnect_recv(addr.to_string(), pkg_name.to_string()).await;
    match result {
        Ok(()) => {
            tracing::info!(
                "registered interconnect-recv addr={} pkg={}",
                addr,
                pkg_name
            );
            true
        }
        Err(reason) => {
            tracing::error!(
                "failed to register interconnect-recv addr={} pkg={}: {}",
                addr,
                pkg_name,
                reason
            );
            false
        }
    }
}

/// Register the given package on every currently connected device. Returns the
/// number of successful registrations.
pub async fn register_for_all_devices(pkg_name: &str) -> usize {
    let devices = state::connected_devices();
    if devices.is_empty() {
        // Try once with an empty address as a "match anything" fallback.
        if register_pair("", pkg_name).await {
            return 1;
        }
        return 0;
    }
    let mut ok = 0usize;
    for (addr, _) in devices {
        if register_pair(&addr, pkg_name).await {
            ok += 1;
        }
    }
    ok
}

/// Send a JSON message back over QAIC to the same (addr, pkg_name) that we
/// received from.
pub async fn send_json(addr: &str, pkg_name: &str, tag: &str, body: Value) -> bool {
    let mut payload = Map::<String, Value>::new();
    payload.insert("tag".to_string(), Value::String(tag.to_string()));
    match body {
        Value::Object(map) => {
            for (k, v) in map {
                payload.insert(k, v);
            }
        }
        other => {
            payload.insert("data".to_string(), other);
        }
    }
    let text = Value::Object(payload).to_string();
    tracing::info!(
        "interconnect send: addr={} pkg={} tag={} len={}",
        addr,
        pkg_name,
        tag,
        text.len()
    );
    let result =
        interconnect::send_qaic_message(addr.to_string(), pkg_name.to_string(), text).await;
    match result {
        Ok(()) => true,
        Err(reason) => {
            tracing::error!(
                "interconnect send failed: addr={} pkg={} tag={}: {}",
                addr,
                pkg_name,
                tag,
                reason
            );
            false
        }
    }
}

/// 主动向手表快应用下发一条 `pkr-sync` 命令（备份 / 恢复）。
///
/// 参照可工作的主动推送先例，发送前补齐前置，避免拿过期缓存地址或错配包名：
///   1. 实时查询已连接设备，取当前在线设备地址；
///   2. 从手表已装应用列表解析目标应用（固定包名优先，其次按应用名关键字）；
///   3. 先把快应用拉到前台标题页（launch 失败不阻断，可能已在前台运行）；
///   4. 确保该 `(addr, pkg)` 的接收回调已注册（重复注册不阻断）；
///   5. 再投递命令。
pub async fn dispatch_watch_command(action: &str, backup_id: Option<&str>) -> Result<(), String> {
    let devices = device::get_connected_device_list().await;
    let dev = devices
        .into_iter()
        .next()
        .ok_or_else(|| "未检测到已连接手表，请先在 AstroBox 中连接设备".to_string())?;

    let apps = thirdpartyapp::get_thirdparty_app_list(dev.addr.clone())
        .await
        .map_err(|reason| format!("读取手表应用列表失败：{reason}"))?;

    let app = apps
        .iter()
        .find(|a| a.package_name == HANDHELD_PKG || a.package_name == TARGET_PKG)
        .or_else(|| apps.iter().find(|a| a.app_name.contains("宝可梦")))
        .cloned()
        .ok_or_else(|| "手表上未找到宝可梦肉鸽快应用，请先安装".to_string())?;
    let pkg = app.package_name.clone();

    // 先拉起快应用到前台标题页；宿主可能在应用未活跃时拒收下行命令。
    if thirdpartyapp::launch_qa(dev.addr.clone(), app, "pages/index".to_string())
        .await
        .is_err()
    {
        tracing::warn!("launch_qa failed/ignored: addr={} pkg={}", dev.addr, pkg);
    }

    // 确保接收回调已注册；宿主对重复注册可能返回 Err，这里记日志后继续尝试发送。
    if register::register_interconnect_recv(dev.addr.clone(), pkg.clone())
        .await
        .is_err()
    {
        tracing::warn!("register recv already/existing: addr={} pkg={}", dev.addr, pkg);
    }

    // 生成命令 ID 并登记为待回执；手表执行完会回 `pkr-sync-ack` 按此 ID 配对回填。
    let cmd_id = state::next_cmd_id();
    state::register_pending_command(&cmd_id, action);

    let mut body = Map::new();
    body.insert("cmdId".into(), Value::String(cmd_id.clone()));
    body.insert("action".into(), Value::String(action.to_string()));
    if let Some(id) = backup_id {
        body.insert("backupId".into(), Value::String(id.to_string()));
    }

    let ok = send_json(&dev.addr, &pkg, "pkr-sync", Value::Object(body)).await;
    if ok {
        Ok(())
    } else {
        // 发送即被拒：直接把命令置失败，避免挂在 pending 一直等待。
        state::resolve_command(&cmd_id, false, "命令被手表拒绝：请先在手表上打开宝可梦肉鸽，再重试");
        Err("命令被手表拒绝：请先在手表上打开宝可梦肉鸽，再重试；或直接在手表游戏菜单内备份/恢复".to_string())
    }
}

/// 处理手表回传的命令回执帧 `{tag:"pkr-sync-ack", cmdId, ok, message?}`。
/// 按 cmdId 回填命令状态，并把结果写入界面提示。
pub fn handle_command_ack(body: Value) {
    let cmd_id = body
        .get("cmdId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if cmd_id.is_empty() {
        tracing::warn!("command ack missing cmdId");
        return;
    }
    let ok = body.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    let message = body
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let action = state::with_state(|s| {
        s.pending_commands
            .get(&cmd_id)
            .map(|c| c.action.clone())
            .unwrap_or_default()
    });
    let resolved = state::resolve_command(&cmd_id, ok, &message);
    // 手表补传完成后，让下一拍渲染立刻重新拉云端榜，不用等 60 秒节流。
    if action == "score-flush" && ok {
        state::cloud_invalidate();
    }
    let Some((action, _)) = resolved else {
        tracing::warn!("command ack for unknown cmdId: {}", cmd_id);
        return;
    };

    let notice = if ok {
        match action.as_str() {
            "restore" => "恢复完成，进度已写回手表".to_string(),
            "sync-atlas" => "图集同步完成".to_string(),
            _ => "备份已保存到手机".to_string(),
        }
    } else if message.is_empty() {
        "手表执行失败，请重试".to_string()
    } else {
        message
    };
    state::set_notice(notice);
}

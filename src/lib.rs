use crate::exports::astrobox::psys_plugin_v4::{event, lifecycle};
use crate::state::{HANDHELD_PKG, TARGET_PKG};

pub mod backup;
pub mod atlas_assets;
pub mod codec;
pub mod fetch;
pub mod handshake;
pub mod interconnect;
pub mod logger;
pub mod persist;
pub mod state;
pub mod stream;
pub mod transfer;
pub mod ui;

wit_bindgen::generate!({
    path: "wit",
    world: "psys-world-v4",
    generate_all,
});

struct MyPlugin;

impl event::Guest for MyPlugin {
    async fn on_event(event_type: event::EventType, event_payload: String) -> String {
        tracing::info!(
            "on_event: type={:?} payload_len={}",
            event_type,
            event_payload.len()
        );

        match event_type {
            event::EventType::InterconnectMessage => {
                let parsed = interconnect::parse_message(&event_payload);
                dispatch_interconnect(&parsed.addr, &parsed.pkg_name, &parsed.data).await;
            }
            event::EventType::PluginMessage => {
                tracing::info!("plugin-message: {}", event_payload);
            }
            event::EventType::DeviceAction => {
                interconnect::refresh_connected_devices().await;
                interconnect::refresh_installed_apps().await;
                for pkg in state::pkg_names() {
                    interconnect::register_for_all_devices(&pkg).await;
                }
                ui::rerender().await;
            }
            event::EventType::Timer => {
                stream::prune_idle();
                transfer::prune_idle();
            }
            event::EventType::ProviderAction
            | event::EventType::DeeplinkAction
            | event::EventType::TransportPacket => {}
        }

        String::new()
    }

    async fn on_ui_event(
        event_id: String,
        event: crate::astrobox::psys_host_v4::ui::Event,
        event_payload: String,
    ) -> String {
        ui::ui_event_processor(event, &event_id, &event_payload).await;
        String::new()
    }

    async fn on_ui_render(element_id: String) {
        ui::render_main_ui(&element_id).await;
    }

    async fn on_card_render(_card_id: String) {}
}

async fn dispatch_interconnect(addr: &str, pkg: &str, data: &str) {
    state::ensure_app(pkg);

    if !state::is_enabled(pkg) {
        tracing::warn!(
            "interconnect message dropped (disabled): pkg={} addr={} len={}",
            pkg,
            addr,
            data.len()
        );
        return;
    }

    let value: serde_json::Value = match serde_json::from_str(data) {
        Ok(v) => v,
        Err(err) => {
            tracing::error!("failed to parse interconnect data as JSON: {err}");
            return;
        }
    };

    let obj = match value {
        serde_json::Value::Object(map) => map,
        other => {
            tracing::error!("interconnect data is not a JSON object: {}", other);
            return;
        }
    };

    let tag = obj
        .get("tag")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let mut body = obj.clone();
    body.remove("tag");
    let body_value = serde_json::Value::Object(body);

    match tag.as_str() {
        handshake::HS_TAG => {
            handshake::handle_packet(addr, pkg, &body_value).await;
            state::record_request(pkg, addr, None);
            ui::rerender().await;
        }
        fetch::FETCH_TAG => {
            fetch::handle_request(addr, pkg, body_value).await;
            ui::rerender().await;
        }
        fetch::FETCH_ACK_TAG => {
            // Pure flow-control frame: advance the finite-chunk window.
            fetch::handle_ack(addr, pkg, body_value).await;
        }
        stream::FETCH_STREAM_ACK_TAG => {
            fetch::handle_stream_ack(addr, pkg, body_value).await;
        }
        stream::FETCH_STREAM_CANCEL_TAG => {
            fetch::handle_stream_cancel(addr, pkg, body_value).await;
        }
        "pkr-sync-ack" => {
            interconnect::handle_command_ack(body_value);
            ui::rerender().await;
        }
        // 独立状态通道：手表直发、不进文件传输队列，几十字节即时到。
        // 只做展示更新，不参与 synced_files 记账（账仍由 atlas-report 权威回执维护）。
        "pkr-stat" => {
            let ready = body_value.get("ready").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let total = body_value.get("total").and_then(|v| v.as_u64()).unwrap_or(31) as usize;
            let cur = body_value
                .get("cur")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let busy = body_value
                .get("busy")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            state::set_live_status(ready, total, cur, busy);
            ui::rerender().await;
        }
        other => {
            tracing::warn!(
                "unknown interconnect tag: {} (pkg={} addr={})",
                other,
                pkg,
                addr
            );
        }
    }
}

impl lifecycle::Guest for MyPlugin {
    async fn on_load() {
        logger::init();
        tracing::info!("PKR Band Bridge plugin loaded");

        // Restore previously authorized monitored apps from disk first, so
        // the rest of startup (device refresh + re-register) operates on the
        // restored set without flashing an empty UI.
        let restored = persist::load_apps();
        let restored_count = restored.len();
        state::install_loaded_apps(restored);
        state::ensure_app(TARGET_PKG);
        state::ensure_app(HANDHELD_PKG);
        tracing::info!("startup: restored {} monitored apps", restored_count);

        // Pre-populate the device cache + installed-app list so the first UI
        // render shows something, then register every restored package on
        // every currently connected device.
        interconnect::refresh_connected_devices().await;
        interconnect::refresh_installed_apps().await;
        for pkg in state::pkg_names() {
            let count = interconnect::register_for_all_devices(&pkg).await;
            tracing::info!("startup register: pkg={} devices={}", pkg, count);
        }
    }
}

export!(MyPlugin);

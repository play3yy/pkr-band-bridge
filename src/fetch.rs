use std::collections::HashMap;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Map, Value, json};
use url::Url;
use waki::{Client, Method, Response};

use crate::atlas_assets;
use crate::backup;
use crate::codec::{self, BodyEncoding, COMPRESS_MIN_SIZE, Compression};
use crate::handshake::{self, NegotiatedCaps};
use crate::interconnect;
use crate::state;
use crate::stream;
use crate::transfer;

/// Tag used for the fetch request/response exchange (matches the JS client).
pub const FETCH_TAG: &str = "fetch";
/// Tag used to carry chunked response data. Only emitted when the peer
/// negotiated chunking via the v2+ handshake `caps`. Legacy peers never see
/// this tag and continue receiving single-message responses.
pub const FETCH_CHUNK_TAG: &str = "fetch-chunk";
/// Tag the peer uses to acknowledge received chunks (v3 ACK flow control).
/// Carries `{ id, ack }` where `ack` is the next contiguous chunk index the
/// peer still needs. Drives the sliding window in `transfer.rs`. Only seen when
/// both sides negotiated `caps.ack`.
pub const FETCH_ACK_TAG: &str = "fetch-ack";
/// Last-resort guard for legacy single-message responses. If negotiated caps
/// are missing, large binary responses would otherwise become one huge JSON
/// `fetch` frame and can wedge the host UI / QAIC transport. Normal large
/// responses must use negotiated chunking instead.
const MAX_UNCHUNKED_WIRE_LEN: usize = 16 * 1024;
/// Responses at least this large are automatically streamed for v4 peers when
/// the caller did not explicitly choose `options.stream`.
const AUTO_STREAM_MIN_BYTES: usize = 64 * 1024;
/// Stop redirect loops and unbounded chains. This matches the usual browser/
/// HTTP-client order of magnitude while keeping one fetch event bounded.
const MAX_REDIRECTS: usize = 10;
/// TCP/TLS 建连超时。waki 默认不设超时，弱网/CDN 不可达时 `pollable.block()` 会
/// 永久挂住组件任务，后续所有同步请求排队；封顶到该时长后快速失败，交由手表侧重试。
const CONNECT_TIMEOUT_SECS: u64 = 8;

#[derive(Debug, Deserialize)]
pub struct FetchRequest {
    pub id: Option<String>,
    pub url: String,
    #[serde(default)]
    pub options: Option<FetchOptions>,
}

#[derive(Debug, Default, Deserialize)]
pub struct FetchOptions {
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub headers: Option<HashMap<String, Value>>,
    #[serde(default)]
    pub body: Option<String>,
    /// Mirrors the JS `raw` option. When true we hand back the raw bytes
    /// instead of trying to decode them as UTF-8 text.
    #[serde(default)]
    pub raw: Option<bool>,
    /// v4 streaming preference. `true` forces a stream when v4 was negotiated,
    /// `false` keeps the finite v1-v3 response path, and omission automatically
    /// streams large or audio/video responses.
    #[serde(default)]
    pub stream: Option<bool>,
    /// Follow HTTP 301/302/303/307/308 responses. Disabled by default for
    /// backwards compatibility; redirect chains are capped at 10 hops.
    #[serde(default, rename = "followRedirects", alias = "follow_redirects")]
    pub follow_redirects: Option<bool>,
    /// v4 only: coalesce short HTTP reads so every non-tail data frame is exactly
    /// the negotiated chunk size. Useful for `writeArrayBuffer` at `seq * chunkSize`.
    #[serde(default, rename = "fixedChunks", alias = "fixed_chunks")]
    pub fixed_chunks: Option<bool>,
}

struct FetchResponse {
    ok: bool,
    status: u16,
    status_text: &'static str,
    headers: Map<String, Value>,
    body_bytes: Vec<u8>,
    /// True when the assembled bytes should be treated as binary by the peer;
    /// false means "decode as UTF-8". Independent of any wire encoding.
    raw: bool,
}

struct HttpResponse {
    response: Response,
    ok: bool,
    status: u16,
    status_text: &'static str,
    headers: Map<String, Value>,
    raw_requested: bool,
}

impl HttpResponse {
    fn into_buffered(self) -> Result<FetchResponse, String> {
        let body_bytes = self
            .response
            .body()
            .map_err(|e| format!("read body failed: {e}"))?;
        let raw = self.raw_requested || std::str::from_utf8(&body_bytes).is_err();
        Ok(FetchResponse {
            ok: self.ok,
            status: self.status,
            status_text: self.status_text,
            headers: self.headers,
            body_bytes,
            raw,
        })
    }
}

fn cdn_urls() -> HashMap<&'static str, &'static str> {
    HashMap::from([
        ("back_atlas00_v1.png", "https://media.doubao.com/space/api/box/stream/download/all_by_mount_point/GHbebku7soVMD4xRZZPcMqwznwb"),
        ("back_atlas01_v1.png", "https://media.doubao.com/space/api/box/stream/download/all_by_mount_point/MkOHb0yotor8Zlx1y0HcuBq8nPd"),
        ("back_atlas02_v1.png", "https://media.doubao.com/space/api/box/stream/download/all_by_mount_point/LUhIbQHipoItSYx7mLZcqF3Cngh"),
        ("back_atlas03_v1.png", "https://media.doubao.com/space/api/box/stream/download/all_by_mount_point/OqUPbG870o5XSKxllVTc9JiXnYD"),
        ("back_atlas04_v1.png", "https://media.doubao.com/space/api/box/stream/download/all_by_mount_point/AezvbY7aooqrqwxtjDncWyErnRc"),
        ("back_atlas05_v1.png", "https://media.doubao.com/space/api/box/stream/download/all_by_mount_point/PSsAbwApqoD0INxNL9kcRROYnWe"),
        ("back_atlas06_v1.png", "https://media.doubao.com/space/api/box/stream/download/all_by_mount_point/IQPCbynR8oAx8axovmlcuDI4n7f"),
        ("back_atlas07_v1.png", "https://media.doubao.com/space/api/box/stream/download/all_by_mount_point/RxtgbmU5wou0srxOd46chX1TnJb"),
        ("back_atlas08_v1.png", "https://media.doubao.com/space/api/box/stream/download/all_by_mount_point/SwXRb2I2jo7tnXxSDDvcrd2UnMf"),
        ("back_atlas09_v1.png", "https://media.doubao.com/space/api/box/stream/download/all_by_mount_point/PW2vbLGJaoVDtvxYYnVcWEEUnOm"),
        ("back_atlas10_v1.png", "https://media.doubao.com/space/api/box/stream/download/all_by_mount_point/ITvYbzxBUo6iv2xcAEFcJy62nve"),
        ("back_atlas11_v1.png", "https://media.doubao.com/space/api/box/stream/download/all_by_mount_point/WhCnbYdumoG2gQxwBcVc05fRnhf"),
        ("back_atlas12_v1.png", "https://media.doubao.com/space/api/box/stream/download/all_by_mount_point/KUKDbJ5zjoOGBjxHUYrch2qynRe"),
        ("forms_atlas0_v1.png", "https://media.doubao.com/space/api/box/stream/download/all_by_mount_point/Bq8hbHDz3o1voDxft7YcmC4un4b"),
        ("front_atlas00_v1.png", "https://media.doubao.com/space/api/box/stream/download/all_by_mount_point/Zyn7brrKkoTzHfxMoZmcwkaZncd"),
        ("front_atlas01_v1.png", "https://media.doubao.com/space/api/box/stream/download/all_by_mount_point/BDQ2bXPmJoLsQSxwH3bc7GTHnNh"),
        ("front_atlas02_v1.png", "https://media.doubao.com/space/api/box/stream/download/all_by_mount_point/I4ZbbfGb6ooZXmxPhgXcSCgznUb"),
        ("front_atlas03_v1.png", "https://media.doubao.com/space/api/box/stream/download/all_by_mount_point/JNL1bcMJhopbFexNmGnclrdRngd"),
        ("front_atlas04_v1.png", "https://media.doubao.com/space/api/box/stream/download/all_by_mount_point/U6tubClHmoDQybxfFYlcFhMpnTe"),
        ("front_atlas05_v1.png", "https://media.doubao.com/space/api/box/stream/download/all_by_mount_point/XvtlbxfIFoqsOzxaRmucSMmpnhg"),
        ("front_atlas06_v1.png", "https://media.doubao.com/space/api/box/stream/download/all_by_mount_point/O5DGbC7wpop9blxdwiHcV8Qwn8d"),
        ("front_atlas07_v1.png", "https://media.doubao.com/space/api/box/stream/download/all_by_mount_point/U29QbL0iQo0nzyxpIgycvAcRnSg"),
        ("front_atlas08_v1.png", "https://media.doubao.com/space/api/box/stream/download/all_by_mount_point/Ictkb6m3BovRFRxW1MkcoBOLnNh"),
        ("front_atlas09_v1.png", "https://media.doubao.com/space/api/box/stream/download/all_by_mount_point/NNfkb12osotSbdxZhbnchfYAnFf"),
        ("front_atlas10_v1.png", "https://media.doubao.com/space/api/box/stream/download/all_by_mount_point/RV8CbPCdRoMNKYxoHXYcB6e9nIc"),
        ("front_atlas11_v1.png", "https://media.doubao.com/space/api/box/stream/download/all_by_mount_point/M8WsbCoWsoGRq3xJGPvcTcmvnVf"),
        ("front_atlas12_v1.png", "https://media.doubao.com/space/api/box/stream/download/all_by_mount_point/PQ3fb0BNIo1WWOxkL7Dcquimnmg"),
        ("icon_atlas00_v1.png", "https://media.doubao.com/space/api/box/stream/download/all_by_mount_point/ODZobxg7woTyS3xgy1Nctf3Un4f"),
        ("icon_atlas01_v1.png", "https://media.doubao.com/space/api/box/stream/download/all_by_mount_point/ZowZb8rzHo1iyqxom9hc8SRnnmb"),
        ("icon_atlas02_v1.png", "https://media.doubao.com/space/api/box/stream/download/all_by_mount_point/LOcqbSGQGo9GqsxVWbncwJNbnyg"),
        ("icon_atlas03_v1.png", "https://media.doubao.com/space/api/box/stream/download/all_by_mount_point/NAYRbcYxFojBxUxjypDcQhJjnee"),
    ])
}

fn expected_sizes() -> HashMap<&'static str, usize> {
    HashMap::from([
        ("back_atlas00_v1.png", 52979),
        ("back_atlas01_v1.png", 50851),
        ("back_atlas02_v1.png", 45577),
        ("back_atlas03_v1.png", 49961),
        ("back_atlas04_v1.png", 51845),
        ("back_atlas05_v1.png", 44382),
        ("back_atlas06_v1.png", 48066),
        ("back_atlas07_v1.png", 47713),
        ("back_atlas08_v1.png", 48633),
        ("back_atlas09_v1.png", 54935),
        ("back_atlas10_v1.png", 54079),
        ("back_atlas11_v1.png", 56106),
        ("back_atlas12_v1.png", 42349),
        ("forms_atlas0_v1.png", 34413),
        ("front_atlas00_v1.png", 58949),
        ("front_atlas01_v1.png", 55422),
        ("front_atlas02_v1.png", 50593),
        ("front_atlas03_v1.png", 54576),
        ("front_atlas04_v1.png", 55744),
        ("front_atlas05_v1.png", 47823),
        ("front_atlas06_v1.png", 51871),
        ("front_atlas07_v1.png", 52759),
        ("front_atlas08_v1.png", 53252),
        ("front_atlas09_v1.png", 61185),
        ("front_atlas10_v1.png", 61533),
        ("front_atlas11_v1.png", 61827),
        ("front_atlas12_v1.png", 45785),
        ("mega_atlas00_v1.png", 108470),
        ("mega_atlas01_v1.png", 13987),
        ("gmax_atlas00_v1.png", 48234),
        ("icon_atlas00_v1.png", 51470),
        ("icon_atlas01_v1.png", 49305),
        ("icon_atlas02_v1.png", 48763),
        ("icon_atlas03_v1.png", 55793),
    ])
}

/// 图集分组清单（basename），供界面按组逐张标注已有/缺失。顺序与手表端
/// `REQUIRED_ATLASES` 对齐：背面 13、形态 1、正面 13、图标 4，共 31 张。
pub fn atlas_groups() -> &'static [(&'static str, &'static [&'static str])] {
    &[
        (
            "背面",
            &[
                "back_atlas00_v1.png",
                "back_atlas01_v1.png",
                "back_atlas02_v1.png",
                "back_atlas03_v1.png",
                "back_atlas04_v1.png",
                "back_atlas05_v1.png",
                "back_atlas06_v1.png",
                "back_atlas07_v1.png",
                "back_atlas08_v1.png",
                "back_atlas09_v1.png",
                "back_atlas10_v1.png",
                "back_atlas11_v1.png",
                "back_atlas12_v1.png",
            ],
        ),
        ("形态", &["forms_atlas0_v1.png"]),
        (
            "Mega",
            &[
                "mega_atlas00_v1.png",
                "mega_atlas01_v1.png",
            ],
        ),
        ("极巨化", &["gmax_atlas00_v1.png"]),
        (
            "正面",
            &[
                "front_atlas00_v1.png",
                "front_atlas01_v1.png",
                "front_atlas02_v1.png",
                "front_atlas03_v1.png",
                "front_atlas04_v1.png",
                "front_atlas05_v1.png",
                "front_atlas06_v1.png",
                "front_atlas07_v1.png",
                "front_atlas08_v1.png",
                "front_atlas09_v1.png",
                "front_atlas10_v1.png",
                "front_atlas11_v1.png",
                "front_atlas12_v1.png",
            ],
        ),
        (
            "图标",
            &[
                "icon_atlas00_v1.png",
                "icon_atlas01_v1.png",
                "icon_atlas02_v1.png",
                "icon_atlas03_v1.png",
            ],
        ),
    ]
}

enum BackupRoute {
    Save,
    List,
    Latest,
    Get(String),
    Delete(String),
    /// 手表端每张图集落盘并通过 PNG 魔数校验后回传的结果上报：{ file, ok }。
    /// 成功/已同步计数以这份手表端真实落盘回执为准，而不是插件把分片发出去的时刻。
    AtlasReport,
    /// 手表端门禁时全量校验沙箱图集后，一次性上报已有清单：{ files: [base...] }。
    /// 以此覆盖该包的已同步记录，作为「界面显示已有图纸」的准实时基准。
    AtlasList,
}

fn backup_route(url: &str) -> Option<BackupRoute> {
    let rest = url.strip_prefix("local://")?;
    let without_query = rest.split('?').next().unwrap_or(rest);
    let parts: Vec<&str> = without_query.split('/').filter(|s| !s.is_empty()).collect();
    if parts.first()? != &"pkr.sync" {
        return None;
    }
    // 首段是命名空间 `pkr.sync`，路由判断只看其后的路径段。
    let route: &[&str] = &parts[1..];
    match route {
        ["backup"] => Some(BackupRoute::Save),
        ["backup", "list"] => Some(BackupRoute::List),
        ["backup", "latest"] => Some(BackupRoute::Latest),
        ["backup", id] if !id.is_empty() => Some(BackupRoute::Get((*id).to_string())),
        ["backup", id, "delete"] if !id.is_empty() => Some(BackupRoute::Delete((*id).to_string())),
        ["atlas-report"] => Some(BackupRoute::AtlasReport),
        ["atlas-list"] => Some(BackupRoute::AtlasList),
        _ => None,
    }
}

fn backup_json_response(body: Value, status: u16, status_text: &'static str) -> FetchResponse {
    let mut headers = Map::new();
    headers.insert("content-type".into(), Value::String("application/json; charset=utf-8".into()));
    FetchResponse {
        ok: (200..300).contains(&status),
        status,
        status_text,
        headers,
        body_bytes: body.to_string().into_bytes(),
        raw: false,
    }
}

fn handle_backup_route(
    addr: &str,
    pkg: &str,
    method: &str,
    route: BackupRoute,
    body: Option<String>,
) -> Result<FetchResponse, String> {
    match route {
        BackupRoute::Save => {
            if method != "POST" {
                return Err("backup save requires POST".to_string());
            }
            let raw = body.ok_or("backup body missing")?;
            let snapshot: Value = serde_json::from_str(&raw).map_err(|err| format!("backup json invalid: {err}"))?;
            let device_name = state::device_name_for(addr);
            let meta = backup::save_backup(pkg, addr, &device_name, snapshot)?;
            Ok(backup_json_response(
                json!({ "ok": true, "backupId": meta.id, "savedAt": meta.saved_at, "size": meta.size, "summary": meta.summary }),
                200,
                "OK",
            ))
        }
        BackupRoute::List => {
            let metas = backup::list_backups(Some(pkg));
            Ok(backup_json_response(json!({ "ok": true, "backups": metas }), 200, "OK"))
        }
        BackupRoute::Latest => {
            let record = backup::latest_backup(pkg).ok_or_else(|| "no backup found".to_string())?;
            let value = serde_json::to_value(&record).map_err(|err| format!("backup serialize failed: {err}"))?;
            Ok(backup_json_response(value, 200, "OK"))
        }
        BackupRoute::Get(id) => {
            let record = backup::backup_by_id(&id).ok_or_else(|| "backup not found".to_string())?;
            let value = serde_json::to_value(&record).map_err(|err| format!("backup serialize failed: {err}"))?;
            Ok(backup_json_response(value, 200, "OK"))
        }
        BackupRoute::Delete(id) => {
            if method != "POST" && method != "DELETE" {
                return Err("backup delete requires POST or DELETE".to_string());
            }
            let deleted = backup::delete_backup(&id);
            if !deleted {
                return Err("backup not found".to_string());
            }
            Ok(backup_json_response(json!({ "ok": true, "deleted": id }), 200, "OK"))
        }
        BackupRoute::AtlasReport => {
            if method != "POST" {
                return Err("atlas report requires POST".to_string());
            }
            let raw = body.ok_or("atlas report body missing")?;
            let parsed: Value =
                serde_json::from_str(&raw).map_err(|err| format!("atlas report json invalid: {err}"))?;
            let file = parsed
                .get("file")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let ok = parsed.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
            let Some(base) = safe_atlas_base(&file) else {
                return Err("atlas report file invalid".to_string());
            };
            if ok {
                state::record_synced_file(pkg, &base);
                state::record_result(pkg, true, Some("atlas reported ok".to_string()));
            } else {
                state::record_result(pkg, false, Some("atlas reported bad/missing".to_string()));
            }
            Ok(backup_json_response(json!({ "ok": true, "recorded": ok }), 200, "OK"))
        }
        BackupRoute::AtlasList => {
            if method != "POST" {
                return Err("atlas list requires POST".to_string());
            }
            let raw = body.ok_or("atlas list body missing")?;
            let parsed: Value =
                serde_json::from_str(&raw).map_err(|err| format!("atlas list json invalid: {err}"))?;
            let files = parsed
                .get("files")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let mut safe: Vec<String> = Vec::new();
            for item in files {
                let Some(text) = item.as_str() else { continue };
                if let Some(base) = safe_atlas_base(text) {
                    if !safe.iter().any(|existing| existing == &base) {
                        safe.push(base);
                    }
                }
            }
            // 以手表沙箱真实落盘清单覆盖该包记录；之后单张 atlas-report 再增量追加。
            state::replace_synced_files(pkg, safe.clone());
            Ok(backup_json_response(
                json!({ "ok": true, "count": safe.len() }),
                200,
                "OK",
            ))
        }
    }
}

/// 取图集文件名的 basename 并做安全校验：必须是 `.png`，且不含路径穿越/分隔符。
fn safe_atlas_base(file: &str) -> Option<String> {
    let base = file.rsplit('/').next().unwrap_or(file).to_string();
    if base.ends_with(".png") && !base.contains("..") && !base.contains('\\') && !base.contains('/') {
        Some(base)
    } else {
        None
    }
}

fn local_asset_file(url: &str) -> Option<String> {
    let rest = url.strip_prefix("local://")?;
    let parts: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() != 2 {
        return None;
    }
    let (pkg, file) = (parts[0], parts[1]);
    if pkg != state::TARGET_PKG && pkg != state::HANDHELD_PKG {
        return None;
    }
    if !file.ends_with(".png") || file.contains("..") || file.contains('\\') || file.contains('/') {
        return None;
    }
    Some(file.to_string())
}

fn png_body_ok(body: &[u8]) -> bool {
    body.len() >= 33
        && body[0] == 0x89
        && body[1] == 0x50
        && body[2] == 0x4e
        && body[3] == 0x47
        && body[4] == 0x0d
        && body[5] == 0x0a
        && body[6] == 0x1a
        && body[7] == 0x0a
        && body[12] == 0x49
        && body[13] == 0x48
        && body[14] == 0x44
        && body[15] == 0x52
}

pub async fn handle_request(addr: &str, pkg: &str, body: Value) {
    let req: FetchRequest = match serde_json::from_value::<FetchRequest>(body) {
        Ok(r) => r,
        Err(err) => {
            tracing::error!("invalid fetch payload: {err}");
            return;
        }
    };

    let id = req.id.clone();
    let url = req.url.clone();
    state::record_request(pkg, addr, Some(&url));
    handshake::ensure_open(addr, pkg).await;

    let options = req.options.unwrap_or_default();
    let method = options
        .method
        .as_deref()
        .unwrap_or("GET")
        .to_ascii_uppercase();
    let raw_mode = options.raw.unwrap_or(false);
    let stream_preference = options.stream;
    let follow_redirects = options.follow_redirects.unwrap_or(false);
    let fixed_chunks = options.fixed_chunks.unwrap_or(false);

    if let Some(route) = backup_route(&url) {
        match handle_backup_route(addr, pkg, &method, route, options.body) {
            Ok(resp) => {
                state::record_result(pkg, true, Some(format!("backup {}", resp.status)));
                let _ = send_response(addr, pkg, id.as_deref(), resp).await;
            }
            Err(err) => {
                tracing::warn!("backup route failed: {err}");
                state::record_result(pkg, false, Some(err.clone()));
                send_error(addr, pkg, id.as_deref(), &err).await;
            }
        }
        return;
    }

    let local_file = local_asset_file(&url);
    let remote_file = cdn_urls()
        .iter()
        .find(|(_, remote)| **remote == url.as_str())
        .map(|(file, _)| *file);
    let atlas_file = local_file.as_deref().or(remote_file);
    let expected_size = atlas_file.and_then(|file| expected_sizes().get(file).copied());
    let fetch_url = atlas_file
        .and_then(|file| cdn_urls().get(file).copied())
        .map(|remote| remote.to_string())
        .unwrap_or_else(|| url.clone());

    // 内置图集优先：编译期已嵌进 wasm，本地字节直出给手表，不再走 CDN 下载；
    // 内置清单缺失时才回落下面的网络路径兜底。
    if let Some(file) = atlas_file {
        if let Some(bytes) = atlas_assets::bundled_atlas(file) {
            if !png_body_ok(bytes) {
                let message = format!("bundled atlas png header invalid: {file}");
                tracing::warn!("{message}");
                state::record_result(pkg, false, Some(message.clone()));
                send_error(addr, pkg, id.as_deref(), &message).await;
                return;
            }
            let resp = FetchResponse {
                ok: true,
                status: 200,
                status_text: "OK",
                headers: Map::new(),
                body_bytes: bytes.to_vec(),
                raw: true,
            };
            tracing::info!(
                "atlas served bundled: pkg={} file={} bytes={}",
                pkg,
                file,
                bytes.len()
            );
            match send_response(addr, pkg, id.as_deref(), resp).await {
                Ok(()) => tracing::info!("bundled atlas dispatched: pkg={} file={}", pkg, file),
                Err(err) => state::record_result(pkg, false, Some(err)),
            }
            return;
        }
    }

    tracing::info!(
        "fetch begin: pkg={} addr={} id={} method={} url={} remote={}",
        pkg,
        addr,
        id.as_deref().unwrap_or(""),
        method,
        url,
        fetch_url
    );

    match perform_request(
        &method,
        &fetch_url,
        options.headers,
        options.body,
        raw_mode,
        follow_redirects,
    ) {
        Ok(resp) => {
            let status = resp.status;
            if let Some(expected) = expected_size {
                let declared = resp
                    .headers
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                    .and_then(|(_, value)| value.as_str())
                    .and_then(|value| value.parse::<usize>().ok());
                if declared != Some(expected) {
                    let message = format!("atlas size mismatch: expected {expected}, header {declared:?}");
                    tracing::warn!("{message}");
                    state::record_result(pkg, false, Some(message.clone()));
                    send_error(addr, pkg, id.as_deref(), &message).await;
                    return;
                }
            }

            let caps = handshake::negotiated_caps(addr, pkg);
            let use_stream = expected_size.is_none()
                && caps.as_ref().map(|c| c.stream).unwrap_or(false)
                && should_stream(&resp, stream_preference);

            let result = if use_stream {
                match id.as_deref() {
                    Some(id) if !id.is_empty() => {
                        send_streaming(addr, pkg, id, resp, caps.as_ref().unwrap(), fixed_chunks)
                            .await
                    }
                    _ => {
                        let message = "protocol v4 streaming requires a non-empty fetch id";
                        send_error(addr, pkg, id.as_deref(), message).await;
                        Err(message.to_string())
                    }
                }
            } else {
                match resp.into_buffered() {
                    Ok(resp) => {
                        if expected_size.is_some() && !png_body_ok(&resp.body_bytes) {
                            Err("atlas png header invalid".to_string())
                        } else {
                            send_response(addr, pkg, id.as_deref(), resp).await
                        }
                    }
                    Err(err) => Err(err),
                }
            };

            match result {
                Ok(()) => {
                    // 图集的成功/已同步计数以手表端落盘回执（atlas-report）为准；这里只
                    // 代表插件把响应发出去了，不代表手表已收齐、解码并写盘成好图。非图集
                    // 请求仍按单次响应成功记账。
                    if atlas_file.is_none() {
                        state::record_result(pkg, true, Some(format!("HTTP {}", status)));
                    } else {
                        tracing::info!(
                            "atlas dispatched, awaiting watch report: pkg={} file={}",
                            pkg,
                            atlas_file.unwrap_or("")
                        );
                    }
                }
                Err(err) => state::record_result(pkg, false, Some(err)),
            }
        }
        Err(err) => {
            tracing::error!("fetch error: {err}");
            state::record_result(pkg, false, Some(err.clone()));
            send_error(addr, pkg, id.as_deref(), &err).await;
        }
    }
}

fn perform_request(
    method: &str,
    url: &str,
    headers: Option<HashMap<String, Value>>,
    body: Option<String>,
    raw: bool,
    follow_redirects: bool,
) -> Result<HttpResponse, String> {
    let client = Client::new();
    let mut current_url = Url::parse(url).map_err(|e| format!("invalid URL: {e}"))?;
    let mut current_method = method.to_ascii_uppercase();
    let mut current_body = body;
    let mut headers = headers.unwrap_or_default();
    let mut redirect_count = 0;

    loop {
        let mut attempt = 0u32;
        let response = loop {
            attempt += 1;
            let mut req = client
                .request(parse_method(&current_method), current_url.as_str())
                .connect_timeout(Duration::from_secs(CONNECT_TIMEOUT_SECS));

            for (k, v) in &headers {
                let value = match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                let name = match waki::header::HeaderName::try_from(k.as_str()) {
                    Ok(name) => name,
                    Err(err) => {
                        tracing::warn!("skip header {}: {}", k, err);
                        continue;
                    }
                };
                req = req.header(name, value);
            }

            if let Some(body) = &current_body {
                req = req.body(body.as_bytes().to_vec());
            }

            match req.send() {
                Ok(response) => break response,
                Err(err) => {
                    // 图集/精灵数据下载走无 body 的 GET：弱网下偶发 HttpProtocolError
                    // 属瞬时链路问题，重试 3 次可显著降低「传输失败/停在 0%」。带 body 的
                    // 请求不重试，避免重复提交。
                    if current_body.is_none() && attempt < 3 {
                        tracing::warn!("send failed (attempt {attempt}/3): {err}");
                        continue;
                    }
                    return Err(format!("send failed: {err}"));
                }
            }
        };
        let status = response.status_code();
        if !follow_redirects || !is_redirect_status(status) {
            return into_http_response(response, raw);
        }

        let Some(location) = response.header("location") else {
            // A redirect status without Location cannot be followed; return it to
            // the caller as an ordinary HTTP response.
            return into_http_response(response, raw);
        };
        let location = location
            .to_str()
            .map_err(|e| format!("invalid redirect Location header: {e}"))?;
        if redirect_count >= MAX_REDIRECTS {
            return Err(format!("too many HTTP redirects (limit {MAX_REDIRECTS})"));
        }
        let next_url = current_url
            .join(location)
            .map_err(|e| format!("invalid redirect Location {location:?}: {e}"))?;

        // Fetch/browser semantics: POST becomes GET for 301/302; 303 becomes GET
        // for every method except HEAD. 307/308 preserve both method and body.
        if should_redirect_as_get(status, &current_method) {
            current_method = "GET".to_string();
            current_body = None;
            remove_body_headers(&mut headers);
        }
        if !same_origin(&current_url, &next_url) {
            remove_sensitive_headers(&mut headers);
        }

        redirect_count += 1;
        tracing::debug!(
            "following HTTP redirect: status={} hop={}/{} url={}",
            status,
            redirect_count,
            MAX_REDIRECTS,
            next_url,
        );
        current_url = next_url;
        drop(response);
    }
}

fn into_http_response(response: Response, raw: bool) -> Result<HttpResponse, String> {
    let status = response.status_code();
    let resp_headers_raw = response.headers().clone();
    let mut headers = Map::new();
    for (name, value) in resp_headers_raw.iter() {
        let key = name.as_str().to_string();
        let val = value.to_str().unwrap_or("").to_string();
        headers.insert(key, Value::String(val));
    }

    Ok(HttpResponse {
        response,
        ok: (200..300).contains(&status),
        status,
        status_text: status_text(status),
        headers,
        raw_requested: raw,
    })
}

fn is_redirect_status(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

fn should_redirect_as_get(status: u16, method: &str) -> bool {
    (matches!(status, 301 | 302) && method.eq_ignore_ascii_case("POST"))
        || (status == 303 && !method.eq_ignore_ascii_case("HEAD"))
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

fn remove_body_headers(headers: &mut HashMap<String, Value>) {
    headers.retain(|name, _| {
        !matches!(
            name.to_ascii_lowercase().as_str(),
            "content-length" | "content-type" | "content-encoding" | "transfer-encoding"
        )
    });
}

fn remove_sensitive_headers(headers: &mut HashMap<String, Value>) {
    headers.retain(|name, _| {
        !matches!(
            name.to_ascii_lowercase().as_str(),
            "authorization" | "proxy-authorization" | "cookie" | "host"
        )
    });
}

fn should_stream(resp: &HttpResponse, preference: Option<bool>) -> bool {
    if let Some(preference) = preference {
        return preference;
    }
    let content_type = resp
        .headers
        .get("content-type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    if content_type.starts_with("audio/") || content_type.starts_with("video/") {
        return true;
    }
    resp.headers
        .get("content-length")
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<usize>().ok())
        .map(|len| len >= AUTO_STREAM_MIN_BYTES)
        .unwrap_or(false)
}

fn stream_encoding(caps: &NegotiatedCaps) -> BodyEncoding {
    caps.encodings
        .iter()
        .copied()
        .find(|enc| matches!(enc, BodyEncoding::Base64 | BodyEncoding::Hex))
        .unwrap_or(BodyEncoding::Base64)
}

async fn send_streaming(
    addr: &str,
    pkg: &str,
    id: &str,
    resp: HttpResponse,
    caps: &NegotiatedCaps,
    fixed_chunks: bool,
) -> Result<(), String> {
    let content_type = resp
        .headers
        .get("content-type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    let raw = resp.raw_requested
        || !(content_type.starts_with("text/")
            || content_type.contains("json")
            || content_type.contains("xml")
            || content_type.contains("javascript"));
    let content_length = resp
        .headers
        .get("content-length")
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<usize>().ok());
    let encoding = stream_encoding(caps);

    let mut resp_obj = Map::new();
    resp_obj.insert("ok".into(), Value::Bool(resp.ok));
    resp_obj.insert("status".into(), Value::from(resp.status));
    resp_obj.insert("statusText".into(), Value::String(resp.status_text.into()));
    resp_obj.insert("headers".into(), Value::Object(resp.headers.clone()));
    resp_obj.insert("body".into(), Value::String(String::new()));
    resp_obj.insert("raw".into(), Value::Bool(raw));
    resp_obj.insert("stream".into(), Value::Bool(true));
    resp_obj.insert("chunkSize".into(), Value::from(caps.chunk_size));
    resp_obj.insert(
        "bodyEncoding".into(),
        Value::String(encoding.as_str().into()),
    );
    resp_obj.insert("compression".into(), Value::String("none".into()));
    resp_obj.insert("ack".into(), Value::Bool(true));
    resp_obj.insert("checksum".into(), Value::String("crc32".into()));
    if fixed_chunks {
        resp_obj.insert("fixedChunks".into(), Value::Bool(true));
    }
    if let Some(content_length) = content_length {
        resp_obj.insert("contentLength".into(), Value::from(content_length));
    }

    if !interconnect::send_json(
        addr,
        pkg,
        FETCH_TAG,
        wrap_with_id(Some(id), "resp", Value::Object(resp_obj)),
    )
    .await
    {
        return Err("failed to send v4 stream header".to_string());
    }
    stream::begin(
        addr,
        pkg,
        id,
        resp.response,
        stream::StreamConfig {
            chunk_size: caps.chunk_size,
            encoding,
            window: caps.ack_window,
            fixed_chunks,
        },
    )
    .await;
    Ok(())
}

/// Composed transfer plan for one response. Built from negotiated caps plus
/// per-body heuristics, then handed to the encoder + sender.
struct TransferPlan {
    compression: Compression,
    /// Bytes after compression (or original if `compression == None`). These
    /// are what gets chunked and wire-encoded.
    payload: Vec<u8>,
    /// Original body length in bytes, *before* compression. Reported to the
    /// peer so it can size its receive buffer.
    original_bytes: usize,
    /// Wire encoding to use for `payload`.
    encoding: BodyEncoding,
    /// `Some(chunk_size)` when the response will be split across multiple
    /// frames; chunk_size is in bytes of `payload` per chunk.
    chunk_size: Option<usize>,
    /// In-flight window for ACK-paced chunk delivery. `0` ⇒ the peer can't ACK,
    /// so we fall back to the legacy un-paced blast. Only meaningful when
    /// `chunk_size` is `Some`.
    ack_window: usize,
}

fn build_plan(resp: &FetchResponse, caps: Option<&NegotiatedCaps>) -> TransferPlan {
    let original_bytes = resp.body_bytes.len();
    let compression = pick_compression(caps, original_bytes);
    let payload = codec::compress(&resp.body_bytes, compression);

    // Decide chunking from the *post-compression* size — that's what actually
    // moves over the wire.
    let chunk_size = caps
        .and_then(|c| if c.chunked { Some(c.chunk_size) } else { None })
        .filter(|cs| payload.len() > *cs);

    let encoding = pick_encoding(caps, &payload, resp.raw, chunk_size.is_some(), compression);

    // Pace chunk delivery with ACKs whenever we're actually chunking and the
    // peer negotiated a window. Otherwise stay on the legacy blast path.
    let ack_window = if chunk_size.is_some() {
        caps.map(|c| c.ack_window).unwrap_or(0)
    } else {
        0
    };

    TransferPlan {
        compression,
        payload,
        original_bytes,
        encoding,
        chunk_size,
        ack_window,
    }
}

/// Choose the compressor to apply. Defaults to `None` whenever the peer
/// didn't advertise a list (v1/v2 baseline) or the body is too small to be
/// worth compressing.
fn pick_compression(caps: Option<&NegotiatedCaps>, body_len: usize) -> Compression {
    let Some(caps) = caps else {
        return Compression::None;
    };
    if caps.compressions.is_empty() || body_len < COMPRESS_MIN_SIZE {
        return Compression::None;
    }
    // Peer's first preference that we also implement wins. Falls back to
    // `None`, which is also always implicitly supported.
    caps.compressions
        .iter()
        .copied()
        .next()
        .unwrap_or(Compression::None)
}

/// Choose the wire encoding. Rules:
///   - `text` is only viable when the payload is valid UTF-8 AND we're not
///     chunking (we don't split across UTF-8 code points). Compressed payloads
///     are binary, so they never qualify.
///   - Otherwise honour the peer's preference order over {base64, hex}.
///   - If the peer never advertised an `encodings` list, fall back to the v1
///     baseline: `text` for plain UTF-8 text bodies, `base64` for everything
///     else.
fn pick_encoding(
    caps: Option<&NegotiatedCaps>,
    payload: &[u8],
    raw: bool,
    will_chunk: bool,
    compression: Compression,
) -> BodyEncoding {
    let text_viable = !will_chunk
        && !raw
        && compression == Compression::None
        && std::str::from_utf8(payload).is_ok();

    let peer_encs = caps.map(|c| c.encodings.as_slice()).unwrap_or(&[]);

    if peer_encs.is_empty() {
        // v1 / v2 peers: text or base64, exactly like before.
        return if text_viable {
            BodyEncoding::Text
        } else {
            BodyEncoding::Base64
        };
    }

    for &enc in peer_encs {
        match enc {
            BodyEncoding::Text if text_viable => return BodyEncoding::Text,
            BodyEncoding::Base64 | BodyEncoding::Hex => return enc,
            _ => continue,
        }
    }

    // Peer's list didn't contain anything we can satisfy under current
    // constraints (e.g. only `text` but body is binary or chunked). Base64 is
    // the universal fallback every peer is required to handle.
    BodyEncoding::Base64
}

async fn send_response(
    addr: &str,
    pkg: &str,
    id: Option<&str>,
    mut resp: FetchResponse,
) -> Result<(), String> {
    let caps = handshake::negotiated_caps(addr, pkg);
    let plan = build_plan(&resp, caps.as_ref());

    // `plan.payload` 已从原始体压缩/拷贝得到；分片传输的整个 ACK 周期里不再需要
    // 原始 body，提前释放这一份整图拷贝，压低 WASM 线性内存峰值。PNG 头校验在
    // 进入本函数之前已完成，发送路径只读元数据，不再访问 body_bytes。
    resp.body_bytes = Vec::new();

    // `chunk_size` is `Copy`, so matching on it doesn't borrow `plan` — the
    // chunked arm can take ownership and hand the payload off to `transfer`.
    match plan.chunk_size {
        Some(cs) => {
            send_chunked(addr, pkg, id, &resp, plan, cs).await;
            Ok(())
        }
        None => send_unchunked(addr, pkg, id, &resp, &plan).await,
    }
}

async fn send_unchunked(
    addr: &str,
    pkg: &str,
    id: Option<&str>,
    resp: &FetchResponse,
    plan: &TransferPlan,
) -> Result<(), String> {
    // Encoding `Text` can't fail at this point: pick_encoding only returns it
    // when the payload was checked to be valid UTF-8 and not compressed.
    let encoded = codec::encode(&plan.payload, plan.encoding)
        .unwrap_or_else(|_| codec::encode(&plan.payload, BodyEncoding::Base64).unwrap());

    if encoded.len() > MAX_UNCHUNKED_WIRE_LEN {
        tracing::error!(
            "refusing oversized unchunked fetch response: pkg={} addr={} id={} encoded={} original={} enc={} comp={}",
            pkg,
            addr,
            id.unwrap_or(""),
            encoded.len(),
            plan.original_bytes,
            plan.encoding.as_str(),
            plan.compression.as_str(),
        );
        let message = "response too large for unchunked interconnect frame; complete FetchBridge handshake with chunk=true";
        send_error(addr, pkg, id, message).await;
        return Err(message.to_string());
    }

    let mut resp_obj = Map::new();
    resp_obj.insert("ok".into(), Value::Bool(resp.ok));
    resp_obj.insert("status".into(), Value::from(resp.status));
    resp_obj.insert("statusText".into(), Value::String(resp.status_text.into()));
    resp_obj.insert("headers".into(), Value::Object(resp.headers.clone()));
    resp_obj.insert("body".into(), Value::String(encoded));
    resp_obj.insert("raw".into(), Value::Bool(resp.raw));

    // Only annotate non-default codec choices so v1/v2 peers continue to see
    // exactly the wire shape they used to. This keeps the doc-stated promise
    // that omitting `caps` keeps you on the legacy path.
    if plan.encoding != legacy_encoding_for(resp.raw) {
        resp_obj.insert(
            "bodyEncoding".into(),
            Value::String(plan.encoding.as_str().into()),
        );
    }
    if plan.compression != Compression::None {
        resp_obj.insert(
            "compression".into(),
            Value::String(plan.compression.as_str().into()),
        );
        resp_obj.insert("originalBytes".into(), Value::from(plan.original_bytes));
    }

    if interconnect::send_json(
        addr,
        pkg,
        FETCH_TAG,
        wrap_with_id(id, "resp", Value::Object(resp_obj)),
    )
    .await
    {
        Ok(())
    } else {
        Err("failed to send fetch response".to_string())
    }
}

async fn send_chunked(
    addr: &str,
    pkg: &str,
    id: Option<&str>,
    resp: &FetchResponse,
    plan: TransferPlan,
    chunk_size: usize,
) {
    // 并发兜底：在途分片响应已达上限就拒绝本次（chunked header 尚未发出），让对端走
    // 失败重试，避免多张整图 payload 同时堆进 WASM 线性内存把宿主推到 OOM。
    if transfer::in_flight_count() >= transfer::MAX_CONCURRENT_TRANSFERS {
        tracing::warn!(
            "chunked transfer rejected: in-flight >= {} (pkg={} id={})",
            transfer::MAX_CONCURRENT_TRANSFERS,
            pkg,
            id.unwrap_or(""),
        );
        send_error(addr, pkg, id, "too many concurrent transfers; retry later").await;
        return;
    }
    let total_bytes = plan.payload.len();
    let chunk_count = total_bytes.div_ceil(chunk_size);
    let ack_paced = plan.ack_window > 0;

    tracing::info!(
        "fetch chunked: pkg={} addr={} id={} original={} compressed={} chunk_size={} chunks={} enc={} comp={} ack_window={}",
        pkg,
        addr,
        id.unwrap_or(""),
        plan.original_bytes,
        total_bytes,
        chunk_size,
        chunk_count,
        plan.encoding.as_str(),
        plan.compression.as_str(),
        plan.ack_window,
    );

    // Header: keep every v1 field, then append v2 chunking metadata and any
    // v3 codec annotations. Old peers never opt into chunking so they never
    // get here in the first place.
    let mut resp_obj = Map::new();
    resp_obj.insert("ok".into(), Value::Bool(resp.ok));
    resp_obj.insert("status".into(), Value::from(resp.status));
    resp_obj.insert("statusText".into(), Value::String(resp.status_text.into()));
    resp_obj.insert("headers".into(), Value::Object(resp.headers.clone()));
    resp_obj.insert("body".into(), Value::String(String::new()));
    resp_obj.insert("raw".into(), Value::Bool(resp.raw));
    resp_obj.insert("chunked".into(), Value::Bool(true));
    // `totalBytes` keeps its v2 meaning: payload as it appears on the wire
    // (= compressed size, because that's what the peer needs to buffer).
    resp_obj.insert("totalBytes".into(), Value::from(total_bytes));
    resp_obj.insert("chunkSize".into(), Value::from(chunk_size));
    resp_obj.insert("chunkCount".into(), Value::from(chunk_count));
    resp_obj.insert(
        "bodyEncoding".into(),
        Value::String(plan.encoding.as_str().into()),
    );
    if plan.compression != Compression::None {
        resp_obj.insert(
            "compression".into(),
            Value::String(plan.compression.as_str().into()),
        );
        // `originalBytes` is the uncompressed size — handy for the peer to
        // size its post-decompression buffer up front.
        resp_obj.insert("originalBytes".into(), Value::from(plan.original_bytes));
    }
    // Tell the peer this transfer is ACK-paced so it knows to emit `fetch-ack`.
    // Optional field; legacy peers (which never negotiate `ack`) never see it.
    if ack_paced {
        resp_obj.insert("ack".into(), Value::Bool(true));
    }
    interconnect::send_json(
        addr,
        pkg,
        FETCH_TAG,
        wrap_with_id(id, "resp", Value::Object(resp_obj)),
    )
    .await;

    // Header is out (compat rule #1). Now ship the body.
    if ack_paced {
        // ACK-paced path: register the transfer and prime the first window.
        // `transfer` ships at most `window` chunks now and resumes from
        // `handle_ack` as the peer acknowledges — bounding in-flight bytes and
        // yielding control back to the host between bursts.
        transfer::begin(
            addr,
            pkg,
            id,
            plan.payload,
            chunk_size,
            plan.encoding,
            plan.ack_window,
        )
        .await;
        return;
    }

    // Legacy un-paced path: the peer can't ACK, so blast every chunk in one go,
    // exactly like v2. Fine for the modest responses such peers receive; large
    // ones are why v3 added ACK pacing above.
    for (seq, chunk) in plan.payload.chunks(chunk_size).enumerate() {
        let encoded = codec::encode(chunk, plan.encoding)
            .unwrap_or_else(|_| codec::encode(chunk, BodyEncoding::Base64).unwrap());
        let mut msg = Map::new();
        if let Some(id) = id {
            msg.insert("id".to_string(), Value::String(id.to_string()));
        }
        msg.insert("seq".to_string(), Value::from(seq));
        msg.insert("total".to_string(), Value::from(chunk_count));
        msg.insert("data".to_string(), Value::String(encoded));
        msg.insert("crc32".to_string(), Value::String(format!("{:08x}", codec::crc32(chunk))));
        interconnect::send_json(addr, pkg, FETCH_CHUNK_TAG, Value::Object(msg)).await;
    }
}

/// Handle a peer `fetch-ack` frame: `{ id?, ack }`. `ack` is the next
/// contiguous chunk index the peer still needs. Drives the sliding window so
/// the next batch of chunks goes out (see `transfer::on_ack`).
pub async fn handle_ack(addr: &str, pkg: &str, body: Value) {
    handshake::record_activity(addr, pkg);
    let id = body.get("id").and_then(|v| v.as_str());
    let ack = body.get("ack").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    tracing::debug!(
        "fetch-ack: pkg={} addr={} id={} ack={}",
        pkg,
        addr,
        id.unwrap_or(""),
        ack
    );
    transfer::on_ack(addr, pkg, id, ack).await;
}

pub async fn handle_stream_ack(addr: &str, pkg: &str, body: Value) {
    handshake::record_activity(addr, pkg);
    let Some(id) = body
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
    else {
        tracing::warn!(
            "ignored v4 stream ACK without id: pkg={} addr={}",
            pkg,
            addr
        );
        return;
    };
    let ack = body.get("ack").and_then(Value::as_u64).unwrap_or(0) as usize;
    stream::on_ack(addr, pkg, id, ack).await;
}

pub async fn handle_stream_cancel(addr: &str, pkg: &str, body: Value) {
    handshake::record_activity(addr, pkg);
    let Some(id) = body
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
    else {
        tracing::warn!(
            "ignored v4 stream cancel without id: pkg={} addr={}",
            pkg,
            addr
        );
        return;
    };
    stream::cancel(addr, pkg, id);
}

async fn send_error(addr: &str, pkg: &str, id: Option<&str>, message: &str) {
    let resp_value = json!({
        "ok": false,
        "status": 0,
        "statusText": message,
        "headers": {},
        "body": "",
        "raw": false,
    });
    interconnect::send_json(addr, pkg, FETCH_TAG, wrap_with_id(id, "resp", resp_value)).await;
}

fn wrap_with_id(id: Option<&str>, key: &str, value: Value) -> Value {
    let mut payload = Map::new();
    payload.insert(key.to_string(), value);
    if let Some(id) = id {
        payload.insert("id".to_string(), Value::String(id.to_string()));
    }
    Value::Object(payload)
}

/// The encoding a v1 peer would have produced for this body: `text` for
/// UTF-8 text responses, `base64` for raw / binary. Used to decide whether to
/// annotate `bodyEncoding` on the wire — annotating the legacy choice would
/// just be noise that a strict v1 parser shouldn't even see.
fn legacy_encoding_for(raw: bool) -> BodyEncoding {
    if raw {
        BodyEncoding::Base64
    } else {
        BodyEncoding::Text
    }
}

fn parse_method(method: &str) -> Method {
    match method.to_ascii_uppercase().as_str() {
        "GET" => Method::Get,
        "POST" => Method::Post,
        "PUT" => Method::Put,
        "DELETE" => Method::Delete,
        "HEAD" => Method::Head,
        "PATCH" => Method::Patch,
        "OPTIONS" => Method::Options,
        "CONNECT" => Method::Connect,
        "TRACE" => Method::Trace,
        other => Method::Other(other.to_string()),
    }
}

fn status_text(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        304 => "Not Modified",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_switch_is_opt_in_and_accepts_camel_case() {
        let default: FetchOptions = serde_json::from_value(json!({})).unwrap();
        assert_eq!(default.follow_redirects, None);

        let enabled: FetchOptions = serde_json::from_value(json!({
            "followRedirects": true,
            "fixedChunks": true
        }))
        .unwrap();
        assert_eq!(enabled.follow_redirects, Some(true));
        assert_eq!(enabled.fixed_chunks, Some(true));
    }

    #[test]
    fn redirect_method_rewrite_matches_fetch_semantics() {
        assert!(should_redirect_as_get(301, "POST"));
        assert!(should_redirect_as_get(302, "post"));
        assert!(should_redirect_as_get(303, "PUT"));
        assert!(!should_redirect_as_get(301, "GET"));
        assert!(!should_redirect_as_get(303, "HEAD"));
        assert!(!should_redirect_as_get(307, "POST"));
        assert!(!should_redirect_as_get(308, "POST"));
    }

    #[test]
    fn cross_origin_redirect_drops_credentials() {
        let mut headers = HashMap::from([
            ("Authorization".to_string(), Value::String("secret".into())),
            ("Cookie".to_string(), Value::String("session=1".into())),
            ("Accept".to_string(), Value::String("*/*".into())),
        ]);
        remove_sensitive_headers(&mut headers);
        assert!(!headers.contains_key("Authorization"));
        assert!(!headers.contains_key("Cookie"));
        assert!(headers.contains_key("Accept"));
    }

    #[test]
    fn origin_comparison_accounts_for_default_ports() {
        let https = Url::parse("https://example.com/a").unwrap();
        let explicit = Url::parse("https://example.com:443/b").unwrap();
        let other = Url::parse("https://other.example/b").unwrap();
        assert!(same_origin(&https, &explicit));
        assert!(!same_origin(&https, &other));
    }

    #[test]
    fn backup_route_strips_pkr_sync_namespace_prefix() {
        assert!(matches!(backup_route("local://pkr.sync/backup"), Some(BackupRoute::Save)));
        assert!(matches!(backup_route("local://pkr.sync/backup/list"), Some(BackupRoute::List)));
        assert!(matches!(backup_route("local://pkr.sync/backup/latest"), Some(BackupRoute::Latest)));
        assert!(matches!(
            backup_route("local://pkr.sync/backup/abc123"),
            Some(BackupRoute::Get(id)) if id == "abc123"
        ));
        assert!(matches!(
            backup_route("local://pkr.sync/backup/abc123/delete"),
            Some(BackupRoute::Delete(id)) if id == "abc123"
        ));
        assert!(matches!(backup_route("local://pkr.sync/atlas-report"), Some(BackupRoute::AtlasReport)));
        assert!(matches!(backup_route("local://pkr.sync/atlas-list"), Some(BackupRoute::AtlasList)));
        // 图集本地资源与外部 URL 不进备份路由。
        assert!(backup_route("local://com.pkr.handheld/front_1.png").is_none());
        assert!(backup_route("https://media.doubao.com/x.png").is_none());
    }
}

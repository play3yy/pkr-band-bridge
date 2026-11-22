use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::codec::crc32;

const BACKUP_DIR: &str = "./pkr_backups";
const INDEX_PATH: &str = "./pkr_backups/index.json";
const MAX_BACKUPS_PER_PKG: usize = 5;
const MAX_SNAPSHOT_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BackupSummary {
    pub saves: u64,
    pub bsaves: u64,
    pub unlocks: u64,
    pub candies: u64,
    pub history: u64,
    pub battles: u64,
    pub catches: u64,
    pub seen: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BackupMeta {
    pub id: String,
    pub pkg_name: String,
    pub addr: String,
    pub device_name: String,
    pub app_version: String,
    pub saved_at: u128,
    pub size: usize,
    pub checksum: String,
    pub summary: BackupSummary,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BackupRecord {
    pub meta: BackupMeta,
    pub data: Value,
}

#[derive(Debug, Deserialize, Serialize)]
struct OnDiskIndex {
    version: u32,
    backups: Vec<BackupMeta>,
}

pub fn save_backup(
    pkg: &str,
    addr: &str,
    device_name: &str,
    snapshot: Value,
) -> Result<BackupMeta, String> {
    let data = snapshot
        .get("data")
        .and_then(Value::as_object)
        .ok_or_else(|| "backup data missing".to_string())?;

    let saved_at = snapshot
        .get("savedAt")
        .and_then(Value::as_u64)
        .ok_or_else(|| "backup savedAt missing".to_string())?;
    let app_version = snapshot
        .get("appVersion")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let claimed_checksum = snapshot
        .get("checksum")
        .and_then(Value::as_str)
        .ok_or_else(|| "backup checksum missing".to_string())?
        .to_string();

    let canonical = serde_json::to_string(&Value::Object(data.clone()))
        .map_err(|err| format!("backup serialize failed: {err}"))?;
    let actual_checksum = format!("{:08x}", crc32(canonical.as_bytes()));
    if actual_checksum != claimed_checksum {
        return Err(format!("backup checksum mismatch: {claimed_checksum} != {actual_checksum}"));
    }

    let size = canonical.len();
    if size > MAX_SNAPSHOT_BYTES {
        return Err(format!("backup too large: {size} bytes"));
    }

    validate_data(data)?;
    let summary = summarize(data);
    let id = format!("bk-{saved_at}-{actual_checksum}");
    let meta = BackupMeta {
        id: id.clone(),
        pkg_name: pkg.to_string(),
        addr: addr.to_string(),
        device_name: device_name.to_string(),
        app_version,
        saved_at: saved_at as u128,
        size,
        checksum: actual_checksum,
        summary,
    };

    let record = BackupRecord {
        meta: meta.clone(),
        data: Value::Object(data.clone()),
    };
    write_record(&record)?;

    let mut index = load_index();
    index.retain(|existing| existing.id != id);
    index.push(meta.clone());
    index.sort_by(|a, b| b.saved_at.cmp(&a.saved_at));
    prune_index(pkg, &mut index);
    save_index(&index)?;

    Ok(meta)
}

pub fn list_backups(pkg: Option<&str>) -> Vec<BackupMeta> {
    let mut index = load_index();
    if let Some(pkg) = pkg {
        index.retain(|meta| meta.pkg_name == pkg);
    }
    index.sort_by(|a, b| b.saved_at.cmp(&a.saved_at));
    index
}

pub fn latest_backup(pkg: &str) -> Option<BackupRecord> {
    list_backups(Some(pkg)).into_iter().next().and_then(|meta| load_record(&meta.id))
}

pub fn backup_by_id(id: &str) -> Option<BackupRecord> {
    load_record(id)
}

pub fn delete_backup(id: &str) -> bool {
    let mut index = load_index();
    let before = index.len();
    index.retain(|meta| meta.id != id);
    if index.len() == before {
        return false;
    }

    let _ = fs::remove_file(record_path(id));
    let _ = save_index(&index);
    true
}

fn validate_data(data: &serde_json::Map<String, Value>) -> Result<(), String> {
    let _meta = data.get("meta").and_then(Value::as_object).ok_or("meta must be object")?;
    let _stats = data.get("stats").and_then(Value::as_object).ok_or("stats must be object")?;
    let history = data.get("history").and_then(Value::as_array).ok_or("history must be array")?;
    let _fav = data.get("fav").and_then(Value::as_array).ok_or("fav must be array")?;
    let _tut = data.get("tut").and_then(Value::as_array).ok_or("tut must be array")?;

    // 新备份不再携带 saves/bsaves（只备份解锁/糖果/图鉴等 meta），旧备份里这两个字段
    // 若存在也不强制——恢复侧一律忽略存档槽，所以这里不再校验。
    if history.len() > 10 {
        return Err("history exceeds 10 records".to_string());
    }
    Ok(())
}

fn summarize(data: &serde_json::Map<String, Value>) -> BackupSummary {
    let arr_len = |key: &str| data.get(key).and_then(Value::as_array).map(|v| v.len() as u64).unwrap_or(0);
    let stats = data.get("stats").and_then(Value::as_object);

    let number = |key: &str| stats
        .and_then(|s| s.get(key))
        .and_then(Value::as_u64)
        .unwrap_or(0);

    let unlocks = data
        .get("meta")
        .and_then(Value::as_object)
        .and_then(|m| m.get("unlocks"))
        .and_then(Value::as_array)
        .map(|v| v.len() as u64)
        .unwrap_or(0);
    let candies = data
        .get("meta")
        .and_then(Value::as_object)
        .and_then(|m| m.get("candy"))
        .and_then(Value::as_object)
        .map(|v| v.len() as u64)
        .unwrap_or(0);

    BackupSummary {
        saves: 0,
        bsaves: 0,
        unlocks,
        candies,
        history: arr_len("history"),
        battles: number("battles"),
        catches: number("catches"),
        seen: number("seen"),
    }
}

fn prune_index(pkg: &str, index: &mut Vec<BackupMeta>) {
    let mut kept = Vec::new();
    let mut per_pkg: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for meta in index.iter() {
        let count = per_pkg.entry(meta.pkg_name.clone()).or_insert(0);
        if meta.pkg_name == pkg && *count >= MAX_BACKUPS_PER_PKG {
            let _ = fs::remove_file(record_path(&meta.id));
            continue;
        }
        *count += 1;
        kept.push(meta.clone());
    }
    *index = kept;
}

fn load_index() -> Vec<BackupMeta> {
    let path = Path::new(INDEX_PATH);
    if !path.exists() {
        return Vec::new();
    }
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(_) => return Vec::new(),
    };
    let parsed: OnDiskIndex = match serde_json::from_str(&text) {
        Ok(index) => index,
        Err(_) => return Vec::new(),
    };
    parsed.backups
}

fn save_index(index: &[BackupMeta]) -> Result<(), String> {
    let on_disk = OnDiskIndex {
        version: 1,
        backups: index.to_vec(),
    };
    let text = serde_json::to_string_pretty(&on_disk)
        .map_err(|err| format!("index serialize failed: {err}"))?;
    ensure_dir()?;
    write_text(INDEX_PATH, &text)
}

fn write_record(record: &BackupRecord) -> Result<(), String> {
    ensure_dir()?;
    let text = serde_json::to_string_pretty(record)
        .map_err(|err| format!("backup serialize failed: {err}"))?;
    write_text(&record_path_string(&record.meta.id), &text)
}

fn load_record(id: &str) -> Option<BackupRecord> {
    if !safe_id(id) {
        return None;
    }
    let text = fs::read_to_string(record_path(id)).ok()?;
    let record: BackupRecord = serde_json::from_str(&text).ok()?;
    Some(record)
}

fn record_path(id: &str) -> PathBuf {
    Path::new(BACKUP_DIR).join(format!("{id}.json"))
}

fn record_path_string(id: &str) -> String {
    format!("{BACKUP_DIR}/{id}.json")
}

fn ensure_dir() -> Result<(), String> {
    fs::create_dir_all(BACKUP_DIR).map_err(|err| format!("create backup dir failed: {err}"))
}

fn write_text(path: &str, text: &str) -> Result<(), String> {
    let tmp = format!("{path}.tmp");
    fs::write(&tmp, text).map_err(|err| format!("write {path} failed: {err}"))?;
    if fs::rename(&tmp, path).is_err() {
        fs::write(path, text).map_err(|err| format!("write {path} failed: {err}"))?;
    }
    Ok(())
}

fn safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

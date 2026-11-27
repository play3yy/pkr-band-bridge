//! 局末成绩收集与排行榜。
//!
//! 手表快应用在每一局结束（胜利 / 全灭 / 放弃）时通过 interconnect 发一帧
//! `{tag:"pkr-score", mode, floor, win, durMs, money, date, party[], rules}`，
//! 插件按 (addr, date, mode, floor) 去重后落到 `./pkr_scores.json`，
//! 再按模式给出「最高层」榜与「最快通关」榜。
//!
//! 之所以由手表主动上报而不是从备份快照里扒历史：备份里的 history 只保留
//! 胜利局且上限 10 条，看不出失败局打到多少层，排榜没有意义。

use std::fs;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use waki::{header, Client, Method};

const SCORES_PATH: &str = "./pkr_scores.json";
const NICK_PATH: &str = "./pkr_nick.json";
/// 昵称最长 12 字（中文按字符计），超出截断，避免榜上一行被撑爆。
const NICK_MAX_CHARS: usize = 12;

static NICK_CELL: std::sync::OnceLock<std::sync::Mutex<String>> = std::sync::OnceLock::new();

fn nick_cell() -> &'static std::sync::Mutex<String> {
    NICK_CELL.get_or_init(|| {
        let raw = fs::read_to_string(NICK_PATH).unwrap_or_default();
        let name = serde_json::from_str::<Value>(&raw)
            .ok()
            .and_then(|v| v.get("name").and_then(|s| s.as_str()).map(str::to_string))
            .unwrap_or_default();
        std::sync::Mutex::new(name)
    })
}

/// 当前昵称（空串表示没设，回退到手表上报的名字 / 设备名）。
pub fn nickname() -> String {
    nick_cell().lock().map(|g| g.clone()).unwrap_or_default()
}

/// 随机昵称：不用键盘时也能有一条独立的名字（训练家 + 4 位数字）。
pub fn random_nickname() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("训练家{:04}", secs % 10000)
}

pub fn set_nickname(name: &str) {
    let trimmed: String = name.trim().chars().take(NICK_MAX_CHARS).collect();
    if let Ok(mut g) = nick_cell().lock() {
        *g = trimmed.clone();
    }
    let payload = serde_json::json!({ "name": trimmed }).to_string();
    if let Err(err) = fs::write(NICK_PATH, payload) {
        tracing::warn!("save nickname failed: {err}");
    }
}
const VERSION: u32 = 1;
/// 全局最多保留的条数，避免长期使用后文件无限增长。
const MAX_TOTAL: usize = 240;
/// 单台设备最多保留的条数。
const MAX_PER_ADDR: usize = 40;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScoreEntry {
    #[serde(default)]
    pub addr: String,
    #[serde(default)]
    pub device: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub floor: u32,
    #[serde(default)]
    pub win: bool,
    #[serde(default)]
    pub dur_ms: u64,
    #[serde(default)]
    pub money: u32,
    #[serde(default)]
    pub date: u64,
    #[serde(default)]
    pub party: Vec<String>,
    /// 挑战词条摘要（空字符串代表标准规则局）。
    #[serde(default)]
    pub rules: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct OnDisk {
    version: u32,
    #[serde(default)]
    scores: Vec<ScoreEntry>,
}

pub fn load() -> Vec<ScoreEntry> {
    let path = Path::new(SCORES_PATH);
    if !path.exists() {
        return Vec::new();
    }
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(err) => {
            tracing::warn!("scores: read {} failed: {}", SCORES_PATH, err);
            return Vec::new();
        }
    };
    let on_disk: OnDisk = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!("scores: parse {} failed: {}", SCORES_PATH, err);
            return Vec::new();
        }
    };
    if on_disk.version != VERSION {
        tracing::warn!("scores: version {} ignored", on_disk.version);
        return Vec::new();
    }
    on_disk.scores
}

fn save(list: &[ScoreEntry]) {
    let payload = OnDisk {
        version: VERSION,
        scores: list.to_vec(),
    };
    let text = match serde_json::to_string(&payload) {
        Ok(t) => t,
        Err(err) => {
            tracing::error!("scores: serialize failed: {}", err);
            return;
        }
    };
    let tmp = format!("{}.tmp", SCORES_PATH);
    if fs::write(&tmp, &text).is_err() {
        tracing::error!("scores: write tmp failed");
        return;
    }
    if fs::rename(&tmp, SCORES_PATH).is_err() {
        if fs::write(SCORES_PATH, &text).is_err() {
            tracing::error!("scores: write {} failed", SCORES_PATH);
        }
    }
}

fn text(body: &Value, key: &str) -> String {
    body.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string()
}

fn number(body: &Value, key: &str) -> u64 {
    body.get(key)
        .and_then(|v| v.as_u64())
        .or_else(|| {
            body.get(key)
                .and_then(|v| v.as_f64())
                .map(|v| v.max(0.0) as u64)
        })
        .unwrap_or(0)
}

/// 收下一帧上报。重复帧（手表补传、重试）按 (addr,date,mode,floor) 去重。
pub fn ingest(addr: &str, device: &str, body: &Value) -> Option<ScoreEntry> {
    let mode = {
        let m = text(body, "mode");
        if m.is_empty() { "classic".to_string() } else { m }
    };
    let date = number(body, "date");
    let floor = number(body, "floor").min(9999) as u32;

    let mut list = load();
    let dup = list.iter().any(|e| {
        e.addr == addr && e.date == date && e.mode == mode && e.floor == floor
    });
    if dup {
        tracing::info!("scores: duplicate report ignored (addr={addr} date={date})");
        return None;
    }

    let party = body
        .get("party")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .take(6)
                .map(|s| s.chars().take(24).collect::<String>())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let preferred = nickname();
    let entry = ScoreEntry {
        addr: addr.to_string(),
        device: device.to_string(),
        name: if preferred.is_empty() { text(body, "name") } else { preferred },
        mode,
        floor,
        win: body.get("win").and_then(|v| v.as_bool()).unwrap_or(false),
        dur_ms: number(body, "durMs"),
        money: number(body, "money").min(u32::MAX as u64) as u32,
        date,
        party,
        rules: text(body, "rules"),
    };
    list.push(entry.clone());

    // 单设备限流：只留这台设备最近 MAX_PER_ADDR 条，避免一台机器刷满整个榜。
    let mut kept_per_addr = 0usize;
    let mut filtered: Vec<ScoreEntry> = list
        .iter()
        .rev()
        .filter(|e| {
            if e.addr == addr {
                kept_per_addr += 1;
                kept_per_addr <= MAX_PER_ADDR
            } else {
                true
            }
        })
        .cloned()
        .collect();
    filtered.reverse();
    if filtered.len() > MAX_TOTAL {
        let drop_n = filtered.len() - MAX_TOTAL;
        filtered.drain(0..drop_n);
    }

    save(&filtered);
    tracing::info!(
        "scores: stored mode={} floor={} win={} total={}",
        text(body, "mode"),
        floor,
        body.get("win").and_then(|v| v.as_bool()).unwrap_or(false),
        filtered.len()
    );
    Some(entry)
}

/// 指定模式的最高层榜：层数降序，同层用时短的在前，再按时间新的在前。
pub fn board(mode: &str, limit: usize) -> Vec<ScoreEntry> {
    let mut list: Vec<ScoreEntry> = load().into_iter().filter(|e| e.mode == mode).collect();
    list.sort_by(|a, b| {
        b.floor
            .cmp(&a.floor)
            .then(a.dur_ms.cmp(&b.dur_ms))
            .then(b.date.cmp(&a.date))
    });
    list.truncate(limit);
    list
}

/// 最快通关榜：只看通关（win）的经典局，用时升序。
pub fn fastest(limit: usize) -> Vec<ScoreEntry> {
    let mut list: Vec<ScoreEntry> = load()
        .into_iter()
        .filter(|e| e.win && e.mode == "classic" && e.dur_ms > 0)
        .collect();
    list.sort_by(|a, b| a.dur_ms.cmp(&b.dur_ms).then(b.date.cmp(&a.date)));
    list.truncate(limit);
    list
}

pub fn total_count() -> usize {
    load().len()
}

/// 最近一局的成绩，用于界面顶部一句话摘要。
pub fn latest() -> Option<ScoreEntry> {
    let mut list = load();
    list.sort_by(|a, b| b.date.cmp(&a.date));
    list.into_iter().next()
}

/// 展示用名字：优先该条成绩上报时记录的名字（entry.name），
/// 其次本机昵称兜底，再其次设备名，最后地址尾号。
/// 注意：不能用本机昵称覆盖别人的条目，否则全球榜会把所有人都显示成本机名字。
pub fn display_name(entry: &ScoreEntry) -> String {
    if !entry.name.is_empty() {
        return entry.name.clone();
    }
    let nick = nickname();
    if !nick.is_empty() {
        return nick;
    }
    if !entry.device.is_empty() {
        return entry.device.clone();
    }
    let tail: String = entry
        .addr
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if tail.is_empty() { "未知设备".to_string() } else { format!("设备{tail}") }
}

// —— 在线榜（Upstash Redis REST）——
//
// 三个有序集合：lb:classic / lb:endless 按「层数优先、同层用时短优先」编码分数；
// lb:fast 只收通关局，分数即用时秒，取最小。member 是紧凑 JSON（长短键省体积）。
// 重发同一局 -> member 相同 -> ZADD 覆盖，天然幂等。

pub const CLOUD_URL: &str = "https://teaching-wahoo-211724.upstash.io";
pub const CLOUD_TOKEN: &str = "gQAAAAAAAzsMAAIgcDExOGJmYThhZGFlN2I0NTMwYTM3YTUyOGM4ZWRmOGJlMQ";

pub fn cloud_enabled() -> bool {
    !CLOUD_URL.is_empty() && !CLOUD_TOKEN.is_empty()
}

fn floor_score(e: &ScoreEntry) -> u64 {
    let sec = (e.dur_ms / 1000).min(999_999);
    (e.floor as u64) * 1_000_000 + (1_000_000 - sec)
}

fn member_of(e: &ScoreEntry) -> String {
    serde_json::json!({
        "fp": e.addr,
        "n": e.name,
        "d": e.device,
        "f": e.floor,
        "w": if e.win { 1 } else { 0 },
        "ms": e.dur_ms,
        "m": e.money,
        "t": e.date,
        "p": e.party.join(","),
        "r": e.rules
    })
    .to_string()
}

fn decode_member(raw: &Value, mode: &str) -> Option<ScoreEntry> {
    let text = raw.as_str()?;
    let v: Value = serde_json::from_str(text).ok()?;
    Some(ScoreEntry {
        addr: v.get("fp").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        name: v.get("n").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        device: v.get("d").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        mode: mode.to_string(),
        floor: v.get("f").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
        win: v.get("w").and_then(|x| x.as_u64()).unwrap_or(0) > 0,
        dur_ms: v.get("ms").and_then(|x| x.as_u64()).unwrap_or(0),
        money: v.get("m").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
        date: v.get("t").and_then(|x| x.as_u64()).unwrap_or(0),
        party: v
            .get("p")
            .and_then(|x| x.as_str())
            .map(|s| s.split(',').filter(|p| !p.is_empty()).map(str::to_string).collect())
            .unwrap_or_default(),
        rules: v.get("r").and_then(|x| x.as_str()).unwrap_or("").to_string(),
    })
}

async fn pipeline(cmds: &[Vec<String>]) -> Result<String, String> {
    let body = serde_json::to_string(cmds).map_err(|e| e.to_string())?;
    let client = Client::new();
    let url = format!("{CLOUD_URL}/pipeline");
    let auth = header::HeaderName::try_from("authorization").map_err(|e| e.to_string())?;
    let ctype = header::HeaderName::try_from("content-type").map_err(|e| e.to_string())?;
    let req = client
        .request(Method::Post, &url)
        .connect_timeout(Duration::from_secs(8))
        .header(auth, format!("Bearer {CLOUD_TOKEN}"))
        .header(ctype, "application/json".to_string())
        .body(body.into_bytes());

    match req.send() {
        Ok(resp) => {
            let status = resp.status_code();
            let bytes = resp.body().map_err(|e| format!("read body: {e}"))?;
            let text = String::from_utf8_lossy(&bytes).into_owned();
            if !(200..300).contains(&status) {
                return Err(format!("http {status}: {}", text.chars().take(120).collect::<String>()));
            }
            Ok(text)
        }
        Err(err) => Err(format!("send failed: {err}")),
    }
}

/// 读一个榜的全部成员（含分数）。榜规模在几百条量级，一次全量足够。
async fn read_board(key: &str) -> Vec<(f64, String)> {
    let cmds = vec![vec![
        "ZRANGE".to_string(),
        key.to_string(),
        "0".to_string(),
        "-1".to_string(),
        "WITHSCORES".to_string(),
    ]];
    let Ok(text) = pipeline(&cmds).await else {
        return Vec::new();
    };
    let Some(v) = serde_json::from_str::<Value>(&text).ok() else {
        return Vec::new();
    };
    let Some(items) = v
        .as_array()
        .and_then(|a| a.first())
        .and_then(|o| o.get("result"))
        .and_then(|r| r.as_array())
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < items.len() {
        let member = items[i].as_str().unwrap_or("").to_string();
        let score = items[i + 1]
            .as_str()
            .and_then(|x| x.parse::<f64>().ok())
            .or_else(|| items[i + 1].as_f64())
            .unwrap_or(f64::NAN);
        if !member.is_empty() && !score.is_nan() {
            out.push((score, member));
        }
        i += 2;
    }
    out
}

fn member_fp(member: &str) -> Option<String> {
    let v: Value = serde_json::from_str(member).ok()?;
    v.get("fp").and_then(|x| x.as_str()).map(str::to_string)
}

/// 每个设备在每个榜只留一条成绩：新成绩更好就替换旧的，更差就不上传。
/// `higher_better` 区分「层数越高越好」与「用时越短越好」。
async fn submit_one(key: &str, score: u64, member: &str, fp: &str, higher_better: bool) {
    let existing = read_board(key).await;
    let mut victims: Vec<String> = Vec::new();
    let mut best: Option<f64> = None;
    for (s, m) in existing {
        if member_fp(&m).as_deref() == Some(fp) {
            victims.push(m);
            best = Some(match best {
                Some(b) if higher_better => b.max(s),
                Some(b) => b.min(s),
                None => s,
            });
        }
    }

    let new_score = score as f64;
    if let Some(b) = best {
        let better = if higher_better {
            new_score > b
        } else {
            new_score < b
        };
        if !better {
            // 没变好：顺手清掉同一设备的重复条目（保留原来那几条里最好的）
            if victims.len() > 1 {
                let mut cmd = vec!["ZREM".to_string(), key.to_string()];
                cmd.extend(victims);
                let _ = pipeline(&[cmd]).await;
            }
            return;
        }
    }

    let mut cmds: Vec<Vec<String>> = Vec::new();
    if !victims.is_empty() {
        let mut cmd = vec!["ZREM".to_string(), key.to_string()];
        cmd.extend(victims);
        cmds.push(cmd);
    }
    cmds.push(vec![
        "ZADD".to_string(),
        key.to_string(),
        score.to_string(),
        member.to_string(),
    ]);
    if let Err(err) = pipeline(&cmds).await {
        tracing::warn!("cloud_push {key} failed: {err}");
    }
}

/// 把一局成绩推到在线榜。失败只记日志，不影响本地榜。
/// 每个设备每榜只保留最好的一条。
pub async fn cloud_push(e: &ScoreEntry) -> bool {
    if !cloud_enabled() {
        return false;
    }
    let member = member_of(e);
    let main_key = format!("lb:{}", e.mode);
    submit_one(&main_key, floor_score(e), &member, &e.addr, true).await;
    if e.win && e.mode == "classic" && e.dur_ms > 0 {
        submit_one("lb:fast", e.dur_ms / 1000, &member, &e.addr, false).await;
    }
    true
}

// —— 昵称跨设备持久化 ——
//
// 插件的沙箱文件会随重装被清空，所以昵称要额外存一份到云端（HASH nick:field=fp），
// 并把榜内已有记录的 n 字段就地改名，避免「本地改了、全球榜还是旧名」。

const NICK_KEY: &str = "nick";

fn with_name(member: &str, name: &str) -> Option<String> {
    let mut v: Value = serde_json::from_str(member).ok()?;
    v["n"] = Value::String(name.to_string());
    Some(v.to_string())
}

async fn cloud_nick_get(fp: &str) -> Option<String> {
    let text = pipeline(&[vec!["HGET".to_string(), NICK_KEY.to_string(), fp.to_string()]]).await.ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let r = v.as_array()?.first()?.get("result")?;
    r.as_str().map(str::to_string)
}

async fn cloud_nick_store(fp: &str, name: &str) -> bool {
    let cmds = vec![vec![
        "HSET".to_string(),
        NICK_KEY.to_string(),
        fp.to_string(),
        name.to_string(),
    ]];
    match pipeline(&cmds).await {
        Ok(_) => true,
        Err(err) => {
            tracing::warn!("cloud_nick_store failed: {err}");
            false
        }
    }
}

/// 把榜内同一设备的记录改成新昵称（分数不变，只替换 member）。
pub async fn cloud_rename(fp: &str, name: &str) {
    if !cloud_enabled() || fp.is_empty() {
        return;
    }
    let mut cmds: Vec<Vec<String>> = Vec::new();
    for key in ["lb:classic", "lb:endless", "lb:fast"] {
        let board = read_board(key).await;
        let mut victims: Vec<String> = Vec::new();
        let mut adds: Vec<(u64, String)> = Vec::new();
        for (score, member) in board {
            if member_fp(&member).as_deref() != Some(fp) {
                continue;
            }
            let Some(renamed) = with_name(&member, name) else { continue };
            if renamed == member {
                continue;
            }
            victims.push(member);
            adds.push((score as u64, renamed));
        }
        if victims.is_empty() {
            continue;
        }
        let mut zrem = vec!["ZREM".to_string(), key.to_string()];
        zrem.extend(victims);
        cmds.push(zrem);
        for (score, member) in adds {
            cmds.push(vec![
                "ZADD".to_string(),
                key.to_string(),
                score.to_string(),
                member,
            ]);
        }
    }
    if cmds.is_empty() {
        return;
    }
    if let Err(err) = pipeline(&cmds).await {
        tracing::warn!("cloud_rename failed: {err}");
    }
}

/// 启动/首次渲染时对齐昵称：本地空则从云端恢复，本地有则推上去并刷新榜内名字。
/// 只跑一次；没有已连接设备时留待下一次。
pub async fn ensure_nick_from_cloud() {
    static ONCE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if ONCE.get().is_some() || !cloud_enabled() {
        return;
    }
    let fp = crate::state::connected_devices()
        .into_iter()
        .next()
        .map(|(addr, _)| addr)
        .unwrap_or_default();
    if fp.is_empty() {
        return;
    }
    let local = nickname();
    match (local.trim().is_empty(), cloud_nick_get(&fp).await) {
        (true, Some(c)) if !c.trim().is_empty() => {
            set_nickname(&c);
            crate::state::with_state(|s| s.nick_draft = nickname());
            tracing::info!("昵称已从云端恢复：{c}");
        }
        (false, cloud) => {
            if cloud.as_deref() != Some(local.as_str()) {
                let _ = cloud_nick_store(&fp, &local).await;
            }
            cloud_rename(&fp, &local).await;
        }
        // 本地和云端都没名字：保持空白，等用户自己输入
        (true, _) => {}
    }
    let _ = ONCE.set(true);
}

/// 保存昵称后调用：写云端并改写榜内记录，返回是否连上云端。
pub async fn push_nick_to_cloud() -> bool {
    let fp = crate::state::connected_devices()
        .into_iter()
        .next()
        .map(|(addr, _)| addr)
        .unwrap_or_default();
    let name = nickname();
    if fp.is_empty() || name.trim().is_empty() || !cloud_enabled() {
        return false;
    }
    let ok = cloud_nick_store(&fp, &name).await;
    cloud_rename(&fp, &name).await;
    ok
}

type Triple = (Vec<ScoreEntry>, Vec<ScoreEntry>, Vec<ScoreEntry>);

/// 手动同步：忽略节流，立刻拉一次云端榜。返回错误供 UI 原样展示。
pub async fn force_refresh_cloud() -> Result<Triple, String> {
    if !cloud_enabled() {
        return Err("未配置云端地址".to_string());
    }
    cloud_refresh().await
}

/// 到点就刷一次在线榜（60s 节流）。UI 渲染前调用，保证卡片一打开就有结果，
/// 不依赖 host 是否发 Timer。
pub async fn maybe_refresh_cloud() {
    ensure_nick_from_cloud().await;
    if !cloud_enabled() || !crate::state::cloud_due() {
        return;
    }
    match cloud_refresh().await {
        Ok((classic, endless, fastest)) => {
            crate::state::cloud_store(classic, endless, fastest, None);
        }
        Err(err) => {
            tracing::warn!("cloud_refresh failed: {err}");
            crate::state::cloud_store(Vec::new(), Vec::new(), Vec::new(), Some(err));
        }
    }
}

/// 一次性拉三个榜：经典最高层、无尽最高层、最快通关。
pub async fn cloud_refresh() -> Result<Triple, String> {
    if !cloud_enabled() {
        return Err("cloud disabled".to_string());
    }
    let cmds = vec![
        vec!["ZREVRANGE".to_string(), "lb:classic".into(), "0".into(), "19".into()],
        vec!["ZREVRANGE".to_string(), "lb:endless".into(), "0".into(), "19".into()],
        vec!["ZRANGE".to_string(), "lb:fast".into(), "0".into(), "9".into()],
    ];
    let text = pipeline(&cmds).await?;
    let replies: Vec<Value> = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let mut out: Triple = (Vec::new(), Vec::new(), Vec::new());
    for (idx, reply) in replies.iter().enumerate() {
        let members = reply.get("result").and_then(|r| r.as_array()).cloned().unwrap_or_default();
        let dmode = match idx { 0 => "classic", 1 => "endless", _ => "classic" };
        let parsed: Vec<ScoreEntry> = members.iter().filter_map(|m| decode_member(m, dmode)).collect();
        match idx {
            0 => out.0 = parsed,
            1 => out.1 = parsed,
            _ => out.2 = parsed,
        }
    }
    Ok(out)
}

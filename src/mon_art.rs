//! 角色小图：图鉴编号 → 24x24 PNG 的 data URL。
//!
//! 素材是离线从手表图集（front_atlasXX_v1.png，48x48 格、5 列、每张 80 只看 1025 只）
//! 裁出来的，底色直接叠成卡片底色，免得在深色卡片上出现方块边。全部内嵌在 wasm 里，
//! 不依赖网络，也不依赖手表把图传过来。

#[path = "mon_art_data.rs"]
mod mon_art_data;

/// 图鉴总数（当前图集只到 1025）。
pub const MAX_DEX: u32 = 1025;

fn png_bytes(id: u32) -> Option<&'static [u8]> {
    if id == 0 || id > MAX_DEX {
        return None;
    }
    let i = id as usize;
    // offsets 是「每张起点」，共 1026 项：第 i 张 = [i-1, i)。
    let start = mon_art_data::MON_OFFSETS.get(i - 1).copied()? as usize;
    let end = mon_art_data::MON_OFFSETS.get(i).copied()? as usize;
    mon_art_data::MON_SHEET.get(start..end)
}

/// 标准 base64（无换行）。只手写一份，省掉一个依赖。
fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len() / 3 * 4 + 4);
    let mut chunks = data.chunks_exact(3);
    for chunk in &mut chunks {
        let v = ((chunk[0] as u32) << 16) | ((chunk[1] as u32) << 8) | chunk[2] as u32;
        out.push(TABLE[(v >> 18) as usize & 63] as char);
        out.push(TABLE[(v >> 12) as usize & 63] as char);
        out.push(TABLE[(v >> 6) as usize & 63] as char);
        out.push(TABLE[v as usize & 63] as char);
    }
    match chunks.remainder() {
        [a] => {
            let v = (*a as u32) << 16;
            out.push(TABLE[(v >> 18) as usize & 63] as char);
            out.push(TABLE[(v >> 12) as usize & 63] as char);
            out.push('=');
            out.push('=');
        }
        [a, b] => {
            let v = ((*a as u32) << 16) | ((*b as u32) << 8);
            out.push(TABLE[(v >> 18) as usize & 63] as char);
            out.push(TABLE[(v >> 12) as usize & 63] as char);
            out.push(TABLE[(v >> 6) as usize & 63] as char);
            out.push('=');
        }
        _ => {}
    }
    out
}

/// 给 UI 的 IMAGE 元素用：把 PNG 落到插件工作目录，返回相对路径。
///
/// 宿主 UI 的 IMAGE 不吃 data URL，只能给文件路径；1025 张一次性写太浪费，
/// 按需写（榜上出现某只才落一个文件），第二次直接命中已有文件。
/// 安装包内的相对路径（与 manifest 的 additional_files 一起打进 abp）。
pub fn sprite_file(id: u32) -> Option<String> {
    png_bytes(id)?;
    Some(format!("mon/{id}.png"))
}

/// 解包目录的绝对路径：宿主若按文件系统读图，这条更可能命中。
pub fn sprite_abs(id: u32) -> Option<String> {
    png_bytes(id)?;
    let base = std::env::current_dir().ok()?;
    let p = base.join("assets").join("mon").join(format!("{id}.png"));
    Some(p.to_string_lossy().into_owned())
}

/// 备用：data URL（宿主若支持内联图片可直接用）。
pub fn sprite_data_url(id: u32) -> Option<String> {
    let bytes = png_bytes(id)?;
    Some(format!("data:image/png;base64,{}", base64_encode(bytes)))
}

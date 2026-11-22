//! 编译期内嵌的精灵图集（固定静态资源）。
//! 插件收到图集请求时优先用本地字节直出，不再每次走 CDN；内置缺失才回落网络下载。

pub(crate) fn bundled_atlas(file: &str) -> Option<&'static [u8]> {
    match file {
        "back_atlas00_v1.png" => Some(include_bytes!("../assets/atlas/back_atlas00_v1.png")),
        "back_atlas01_v1.png" => Some(include_bytes!("../assets/atlas/back_atlas01_v1.png")),
        "back_atlas02_v1.png" => Some(include_bytes!("../assets/atlas/back_atlas02_v1.png")),
        "back_atlas03_v1.png" => Some(include_bytes!("../assets/atlas/back_atlas03_v1.png")),
        "back_atlas04_v1.png" => Some(include_bytes!("../assets/atlas/back_atlas04_v1.png")),
        "back_atlas05_v1.png" => Some(include_bytes!("../assets/atlas/back_atlas05_v1.png")),
        "back_atlas06_v1.png" => Some(include_bytes!("../assets/atlas/back_atlas06_v1.png")),
        "back_atlas07_v1.png" => Some(include_bytes!("../assets/atlas/back_atlas07_v1.png")),
        "back_atlas08_v1.png" => Some(include_bytes!("../assets/atlas/back_atlas08_v1.png")),
        "back_atlas09_v1.png" => Some(include_bytes!("../assets/atlas/back_atlas09_v1.png")),
        "back_atlas10_v1.png" => Some(include_bytes!("../assets/atlas/back_atlas10_v1.png")),
        "back_atlas11_v1.png" => Some(include_bytes!("../assets/atlas/back_atlas11_v1.png")),
        "back_atlas12_v1.png" => Some(include_bytes!("../assets/atlas/back_atlas12_v1.png")),
        "forms_atlas0_v1.png" => Some(include_bytes!("../assets/atlas/forms_atlas0_v1.png")),
        "front_atlas00_v1.png" => Some(include_bytes!("../assets/atlas/front_atlas00_v1.png")),
        "front_atlas01_v1.png" => Some(include_bytes!("../assets/atlas/front_atlas01_v1.png")),
        "front_atlas02_v1.png" => Some(include_bytes!("../assets/atlas/front_atlas02_v1.png")),
        "front_atlas03_v1.png" => Some(include_bytes!("../assets/atlas/front_atlas03_v1.png")),
        "front_atlas04_v1.png" => Some(include_bytes!("../assets/atlas/front_atlas04_v1.png")),
        "front_atlas05_v1.png" => Some(include_bytes!("../assets/atlas/front_atlas05_v1.png")),
        "front_atlas06_v1.png" => Some(include_bytes!("../assets/atlas/front_atlas06_v1.png")),
        "front_atlas07_v1.png" => Some(include_bytes!("../assets/atlas/front_atlas07_v1.png")),
        "front_atlas08_v1.png" => Some(include_bytes!("../assets/atlas/front_atlas08_v1.png")),
        "front_atlas09_v1.png" => Some(include_bytes!("../assets/atlas/front_atlas09_v1.png")),
        "front_atlas10_v1.png" => Some(include_bytes!("../assets/atlas/front_atlas10_v1.png")),
        "front_atlas11_v1.png" => Some(include_bytes!("../assets/atlas/front_atlas11_v1.png")),
        "front_atlas12_v1.png" => Some(include_bytes!("../assets/atlas/front_atlas12_v1.png")),
        "icon_atlas00_v1.png" => Some(include_bytes!("../assets/atlas/icon_atlas00_v1.png")),
        "icon_atlas01_v1.png" => Some(include_bytes!("../assets/atlas/icon_atlas01_v1.png")),
        "icon_atlas02_v1.png" => Some(include_bytes!("../assets/atlas/icon_atlas02_v1.png")),
        "icon_atlas03_v1.png" => Some(include_bytes!("../assets/atlas/icon_atlas03_v1.png")),
        _ => None,
    }
}

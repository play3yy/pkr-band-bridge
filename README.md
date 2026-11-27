# 宝可梦肉鸽同步器（AstroBox NG 插件）

PokeRogue（宝可梦肉鸽）手表快应用配套的 AstroBox V2 手机端同步插件（Rust → WASM）。

## 功能

- **图集同步**：内置精灵图集（前台/背面/图标/形态图集，含 Mega 进化与极巨化立绘），按校验清单经网桥同步到手表缓存；整图 CRC32 校验，损坏自动判坏重拉。
- **图纸进度**：实时列出手表端已有图集与同步结果。
- **本地备份/恢复**：在手机端备份与恢复手表上的游戏进度统计。
- **在线排行榜**：每局结束的成绩汇总为榜单（经典/无尽最高层、最快通关），上报在线全球榜；每台设备每个榜只保留最好一条，成绩更高才替换；支持手动同步手表积压成绩。
- **昵称与队伍展示**：手机端可设置昵称，榜单展示收尾队伍与等级（角色小图随包内嵌）。

## 环境准备

- Rust（含 `wasm32-wasip2` 编译目标）
- Python 3

## 构建

```bash
# Release 构建到 dist 文件夹
python scripts/build_dist.py --release

# 同时打一个 .abp 包，便于本地导入
python scripts/build_dist.py --release --package
```

构建产物输出到 `dist/`：`manifest.json`、`pkr_band_bridge.wasm`、`icon_v2.png`。

## 协议

手表端与插件网桥的帧协议见 [PROTOCOL.md](./PROTOCOL.md)。

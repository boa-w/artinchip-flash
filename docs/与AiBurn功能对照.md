# 与 AiBurn 功能对照

对照依据：官方《AiBurn 使用指南》v2.1.0、`upgcmd --help`（V2.1.0）、
`driver/AiBurnUSB.inf`、主程序字符串。目标：本工具完整包含并替代官方功能。

## 已对齐

| 官方功能 | 本工具 | 说明 |
|---|---|---|
| USB 在线烧录（updater→重连→FULL_DISK→image.info→目标组件→结束→复位） | CLI `burn` / GUI 烧录页 | 同 AiBurn 分阶段流程，CRC 校验，断线重连等待 |
| 按分区烧录（spi/env/os/rodata/data/全选） | 分区勾选表 | `image.target.*` 可选，非目标组件自动处理 |
| 镜像解析与摘要（SoC/板级/版本/介质） | `info <img>` / 镜像页 | 2048B 头 + 512B META，与 `upgcmd -i` 一致 |
| 组件解包 | 镜像页“解包组件” / `upgcmd --extract` 透传 | 按 META 名写文件 |
| 串口烧录（串口号+波特率+连接） | `--uart/--baud/--speed` / GUI 串口模式 | SOH/STX 组帧，`SET_UART_ARGS` 提速，自动进入升级模式 |
| 串口监视收发 | `uart-monitor` / GUI 监视面板 | 应答 `AIBURNFORCE`/`AIBURNID`，一键 `aicupg gotobl` |
| ADB 扫描进升级模式 | `adb_scan` 选项 | 调用兼容目录 `adb shell aicupg` |
| `upgcmd` 25 命令 | 工具页 + `build_args` | 含 `--dev/--uart/--baudrate/--verbose/--log/--progress` 透传 |
| 中英文切换 | `language` 设置 | 默认简体中文 |
| 镜像历史 | `img_history.txt`（50 条） | 选择/清除逻辑同官方 |
| 自动烧录 | `auto_burn` | 设备就绪即开烧 |
| 详细日志/重试次数 | `is_verbose/retry_cnt`（`--verbose`） | 传输层日志门控 |
| 环境检查 | `env-check` / 工具页 | 配置目录、USB、串口、镜像、驱动提示 |
| 驱动安装 | `install-usb-access` / Driver 按钮 | Windows `pnputil` WinUSB、Linux udev（见差异 10） |
| `AiBurn.ini` 兼容 | `load_from` | `image_path/auto_burn/show_statistic/is_verbose/retry_cnt/db_inited` 全读入并回存 |
| 检查更新 | `update` / 设置页 | stable（`v*`）/nightly 双通道（见《更新机制》） |
| 烧录速率/用时显示、停止（中止烧写） | CLI `Ctrl+C` + 速率/用时行 / GUI 停止按钮 + 状态行 | chunk 边界检查取消标志（`BurnOptions.cancel`），CLI 取消退出码 130，`--json` 带 `elapsed_secs`/`rate_bps` |

协议层（`aicupg_cmd_*`）已覆盖烧录/查询/内存/分区/日志/串口参数/JTAG 相关命令字；
文件类操作（`write_file/read_file/delete_file/storage_erase`）复用
`SEND_FWC_DATA` + 升级模式字实现，与官方一致。

## 差异与路线图

按优先级排序。标“未实现”的均不在本版本承诺范围内，如实列出以免误导。

| # | 官方功能 | 现状 | 计划 |
|---|---|---|---|
| 1 | 烧录**速率/用时**显示、**停止**（中止烧写） | 已实现（见上表） | 后续仅做文案/样式微调 |
| 2 | **全片擦除**开关（烧录页） | 仅 `upgcmd flasherase` 透传 | P1：接到烧录前流程（存储擦除命令已具备） |
| 3 | **强制升级**选项（与重启互斥） | 无（默认烧后重启） | P1：加 `force_upgrade` 烧录选项（对应 `UPG_MODE_BURN_IMG_FORCE`），需设备端开强制升级开关（Luban/Luban-Lite 配置见官方手册 §2.1.4）；无硬件验证前明确标注为实验性 |
| 4 | **制作启动卡**（SD 枚举/GPT-MBR/格式化/MMC 镜像写卡，需管理员） | 未实现 | P2：跨平台裸盘写入工作量大（Windows `\\.\PhysicalDriveN` + GPT），先出设计文档；CLI 先做 `sd-list` 只读枚举 |
| 5 | **数据擦写**页（进擦写模式、擦除 Boot、读/擦/写数据、eFuse 读/烧录） | 未实现 | P2：读/擦/写数据可映射到现有内存与分区命令；eFuse 官方无 `upgcmd` 直接命令，需抓包/逆向确认地域寻址协议后实现，不猜协议 |
| 6 | **Agent 服务**（TCP 9100、串口监听推送、技能包 `burn/readlog/getstatus/sendcmd`） | 未实现 | P2：独立服务模块 + JSON 线协议文档；技能包放 `skill/` 目录 |
| 7 | **烧写统计**（按天成功/失败/成功率） | 仅保留 `show_statistic/db_inited` 键 | P2：`burn_stats.json` 按天计数 + 设置页表格（无额外依赖，`serde_json` 已有） |
| 8 | 按次 `.log` 文件落盘 | 仅内存日志窗 | P2：`logs/artinchip-flash-<时间>.log`，CLI 复用同一函数 |
| 9 | 环境检测：冲突服务（VMware USB 等）**停止并禁用** | 仅配置/USB/镜像检查 | P2：Windows 服务枚举（`sc query`）+ 提权停用；Linux/macOS 给等效提示 |
| 10 | WHQL 签名驱动（`.cat`） | 自生成未签名 INF | 如实告知：签名需厂商证书，个人开源项目无法提供；企业用户可自行签名后替换 `driver/` 目录 |
| 11 | 随包 updater 固件（`bin/fw_d1xx.bin`） | 使用镜像内嵌 updater 组件 | 等效：官方内置 updater 只是兜底，镜像内 updater 优先；暂不打包 |

## 兼容性说明

- 官方 `AiBurn.ini` 六键与本工具 `config.ini` 互读；本工具扩展键
  （`transport/serial_*/update_*` 等）官方会忽略，未经证实不写回官方目录。
- 官方检查更新仅限内网；本工具走公网 GitHub Releases，两者不互通。

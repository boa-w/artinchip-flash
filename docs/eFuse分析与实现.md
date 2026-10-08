# eFuse 分析与实现

eFuse 读/烧录在本工具中经官方 `upgcmd` 后端透传实现（工具页 7 个命令）。
原生 UPG bulk 会话（进擦写模式、读/擦/写数据）因线格式未经确认，暂不实现，
需要的抓包证据与方法见 §6。本文件记录全部分析结论与证据来源。

> 约定：标“已验证”的均可在本机复现（`C:\Program Files\AiBurn\upgcmd.exe`
> V2.1.0，`AiBurn 使用指南`手册）；标“公开资料”的给出出处；其余为推断并明确标注。

## 1. 结论

| 需求（对照表 #5） | 状态 | 路径 |
|---|---|---|
| eFuse 读取（选区/偏移/长度→hex 显示或文件） | 已实现 | `bdefuse list/select/read/dump` 透传 |
| eFuse 烧录（选区/偏移/hex 或文件） | 已实现 | `bdefuse write/writehex` 透传 |
| 授权烧录（授权文件） | 已实现（透传；文件格式未知） | `auzwritefuse <授权文件>` 透传 |
| 进擦写模式、擦除 Boot、读/擦/写数据 | 未实现 | 需 USB 抓包确认 `bd_*` 会话线格式（§6） |

eFuse 写操作 OTP 不可逆（0→1，只能写 1，不能擦回 0），烧录前务必先
`bdefuse dump` 核对。`secure`/`rotpk` 等安全区一旦写错可致设备变砖或
JTAG 锁定，授权文件格式未知，切勿猜测构造。

## 2. eFuse Bank 表

`efuse list` 输出（公开资料：官方烧写 eFuse 文档，
`aicdoc.artinchip.com/topics/sdk/secure/burn-eFuse-with-upgcmd-luban.html`）：

| Bank | 范围 | 说明 |
|---|---|---|
| disread | 0x00~0x07 | 读锁定 |
| diswrite | 0x08~0x0F | 写锁定 |
| chipid_main | 0x10~0x1F | 芯片 ID（硬件确认 ChipID @0x10 长 0x10） |
| chipid_sub | 0x20~0x27 | 芯片 ID 扩展 |
| cali | 0x28~0x2F | 校准 |
| brom | 0x30~0x37 | BROM 配置 |
| secure | 0x38~0x3F | 安全标志（硬件确认：JTAG_LOCK bit0，SECURE_BOOT_EN bit16，ENCRYPT_BOOT_EN bit17，SPI_ENC_EN bit19，PBP_ENC_EN bit24） |
| rotpk | 0x40~0x4F | 安全启动根公钥哈希 |
| ssk | 0x50~0x5F | 安全存储密钥 |
| huk | 0x60~0x6F | 硬件唯一密钥 |
| psk0~psk3 | 0x70~0x8F（各 8 字节） | 可编程安全密钥 |
| nvcntr | 0x90~0x9F | 防回滚计数器（BROM 专有，需 BROM_PRIV_LOCK） |
| spienc_key | 0xA0~0xAF | SPI 防克隆 AES 密钥（32 字节，硬件确认 @0xA0） |
| spienc_nonce | 0xB0~0xB7 | SPI Nonce（16 字节，硬件确认 @0xB0） |
| pnk | 0xB8~0xBF | — |
| customer | 0xC0~0xFF | 客户区（64 字节，最常用的可写区） |

逻辑空间 256 字节、按字节寻址。另有 `brom.primary/secondary/skip_sd_phase/
checksum_dis/spi_boot_intf`、`secure.jtag_lock/secure_boot_en/encrypt_boot_en/
anti_rollback_en/spi_enc_en` 等 bit 位（`efuse dump/set <bitsname>` 操作）。

硬件层（公开资料：Gitee `artinchip/luban-lite`，`drv/efuse/drv_efuse.c`、
`hal/efuse/hal_efuse.c`；Linux NVMEM `artinchip-sid.c`，`artinchip,sid-v1.0`）：
SID 控制器字操作（`wid=addr>>2`）、双备份或运算、2.5V 烧写脉冲、
每 32 位独立读写锁。D12x/D13x/D125/D21x/M6800 均有 SID eFuse。

## 3. 命令参考

### 3.1 U-Boot 侧 `efuse` 命令（官方文档）

```
efuse list
efuse dump bank offset size
efuse read bank offset size addr
efuse write bank offset size addr
efuse writehex bank offset data
efuse writestr bank offset data
efuse dump bitsname
efuse set bitsname value
```

注意同一文档内 `write` 示例存在重载写法（裸数据写与地址写参数顺序不同），
以 U-Boot 源码为准，不要只信文档示例。

### 3.2 官方远程路径（官方文档，有实例）

```
upgcmd shcmd "efuse dump psk0 0x0 0x10"
upgcmd shcmd "efuse writestr psk0 ArtInChip"
upgcmd shcmd "efuse write customer 6968436E49747241"
upgcmd shcmd "efuse read customer 0x43000010 0 64" + upgcmd hexdump 0x43000010 64
```

### 3.3 `bdefuse` / `auzwritefuse`（本机二进制证据）

`upgcmd --help`（V2.1.0）主列表**没有** eFuse 命令，但二进制内含完整帮助块，
逐字抄录如下（`C:\Program Files\AiBurn\upgcmd.exe` 字符串表）：

```
bdefuse list                           List eFuse information.
bdefuse select <id>                    Select the eFuse.
bdefuse read <start> <length> <file>   Read eFuse data and write to file.
bdefuse dump <start> <length>          Read eFuse data and dump in hex to console.
bdefuse write <start> <length> <file>  Write eFuse from file.
bdefuse writehex <start> <hexdata>     Read eFuse data and dump in hex.
auzwritefuse <authorization file>  Write data to efuse from authorization file.
```

探针验证（无设备，`upgcmd <cmd>`，2026-10-08）：

| 探针 | 结果 | 结论 |
|---|---|---|
| `frobnicate` | `Command 'frobnicate' not found!`，exit -1 | 未知命令的基准行为 |
| `auzwitefuse`（单 z） | 同上 not found | 该拼写无效 |
| `bdefuse` / `bdefuse badcmd` | `Open upg device failed`，exit 1 | `bdefuse` 是合法顶层命令（先开设备，后校验参数） |
| `auzwritefuse`（双 z） | `Open upg device failed` | 合法顶层命令；帮助行确认单参数 |
| `bdefuseread` | not found | 必须 `bdefuse read` 两词形式 |

注意 `writehex` 的帮助描述疑似从 `dump` 复制（写的是 Read…dump），
以命令名为准，不以描述为准。

### 3.4 设备侧 `bd_*` 动词（二进制证据，线格式未知）

```
bdopen %u / bdclose %u / bdreboot
bdcfg mmc 0x%x 0x%x 0x%x
bdread %u 0x%llx 0x%llx 0x%x / bdwrite %u 0x%llx 0x%llx 0x%x
bderase %u boot / bderase %u 0x%x 0x%x
bd_efuse_read / bd_efuse_write / bd_auz_write_efuse
bd_auz_clear / bd_auz_read / bd_auz_write
bd_mmc_read / bd_mmc_write / bd_mmc_cfg
efuse read 0x%x 0x%x 0x%x / efuse write 0x%x 0x%x 0x%x
```

另有 AUZ Meta（magic/name/data/offset/length/attr/encrypt）、
key/workbuffer 分配、加解密与 `write d21x/d13x/d12x eFuse firmware`
等字符串，表明 bulk 会话含授权与缓冲协商。**这些只是设备侧动词名，
UPG 线包（CMD 字节、包格式、顺序）未知，不得据此手写原生实现。**

## 4. 本工具实现

`OfficialCommand::{BdefuseList, BdefuseSelect, BdefuseRead, BdefuseDump,
BdefuseWrite, BdefuseWriteHex, AuzWriteFuse}` → `build_args` 按 §3.3
逐字组装（`bdefuse` + 子命令 + 参数），透传 `--dev/--uart/--baudrate`。
工具页：起始（地址行）、长度、输入/输出文件、hex（值行）、eFuse 编号行
按命令自动切换；参数缺失在运行前报错，不会发残缺命令。

`OfficialCommand::ALL` 现为 32 个（25 + 7）。

## 5. 安全警告

- eFuse 只能 0→1，不可逆；写前先 `dump`，长度不得超区上限
  （设备会报 `Length over efuse capacity`）。
- `secure`/`rotpk`/`huk`/`ssk`、`secure.jtag_lock` 等影响启动与 JTAG，
  非授权流程不要碰；授权文件格式未知，本工具只做透传，不解析不构造。
- 烧录 eFuse 要求芯片上电并运行 BootLoader（BROM/升级模式）。

## 6. 原生实现所需的抓包证据（待做）

在有设备后，用 USB 抓包确认以下三件事，方可写原生 `bd_*` 会话：

1. **工具**：Windows 用 usbpcap + Wireshark（过滤 `usb.device_address == <addr>`）；
   Linux 用 usbmon。只抓控制传输之外的 Bulk 包。
2. **抓哪几个动作**（AiBurn 数据擦写页 / `upgcmd -v`）：
   `进入擦写模式`、`bdefuse list`、`bdefuse read <start> <len>`、
   `读取数据`（小长度）、`bderase`（Boot）。`upgcmd --verbose` 的 CBW/CSW
   日志可作对照（本工具 `--verbose` 同理）。
3. **看什么**：CBW 载荷中 `UPGC` 魔数后的 CMD 字节（现有命令字见
   `src/protocol/commands.rs`，0x00~0x19 之外的新字节即候选），
   以及 `SET_UPG_CFG` 的模式字节、`SEND_FWC_DATA` 式分块。
4. **不做什么**：不要在未确认 CMD 字节含义前发送 bulk 写/擦命令；
   eFuse 写操作一律先在透传路径验证，再考虑原生化。

## 7. 来源

- 本机：`C:\Program Files\AiBurn\upgcmd.exe` V2.1.0（built Sep 29 2026）、
  `AiBurn.exe`（`QList<efuse_info>`、`MENU_READ/WRITE_EFUSE`、19 个 eFuse
  字段名）、《AiBurn 使用指南》§1.2.4/§2.1.4（强制升级互斥）、§2.3（数据擦写）。
- 公开：`aicdoc.artinchip.com` 安全方案“烧写 eFuse”（`upgcmd shcmd` 实例、
  `efuse list` Bank 表）、工具“数据擦写”系列页（进入流程、六操作参数、
  分 Flash 类型对齐规则）、`100ask` D213 eFuse 指南（NVMEM 只读）。
- 负结果：全网无 `bdefuse`/`bd_efuse_*`/`auzwitefuse` 公开资料（2026-10-08
  检索）；`upgcmd --help` 主列表无 eFuse 命令。

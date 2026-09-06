# rssvc

**rssvc** 是一个用 Rust 编写的轻量级 Windows 服务包装器（Service Wrapper），定位是
NSSM 的现代化替代品：把任意可执行文件、脚本或批处理变成原生 Windows 服务，实现开机
自启、崩溃自动重启、日志轮转和优雅停止，而被包裹的程序完全不需要改造。

单文件 exe，无运行时依赖，release 构建约 **560 KB**，还内置了一个原生 Win32
**图形界面管理器**（双击 exe 即可打开），服务运行时空闲内存占用接近
NSSM（一个进程 + 两个日志线程 + 一个监控等待，无 tokio / 无 GC / 无虚拟机）。

```text
┌────────────┐   SCM 启动    ┌──────────────────┐  CreateProcess  ┌─────────────┐
│ services.msc│ ───────────▶ │   rssvc.exe myapp │ ──────────────▶ │  node.exe   │
│  (Windows) │   服务模式    │  (监控/重启/日志)  │ ◀── stdout/stderr│ (你的程序)   │
└────────────┘              └──────────────────┘   管道重定向      └─────────────┘
```

## 特性

- **一条命令安装**：`rssvc install <名称> <程序> [参数...]`，与 NSSM 用法同源
- **崩溃自动重启 + 智能节流**：应用退出后自动拉起；若陷入"秒退循环"，按指数退避
  （1s → 2s → 4s … 上限 60s），连续失败超过上限自动放弃并上报错误，不会空转烧 CPU
- **NSSM 式四级温和停止**：`CTRL_BREAK` → `WM_CLOSE` → `WM_QUIT` → 强杀整棵进程树，
  每级超时可配置，可用位掩码跳过任意级别
- **Job Object 进程树托管**：无论应用怎么 fork 子进程，都在同一 Job 内，停止/崩溃时
  一网打尽，杜绝孤儿进程
- **stdout/stderr 重定向 + 大小/日期双轮转**：超过阈值或跨天自动归档为
  `out-20260906_153045.log`，可配置保留份数自动清理
- **完整 SCM 集成**：install / remove / start / stop / restart / pause / continue /
  status，服务可被 `services.msc`、`sc.exe` 正常管理
- **暂停/恢复**：`pause` 停掉应用但服务保持"已暂停"状态，`continue` 重新拉起
- **PARAM_CHANGE 热重载**：修改注册表/TOML 后通知服务，日志轮转参数立即生效
- **环境变量、进程优先级、启动类型**（自动/延迟/手动）、依赖服务、指定运行账户
- **TOML 导入导出**：配置备份、跨机迁移、脚本化批量部署（密码不导出）
- **图形界面管理器**：双击 exe 或 `rssvc gui` 打开原生 Win32 管理窗口：服务列表
  / 状态 / PID / 内存 / 运行时长实时刷新、详情面板、一键启停、**编辑配置**、
  **实时日志尾随**、**TOML 导入导出**与图形化安装
- **零依赖可执行**：静态链接 Rust 标准库，只依赖系统 DLL（kernel32/advapi32 等）

## 与 NSSM / Servy 对比

| 维度 | rssvc | NSSM | Servy |
|------|-------|------|-------|
| 语言 / 形态 | Rust / 单 exe ~500KB | C / 单 exe ~250KB | C# / 多组件数 MB |
| 维护状态 | 活跃 | 停滞（2013/2017） | 活跃 |
| 许可证 | MIT | 自定义（禁售） | MIT |
| GUI 配置 | ✔ 列表/监控/编辑配置/实时日志/TOML 导入导出 | ✔ 简易对话框 | ✔ 完整 |
| 崩溃自动重启 | ✔ + 指数退避 | ✔ 固定延迟 | ✔ |
| 进程树管控 | ✔ Job Object | ✔ | ✔ |
| 多级停止 | ✔ 与 NSSM 相同四级 | ✔ | ✔ |
| 日志轮转 | ✔ 大小 + 日期 + 保留清理 | ✔（无自动清理） | ✔ |
| 健康检查/探活 | ✖（规划中） | ✖ | ✔ |
| CLI / 自动化 | ✔ 全命令行 | ✔ | ✔ CLI + PS |
| 代码签名 | ✖（自签场景自行签名） | ✖ | ✔ SignPath |

## 快速开始

> 以下所有命令都需要**管理员权限**的命令提示符或 PowerShell。

### 1. 安装一个 Node.js 服务

```bat
rssvc install my-node "C:\Program Files\nodejs\node.exe" server.js ^
    --dir C:\myapp ^
    --stdout C:\logs\my-node.out.log ^
    --stderr C:\logs\my-node.err.log

rssvc start my-node
```

### 2. 安装一个 Python 脚本（开机延迟自启）

```bat
rssvc install my-py C:\Python312\python.exe -m http.server 8000 ^
    --dir C:\www --startup delayed --rotate-bytes 5242880
rssvc start my-py
```

### 3. 日常管理

```bat
rssvc status my-node      :: 查看状态
rssvc list                :: 列出所有 rssvc 管理的服务
rssvc get my-node         :: 查看完整配置
rssvc restart my-node     :: 重启
rssvc pause my-node       :: 暂停（应用停止，服务显示"已暂停"）
rssvc continue my-node    :: 恢复
rssvc stop my-node        :: 停止
rssvc remove my-node      :: 停止并删除
```

## 命令参考

| 命令 | 说明 |
|------|------|
| `install <名称> <程序> [参数...] [选项]` | 安装新服务。程序路径之后、第一个 `--` 之前的参数都作为应用启动参数 |
| `remove <名称>` | 若在运行先停止（最多等 30s），然后删除服务。别名 `uninstall` |
| `start <名称>` | 启动服务 |
| `stop <名称>` | 发送停止命令并等待（最多 30s） |
| `restart <名称>` | 停止 + 启动 |
| `pause <名称>` / `continue <名称>` | 暂停 / 恢复。`continue` 别名 `resume` |
| `status <名称>` | 查询服务状态 |
| `get <名称>` | 打印服务完整配置 |
| `list` | 扫描本机，列出 ImagePath 指向 rssvc.exe 的服务及运行状态 |
| `export <名称> [文件]` | 导出配置为 TOML（省略文件则打印到 stdout；密码不导出） |
| `import <名称> <文件>` | 从 TOML 导入配置；服务不存在时自动创建 |
| `gui` | 打开图形界面服务管理器（双击 exe 同效） |
| `version` / `help` | 版本 / 帮助 |

### install 选项

| 选项 | 默认值 | 说明 |
|------|--------|------|
| `--dir <目录>` | 程序所在目录 | 工作目录 |
| `--stdout <文件>` | 丢弃 | stdout 日志文件（自动创建目录） |
| `--stderr <文件>` | 丢弃 | stderr 日志文件；与 stdout 同路径时共用一个写入器 |
| `--rotate-bytes <字节>` | 10485760 (10MB) | 大小轮转阈值；`0` = 仅按日期轮转 |
| `--rotate-keep <份数>` | 10 | 保留的归档日志份数；`0` = 全部保留 |
| `--env <KEY=VALUE>` | — | 附加环境变量，可重复；同名变量按不区分大小写覆盖 |
| `--startup <auto\|delayed\|manual>` | auto | 启动类型；delayed = 开机延迟自启 |
| `--priority <级别>` | normal | `realtime` `high` `above-normal` `normal` `below-normal` `idle` |
| `--display <名称>` | 服务名 | 显示名称 |
| `--description <文本>` | — | 服务描述 |
| `--depends <服务1,服务2>` | — | 依赖服务列表 |
| `--user <账户> --password <密码>` | LocalSystem | 以指定账户运行（如 `.\svcuser` 或 `DOMAIN\user`） |
| `--restart-delay <毫秒>` | 1000 | 应用退出后到重启的延迟 |
| `--throttle <毫秒>` | 1500 | 运行时长小于该值视为崩溃循环 |
| `--max-restarts <次数>` | 10 | 连续快速崩溃达到该次数后放弃（`0` = 永不放弃） |
| `--stop-timeout-console <毫秒>` | 1500 | 第 1 级 CTRL_BREAK 后的等待 |
| `--stop-timeout-window <毫秒>` | 1500 | 第 2 级 WM_CLOSE 后的等待 |
| `--stop-timeout-threads <毫秒>` | 1500 | 第 3 级 WM_QUIT 后的等待 |
| `--skip-console` / `--skip-window` / `--skip-threads` / `--skip-terminate` | 不跳过 | 跳过对应停止级别 |
| `--` | — | 之后的参数原样作为应用参数（应用自身有 `--` 参数时使用） |

## 图形界面管理器

**双击 `rssvc.exe`**（或在命令行运行 `rssvc gui`）即可打开原生 Win32 管理窗口。
纯 user32/comctl32 控件实现，不引入第三方 UI 库，exe 仅增加数十 KB：

- **服务列表**：自动扫描本机所有由 rssvc / NSSM 安装的服务（共享
  `Parameters\Application` 注册表布局即可识别），显示服务名、状态、PID、
  内存占用、运行时长、类型（rssvc / nssm）与应用程序路径
- **状态自动刷新**：每 1.5 秒轮询一次；操作按钮在执行期间自动禁用，
  操作在后台线程执行，界面不会卡住
- **服务详情**：选中任一服务，下方面板显示完整配置：启动参数、工作目录、
  环境变量（AppEnvironment 与 AppEnvironmentExtra）、日志路径、轮转与重启
  策略、停止策略（跳过掩码 + 三级超时）、优先级、运行账户、依赖服务；
  由 NSSM 安装的额外参数（如 AppExit、AppNoConsole、AppRotateOnline 等
  未建模键）也会以"其他参数"原样展示，不会遗漏任何配置
- **一键操作**：启动 / 停止 / 重启 / 暂停 / 继续 / 删除服务（删除需确认）
- **编辑配置**：点击"编辑配置"弹出表单，预填当前全部参数：显示名称、描述、
  应用程序、参数、工作目录、日志路径、轮转、启动类型、优先级、重启策略、
  停止超时均可修改；环境变量分两栏独立编辑（AppEnvironment 与
  AppEnvironmentExtra，与 NSSM 键位一一对应）；保存后自动下发 PARAM_CHANGE
  （轮转参数即时生效），并可选立即重启服务使全部配置生效（后台执行，不卡界面）
- **实时日志**：点击"实时日志"打开内置尾随窗口（类似 `tail -f`）：
  stdout / stderr 双流下拉切换、600ms 增量读取、文件轮转自动重新尾随、
  显示缓冲上限 280KB 防 UI 过载、可暂停滚动、"外部打开"调系统默认程序
- **TOML 导入导出**："导出TOML"把选中服务的完整配置（密码除外）存为文件；
  "导入TOML"选择配置文件后输入服务名即可还原为一台新服务（环境变量、
  轮转、重启策略等高级字段完整保留），用于备份与跨机迁移
- **图形化安装**：点击"安装新服务"弹出表单，支持文件/目录浏览对话框，
  可填写服务名、显示名、程序、参数、目录、stdout/stderr、启动类型、优先级
  与环境变量（每行一个 KEY=VALUE，写入 AppEnvironmentExtra），
  与 `rssvc install` 行为一致
- **权限提示**：非管理员运行时窗口顶部会显示警示条，启动/停止/安装/删除
  等写操作可能被 SCM 拒绝

> 建议右键"以管理员身份运行"或在管理员终端执行 `rssvc gui`，全部功能可用；
> 仅查看状态与配置则无需管理员。服务操作结果通过弹窗反馈，失败时会附带
> Win32 错误码与管理员权限提示。

## 运行时行为详解

### 重启与节流

```
应用退出
  ├─ 是我们要求停止的？ ──▶ 服务转入 STOPPED
  ├─ 运行时长 ≥ throttle？ ──▶ 正常重启：等待 restart-delay 后拉起（节流计数清零）
  └─ 运行时长 < throttle（秒退）──▶ 崩溃循环：
        第 1 次延迟 1s → 第 2 次 2s → 4s → … → 上限 60s（指数退避）
        连续超过 max-restarts 次 ──▶ 放弃，服务 STOPPED，退出码 = 应用退出码
```

应用进程一退出，其 Job 内残留的子进程会被立即收割（KILL_ON_JOB_CLOSE），
不会出现僵尸进程占着端口/文件句柄的情况。

### 停止序列（可配置跳过）

| 级别 | 动作 | 等待 | 适用 |
|------|------|------|------|
| 1 | AttachConsole + `CTRL_BREAK`（进程组） | `stop-timeout-console` | 控制台程序（node/python/java…） |
| 2 | 对进程树所有可见窗口发送 `WM_CLOSE` | `stop-timeout-window` | GUI 程序 |
| 3 | 对进程树所有线程发送 `WM_QUIT` | `stop-timeout-threads` | 有消息循环的程序 |
| 4 | `TerminateJobObject` 强杀整棵树 | — | 兜底，前三级都超时后执行 |

接收到系统 **SHUTDOWN**（关机）事件时，各级超时自动压缩到 ≤2s，保证 Windows 在
默认时限内完成关机。

### 日志轮转

- 输出经匿名管道由独立线程收集，应用程序永远只写管道，rssvc 负责落盘
- 满足任一条件触发轮转：当前文件超过 `rotate-bytes`，或本地日期跨天
- 归档命名：`<名称>-<YYYYMMDD_HHMMSS>.log`，同秒冲突自动加 `-N` 后缀
- 轮转后按 `rotate-keep` 删除最旧的归档（`0` 表示永不清理）
- 服务自身生命周期消息以 `[rssvc 2026-09-06 12:00:00] ...` 前缀写入日志，便于排障
- stdout 与 stderr 配置为同一路径时共用一个写入线程，两路输出串行落盘不互相覆盖

### 服务模式识别

与 NSSM 相同的单 exe 自举设计：服务的 ImagePath 为 `"...\rssvc.exe" <服务名>`。
程序启动时先尝试 `StartServiceCtrlDispatcher`，被 SCM 调起则进入服务模式，
否则回落到 CLI 模式——一个文件同时是管理工具和服务载体，不会出现"包装器和工具
不是同一个版本"的问题。

## TOML 备份与迁移

```bat
rssvc export my-node my-node.toml        :: 导出
rssvc import my-node my-node.toml        :: 在新机器上导入（服务不存在会自动创建）
```

`my-node.toml` 示例（`export` 生成的就是这种格式，可直接手改）：

```toml
application = 'C:\Program Files\nodejs\node.exe'
app_directory = 'C:\myapp'
app_parameters = 'server.js'
stdout = 'C:\logs\my-node.out.log'
stderr = 'C:\logs\my-node.err.log'
rotate_bytes = 10485760
rotate_keep = 10
environment = ['NODE_ENV=production', 'PORT=3000']
priority = 32
startup = 2
delayed_autostart = false
stop_method_skip = 0
stop_timeout_console = 1500
stop_timeout_window = 1500
stop_timeout_threads = 1500
restart_delay_ms = 1000
throttle_ms = 1500
max_restarts = 10
display_name = 'my-node'
description = 'Node.js server managed by rssvc'
dependencies = []
```

说明：
- `account`/`password` 不参与导出导入（密码明文不出现在 TOML 中），需要指定运行
  账户的服务请在目标机器上用 `install --user/--password` 重新安装
- 对已存在的服务执行 `import` 只更新配置；改完可 `rssvc restart <名称>` 或向服务
  发送 PARAMCHANGE（`sc control <名称> paramchange`）让日志轮转参数立即生效

## 从 NSSM 迁移

| NSSM | rssvc |
|------|-------|
| `nssm install <name> <path> [args]` | `rssvc install <name> <path> [args]` |
| `nssm set <name> AppDirectory <dir>` | `install --dir <dir>`（或 TOML 导入） |
| `nssm set <name> AppStdout <file>` | `install --stdout <file>` |
| `nssm set <name> AppRotateFiles 1` | 默认启用 |
| `nssm set <name> AppRotateBytes <n>` | `--rotate-bytes <n>` |
| `nssm set <name> AppEnvironmentExtra K=V` | `--env K=V` |
| `nssm set <name> AppExit Default Restart` | 默认即重启 |
| `nssm start/stop/restart <name>` | `rssvc start/stop/restart <name>` |
| `nssm remove <name>` | `rssvc remove <name>` |
| `nssm list` | `rssvc list` |

主要差异：rssvc 的 GUI 是一个原生管理窗口（列表 + 操作 + 全量配置编辑 + 实时日志
+ TOML 导入导出），复杂批量场景仍推荐 CLI/TOML；停止级别与 NSSM 的
`AppStopMethod*` 兼容（控制台→窗口→线程→终止，位掩码含义相同）。

### 与 NSSM 的已知语义差异

- **空 `AppEnvironment` 的含义不同**：NSSM 中"该值存在但为空"表示用**空环境**启动
  子进程；rssvc 中该值为空等价于"值不存在"，即**完整继承系统环境**再叠加
  `AppEnvironmentExtra`。依赖 NSSM 空环境隔离语义的服务迁移后行为会变化。
- **服务名不允许空白字符**：rssvc 在安装时即拒绝包含空格/制表符/引号等服务名
  （NSSM 允许），避免 ImagePath 参数解析歧义。
- **退出码上报采用 `SERVICE_SPECIFIC_ERROR` 规范**：rssvc 自定义码（2001/2002/2003）
  与应用自身退出码都会出现在 `dwServiceSpecificExitCode` 字段，`sc query` 可直接读出。

## 构建

### Windows 本地构建

```bat
cargo build --release
:: 产物: target\release\rssvc.exe
```

### Linux / macOS 交叉编译（本文档附带的 exe 即此方式产出）

```bash
rustup target add x86_64-pc-windows-msvc
cargo install cargo-xwin
cargo xwin build --release --target x86_64-pc-windows-msvc
# 产物: target/x86_64-pc-windows-msvc/release/rssvc.exe
```

`release` profile 已内置体积优化（`opt-level="s"`、LTO、单 codegen unit、
panic=abort、strip），产物约 560 KB。

### 测试与 CI

- 纯逻辑单元测试覆盖：命令行参数引号规则、服务名校验、verbatim 路径剥离、
  MULTI_SZ 解码、TOML 序列化（密码不导出）、环境块合并、日志归档名匹配、
  SCM 掩码常量一致性等（`cargo test`）
- GitHub Actions（`.github/workflows/`）：每次 push/PR 执行
  `cargo fmt --check`、`cargo clippy -- -D warnings`、`cargo test`、release 构建；
  推送 `v*` 标签时自动构建并附加 exe 与 zip 到 GitHub Release

## FAQ

**Q: 安装时报 0x00000005 拒绝访问？**
A: install/remove/start/stop 都在写 HKLM 注册表和 SCM，必须以管理员身份运行。

**Q: 能否把 rssvc.exe 移动到别的目录？**
A: 服务的 ImagePath 指向安装时 rssvc.exe 的绝对路径（与 NSSM 相同）。移动 exe 后
需要删除服务重新 install，或直接改注册表中该服务的 ImagePath。

**Q: 应用是控制台程序但第 1 级停止没生效？**
A: `CTRL_BREAK` 要求 rssvc 能 AttachConsole 到目标进程的控制台。个别程序会立即
`FreeConsole` 或自己开新控制台，此路径会自动跳过并记录日志，由后续级别兜底。

**Q: 中文在日志里是乱码？**
A: rssvc 把应用输出按原始字节写入日志（与 NSSM 行为一致），日志编码取决于应用
本身（GBK/UTF-8）。用对应编码查看即可。CLI 输出建议在 Windows Terminal 或
`chcp 65001` 环境下使用。

**Q: 想在应用退出码为 0 时不重启？**
A: 目前"退出即重启"（与 NSSM 默认一致）。如需此行为，可用包装脚本控制退出码，
或等待后续版本的 AppExit 策略。

**Q: 服务日志在哪里看 rssvc 自身的消息？**
A: 所有生命周期消息带 `[rssvc 时间戳]` 前缀写入 stdout/stderr 日志文件；若未配置
日志文件，这些消息不可见。

## 已知限制 / Roadmap

- [ ] 健康检查（心跳文件 / TCP 探活）
- [ ] 生命周期钩子（启动前/停止后脚本）
- [ ] AppExit 退出码策略（按退出码决定是否重启）
- [ ] Windows 事件日志集成（注册消息表资源）
- [ ] ARM64 交叉编译产物

## 更新记录

### v0.3.2（2026-09）— 代码审查修复版

对照外部全量代码审查报告（28 项：P0×3 / P1×5 / P2×12 / P3×8）逐项修复：

| 编号 | 级别 | 问题 | 状态 |
|------|------|------|------|
| BUG-01 | P0 | `SERVICE_NAME` 全局量从未写入，所有服务启动即以 2001 退出 | ✔ 已修复（`try_dispatch` 写入 + ServiceMain argv 兜底解析） |
| BUG-02 | P0 | GUI 删除确认 MB_OKCANCEL 却比较 IDYES，删除永不执行 | ✔ 已修复（统一 `ask_yes_no`，MB_YESNO） |
| BUG-03 | P0 | 编辑保存后"立即重启"询问同样错配 | ✔ 已修复（同上） |
| BUG-04 | P1 | SCM 掩码缺 `SC_MANAGER_CONNECT`，status/list 打开服务失败 | ✔ 已修复（掩码改用 windows crate 标准常量） |
| BUG-05 | P1 | ImagePath 服务名未加引号，含空格服务名无法启动 | ✔ 已修复（名称加引号 + 安装期拒绝空白字符） |
| BUG-06 | P1 | CLI install 重拼参数丢失引号 | ✔ 已修复（按 Windows argv 规则逐参数重加引号） |
| BUG-07 | P1 | `canonicalize` 引入 `\\?\` verbatim 路径写入注册表 | ✔ 已修复（`canonicalize_plain`，CLI/GUI/编辑/导入四处统一） |
| BUG-08 | P1 | 退出码未用 SPECIFIC_ERROR 语义；读取失败兜底为 0 | ✔ 已修复（规范上报 + 失败记录告警） |
| BUG-09 | P2 | 停止事件被手动关闭后全局量仍持有悬空句柄 | ✔ 已修复（移除手动 CloseHandle，句柄随进程回收） |
| BUG-10 | P2 | 归档清理按前缀匹配可能误删无关文件 | ✔ 已修复（严格时间戳模式匹配 + 单元测试） |
| BUG-11 | P2 | 轮转 rename 失败静默，日志可能无限增长 | ✔ 已修复（重试 3 次 + 失败写告警 + 保持真实 size） |
| BUG-12 | P2 | 线程创建失败 panic 叠加 abort 使服务消失 | ✔ 已修复（降级运行：丢弃日志流/记录告警） |
| BUG-13 | P2 | `wait_any` 遇 WAIT_FAILED 每 100ms 空转 | ✔ 已修复（返回错误走受控失败路径，新增退出码 2003） |
| BUG-14 | P2 | install 写注册表失败不回滚遗留坏服务 | ✔ 已修复（CLI/GUI/import 三处失败自动 DeleteService） |
| BUG-15 | P2 | `--password` 明文暴露在命令行 | ✔ 已修复（支持不回显交互输入；明文形式保留但给出警告） |
| BUG-16 | P2 | import 更新服务时 dependencies 被静默忽略 | ✔ 已修复（非空即更新；空列表输出"保留现有依赖"提示） |
| BUG-17 | P2 | 空 AppEnvironment 语义与 NSSM 相反 | ✔ 已文档化（README"与 NSSM 的已知语义差异"） |
| BUG-18 | P2 | INTERROGATE 回包控制掩码与实际状态不符 | ✔ 已修复（按状态返回控制集） |
| BUG-19 | P2 | 主窗口销毁未释放状态盒子与定时器 | ✔ 已修复（WM_DESTROY 补 KillTimer + Box 回收；投递失败自回收） |
| BUG-20 | P2 | 停止流程提前返回不 join 读取线程 | ✔ 已修复（提前返回分支同样回收） |
| BUG-21 | P3 | `env::vars()` 非法 UTF-8 panic | ✔ 已修复（改用 `vars_os` + 损失性转换） |
| BUG-22 | P3 | 附加子进程控制台后不恢复原状态 | ✔ 已修复（检测原有控制台并尽力恢复） |
| BUG-23 | P3 | 进程树快照 PID 复用竞态 | ◔ 记录为已知限制（触发窗口极小，修复成本高） |
| BUG-24 | P3 | 已知命令名与服务名冲突 | ✔ 已修复（单参数一律先尝试 SCM 分发，名为 gui/start 的服务可正常启动） |
| BUG-25 | P3 | 配置打印逻辑双份维护 | ◔ 暂保留（CLI 与 GUI 展示格式需求不同，属重构范畴） |
| BUG-26 | P3 | 日志窗口 WM_SIZE 无宽度下限 | ✔ 已修复（宽高双重下限保护） |
| BUG-27 | P3 | 无测试、无 CI、无静态检查门禁 | ✔ 已补齐（单元测试 + GitHub Actions fmt/clippy/test + 标签发布流水线） |
| BUG-28 | P3 | 手写权限常量 / GUI 刷新开销 / 编辑项不全 | ✔ 常量改用 windows crate 标准常量；✔ 编辑对话框补齐依赖服务与停止级别编辑；◔ 列表刷新逐服务开句柄暂保留 |

### v0.3.1（2026-09）— GUI 实测问题修复

- 下拉框补 `CBS_DROPDOWNLIST`（修复 CBS_SIMPLE 常驻展开遮挡控件导致的界面错乱）
- 对话框统一按客户区尺寸反推外框（修复底部按钮被标题栏/边框裁剪）
- 详情面板与配置读取补全 NSSM 兼容键（AppEnvironmentExtra、停止策略、依赖等）
- 安装对话框新增环境变量输入；编辑对话框双环境变量框

### v0.3.0（2026-09）— GUI 短板补全

- 编辑配置对话框、实时日志尾随窗口、TOML 导入导出按钮

## License

MIT

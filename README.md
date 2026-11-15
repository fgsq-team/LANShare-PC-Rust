# LANShare-RS

[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-edition%202024-orange.svg)](Cargo.toml)
[![Platform](https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-green.svg)]()
[![Protocol](https://img.shields.io/badge/LANShare%20protocol-V1~V5-informational.svg)](src/lanshare/proto.rs)

LANShare 桌面端的 Rust 重写：把 Qt Widgets 换成 [windui](https://crates.io/crates/windui) 自绘 UI，网络与协议层逐字节对齐原版 C++（Qt）与 Java（Android）实现。**同一个局域网里的安卓版、Qt 桌面版、网页端可以直接和本程序互通**，无需连互联网。

---

## 功能特性

### 设备与传输

- **自动设备发现** — UDP 广播心跳，IPv4 与 IPv6 各一条 socket（Windows / macOS），无需手填 IP；退出前向所有在线设备广播下线
- **文件 / 文件夹传输** — TCP 传输进度实时刷新、可中途取消；目录按遍历清单逐项推进
- **协议 V1~V5 全兼容** — 出站走 V5（`fileTransferJson`），入站按首 4 字节魔数分流：`-2` = V4、`-3` = V5、其余走 V1~V3 旧版分帧
- **断点续传** — V5 支持，续传状态存在接收目录下，启动时清理过期项
- **拖拽发送** — 文件或文件夹拖进窗口即可；「全部设备」下先弹「选择设备发送」，可选单台或群发
- **重发与取消** — 失败的文件气泡可重发，进行中的传输可取消

### 消息与界面

- **文本消息** — `Enter` 换行、`Ctrl+Enter` 发送；可选「发到对方剪贴板」，对方系统剪贴板直接被写入
- **聊天记录持久化** — SQLite（`rusqlite` bundled，不依赖系统库）；按设备分页 + 「全部设备」合并流独立分页，顶部探针触发上翻
- **已读管理** — 单会话标已读、一键全部已读、删除单条气泡、清空当前会话（不动历史库）
- **内置图片预览器** — 缩放 / 适应窗口 / 原始大小 / 左右旋转 / 全屏 / 上下张切换 / 复制到剪贴板 / 另存为 / 系统程序打开；缩略图在解码线程池后台生成，不挡 UI
- **表情面板** — 点一下把字符追加到草稿末尾，同时把焦点还给输入框
- **三套配色 + 跟随系统** — 浅色 / 深色 / 祖母绿，色值抄自 Qt 版 `style_light.qss`、`style_dark.qss`、`style_emerald.qss`；侧栏一枚按钮循环切换，跟随系统时响应深浅色变化
- **字体可调** — 字体族与字号在设置里改，整棵树跟着重排
- **窗口自适应** — 初始尺寸与侧栏宽度按屏幕工作区算，高 DPI 下不溢出屏底
- **可选中复制** — 气泡正文、本机地址、网页地址均可拖选；另有「自由复制文本」对话框与「复制全文」按钮
- **系统托盘** — 关窗收进托盘后台常驻（这项改动要重启程序才生效）
- **单实例** — 重复启动不建新窗口，而是把已有窗口唤到前台

### 网页端

- **同一端口共存** — TCP 服务读到首 4 字节是 `GET `/`POST` 就交给 HTTP 栈，网页与局域网传输共用 5856，不必再开一个端口
- **前端内嵌 exe** — `assets/web/` 在构建期打进二进制，运行时不落地文件
- **能力齐全** — 浏览目录、图片预览、单个文件下载、多文件 / 媒体打包 zip 下载、上传文件、给指定设备发消息、WebSocket 推送设备表与传输进度
- **令牌授权** — 新客户端要用户点「同意」才放行（对应原版授权弹框），也可在设置里改为免授权
- **扫码连接** — 「网页二维码」对话框画出地址二维码，下拉可在本机网卡 IPv4 与查回的公网 IPv6 之间切换

### 其它

- **检查更新** — 启动时静默查一次、「关于」里可手动再查；通道名 `release_windows_rust`，与原版同一个版本服务器。更新说明服务端存的是整页 HTML，这里折成纯文本显示；强更档位下「不更新」会变成「退出」
- **设备信息展示** — 设备类型（Android / Windows / Linux / macOS / iOS）、电量与充电状态图标、未读计数
- **运行日志** — 设环境变量 `LANSHARE_TRACE` 才写盘（默认零开销），落在数据目录 `lanshare_trace.log`；panic 的 message 与 backtrace 也进同一个文件

## 技术栈

| 组件 | 技术 |
|------|------|
| 语言 | Rust（edition 2024） |
| GUI 框架 | [windui](https://crates.io/crates/windui) 0.33（自绘、跨平台，自带托盘 / 文件对话框 / 剪贴板 / 快捷键） |
| 加密 | `aes` + `cipher` + `md-5`：AES-256 / ECB / PKCS7，逐字节对齐原版 `QAESEncryption` |
| 数据库 | `rusqlite`（`bundled`，自带 SQLite） |
| 图像 | `image`（纯 Rust 解码：PNG / JPEG / GIF / WebP / BMP / TIFF） |
| 二维码 | `qrcode`（只开 SVG 特性，界面按矢量画） |
| 平台 API | `windows` 0.62（Win32：网卡枚举、时区、电量、CF_DIB 剪贴板、命名互斥体） |
| HTTP / WebSocket | 自研，路由与端点对齐原版 `httpserver` |
| 构建 | `cargo` + `build.rs`（注入数值版本号、Windows 嵌图标） |

## 项目结构

```
LANShare-RS/
├── src/
│   ├── lanshare/           # 引擎：不认识界面
│   │   ├── mod.rs          #   Engine 中心对象、事件队列、收发调度
│   │   ├── proto.rs        #   包头 / 包体编解码与混淆
│   │   ├── crypto.rs       #   AES-256-ECB/PKCS7、密钥派生、UUID
│   │   ├── config.rs       #   config.ini 读写与默认值
│   │   ├── db.rs           #   聊天记录（SQLite）
│   │   ├── walk.rs         #   目录遍历与传输清单
│   │   ├── manager/        #   设备表 / UDP / TCP / 旧版协议 / 网卡与电量
│   │   └── version/        #   V4、V5 收发与断点续传
│   ├── web/                # 网页服务：HTTP/WS 框架、路由、multipart、zip、二维码、会话与令牌
│   ├── app/                # 应用层：状态、消息分发、后台事件泵、托盘、更新、设置
│   ├── views/              # 界面：侧栏、消息流、输入区、对话框、图片预览
│   ├── utils/              # 剪贴板、文件选择、公网 IPv6、单实例、运行日志、更新检测
│   ├── assets.rs           # 图标 / SVG / 缩略图队列
│   ├── theme.rs            # 配色档案、尺寸 token、窗口自适应
│   └── model.rs            # 设备 / 消息 / 传输的数据模型
├── assets/                 # 图标、Material 图标集、内嵌的网页前端、exe 资源
├── LANShare/               # Android 原版（对照参考，不参与构建）
├── LANShare-PC/            # Qt C++ 原版（对照参考，不参与构建）
├── build.rs                # 读 Cargo.toml 注入版本号 + Windows 嵌图标
└── Cargo.toml
```

## 构建与运行

### 环境要求

- **Rust** 稳定版工具链，需支持 `edition = "2024"`（1.85 以上）
- **Windows**：本项目在 `x86_64-pc-windows-msvc` 上开发与验证（需要 VS 的链接器与 Windows SDK）
- 不依赖系统数据库与图形库：SQLite 是 `bundled` 编译进来，图像解码、HTTP / WebSocket、zip 都是纯 Rust 实现，UI 自绘

### 构建

```bash
# 调试运行
cargo run

# 发布构建，产物 target/release/LANShare-RS.exe
cargo build --release
```

`build.rs` 在 Windows 上会把 `assets/win/LANShare.ico` 嵌进 exe；这一步失败只报 `cargo:warning`，不影响构建结果。

### 测试

```bash
cargo test --lib
```

协议编解码、AES、ini 往返、分页、HTML 折叠、更新报文、路由表等都在单元测试里（当前 251 项）。引擎与 UI 分成库 + 可执行两个目标，就是为了让这些测试不用拉起窗口。

### 版本号（发版要动的两处）

包版本走 Cargo 的 semver（`[package] version`），**数值版本号另存在自定义区**：

```toml
# 具体值以 Cargo.toml 为准，这里只取当前那一份
[package.metadata.lanshare]
version-code = 2601115   # 前四位 YYMM + 末三位流水
```

`build.rs` 把它读出来注入 `LANSHARE_VERSION_CODE`，代码那头是 `utils::update::VERSION_CODE`。这个数不能从 semver 换算（换算出来只剩 2000000，跟服务端那一行一比就变成「每次启动都提示有新版」）。发 Rust 版时还要请服务端把 `release_windows_rust` 那一行的 `versionCode` 填得比它大，否则用户永远等不到提示。

## 使用说明

1. 两端连同一个局域网（防火墙首次运行请允许专用网络）
2. 启动后自动开始 UDP 广播发现，左侧列出在线设备；点设备即切到该会话，「全部设备」是合并流
3. 发消息：输入框写完按 `Ctrl+Enter`；发文件：点工具栏按钮选文件 / 文件夹，或直接拖进窗口
4. 收文件：默认不询问直接落盘（`accept_recv_files=true`），想先问一句就在设置里关掉「收到文件时不再询问，直接接收」；落盘目录在设置里改
5. 网页端：侧栏勾「开启网页功能」，点二维码图标扫码或复制地址，浏览器直接打开

### 快捷键

| 位置 | 按键 | 作用 |
|------|------|------|
| 主窗口 | `Ctrl+Enter` | 发送草稿 |
| 主窗口 | `Enter` | 换行（不发送） |
| 主窗口 | `Ctrl+L` | 光标回到设备搜索框 |
| 主窗口 | `Esc` | 收起当前遮罩面板（接收与网页授权确认不算答复，那条请求下次同步会重新摆回） |
| 图片预览 | `+` / `-` | 放大 / 缩小（滚轮同效） |
| 图片预览 | `0` / `1` | 适应窗口 / 原始大小 |
| 图片预览 | `←` `→` 或 `A` `D` | 上一张 / 下一张 |
| 图片预览 | `R` / `Shift+R` | 顺时针 / 逆时针旋转 |
| 图片预览 | `F` | 全屏切换 |
| 图片预览 | `C` / `S` / `O` | 复制到剪贴板 / 另存为 / 系统程序打开 |
| 图片预览 | `Esc` | 关闭（全屏时先退出全屏） |

### 默认端口与数据目录

| 用途 | 值 |
|------|------|
| UDP（设备发现） | 4573 |
| TCP（文件传输 / 消息） | 5856 |
| HTTP / WebSocket（网页端） | 与 TCP 同一个 5856，按首 4 字节分流 |
| 接收文件默认目录 | `C:/LANShare/`（非 Windows 为 `~/LANShare/`） |
| 数据目录 | Windows `%LOCALAPPDATA%\LANShareRust`、macOS `~/Library/Application Support/LANShareRust`、其余 `$XDG_DATA_HOME/LANShareRust` |
| 配置 / 历史库 | 数据目录下 `config.ini` / `message_v2.db` |

数据目录刻意与 Qt 版分开：两边共用同一个 `message_v2.db` 会互相写坏。

### 配置项（`config.ini`）

| 键 | 默认值 | 说明 |
|------|------|------|
| `filePath` | `C:/LANShare/` | 接收文件保存目录 |
| `userName` | 当前用户名 | 本机在局域网里的显示名 |
| `uniqueUUid` | 首次启动生成 | 设备唯一码，变了会被当成新设备 |
| `tcpPort` / `udpPort` | `5856` / `4573` | 传输与发现端口 |
| `messageKey` | 内置默认密钥的密文 | 消息文本密钥，落盘前用固定密钥 AES 加密 |
| `enc_data` | `false` | 文件数据块是否逐字节混淆 |
| `accept_recv_files` | `true` | 收到文件免确认直接接收 |
| `save_message` | `true` | 聊天记录写入 SQLite |
| `theme` | 空 = 跟随系统 | 可填 `light` / `dark` / `emerald` |
| `font_family` / `font_size` | `幼圆` / `9` | 界面字体 |
| `web_service` | `true` | 是否在 TCP 端口上接受 HTTP / WebSocket 访问 |
| `open_web_service` | `false` | 网页客户端免授权 |
| `use_internal_image_preview` | `true` | 点图片走内置预览器，关掉退回系统默认程序 |
| `hide_on_close` | `true` | 关窗收进托盘后台常驻，**改动要重启才生效** |
| `force_version` | `-1` | 用户在更新提示上点过「不更新」的那个数值版本 |
| `allow_same_host_test` | `false` | 放行自己发的包，方便同机开两个实例互传 |

## 与 Qt 原版的差异

- **远程媒体控制未接** — 静音 / 恢复音量的命令字与配置项都留着，收到即忽略（`manager/udp.rs`）
- **更新说明折成纯文本** — 服务端 `updateContent` 是整页 HTML，windui 没有 HTML 引擎，先折文本再排版
- **弹框走程序自己的遮罩面板** — 不再起系统模态框，观感与主界面统一
- **非 Windows 下「图片复制到剪贴板」不可用** — 那条通路是 Win32 的 `CF_DIB`，其他平台返回 `Unsupported`
- **只有 zip 发布物** — 更新通道里的 `fileUrl` 指向 zip，本仓库不含 Inno Setup / `macdeployqt` 那类打包脚本

## 故障排查

- **看不到设备** — 检查两端是否同一子网、防火墙是否放行、`udpPort` 是否被改得两边不一致
- **收不到文件或网页打不开** — 两者共用 TCP 端口，`netstat -ano | findstr 5856` 看有没有监听；被别的程序占了就在设置里换端口
- **界面卡住 / 崩溃** — 设 `LANSHARE_TRACE=1` 重新启动，把数据目录下的 `lanshare_trace.log` 附上来。release 是无控制台构建，出问题的现场只在这个文件里

## 相关项目

- **LANShare**（Android）— [fgsq-team/LANShare](https://github.com/fgsq-team/LANShare)
- **LANShare-PC**（Qt C++ 桌面端）— [fgsq-team/LANShare-PC](https://github.com/fgsq-team/LANShare-PC)
- **官方网站** — [lanshares.com](http://lanshares.com)

## 问题反馈

- QQ 群：538809905
- [GitHub Issues](https://github.com/fgsq-team/LANShare-PC-Rust/issues)

## 赞助支持

如果 LANShare 对你有帮助，欢迎请作者喝杯咖啡 ☕

| 微信 | 支付宝 |
|:---:|:---:|
| ![微信打赏](assets/img/wx.png) | ![支付宝打赏](assets/img/alipay.png) |

## 许可证

本项目基于 [Apache License 2.0](LICENSE) 开源，与 LANShare 各端一致。

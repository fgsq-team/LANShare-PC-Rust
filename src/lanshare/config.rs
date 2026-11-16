//! 配置读写。数据目录与 Qt 版完全隔离（Windows 下是 `%LOCALAPPDATA%\LANShareRust`），
//! 但 ini 的键名沿用原版（`filePath`、`userName`、`messageKey`……），便于对照与迁移。

use std::path::{Path, PathBuf};

use super::crypto::{DEFAULT_MESSAGE_KEY, KEY, derive_message_key, ecb_encrypt, hex_encode};

/// 默认保存路径（原版 `DEFAULT_FILE_PATH`），只在 Windows 上直接用它。
pub const DEFAULT_FILE_PATH: &str = "C:/LANShare/";
/// 默认 TCP 端口。
pub const DEFAULT_TCP_PORT: i32 = 5856;
/// 默认 UDP 端口。
pub const DEFAULT_UDP_PORT: i32 = 4573;
/// 默认字体（原版 `DEFAULT_FONT_FAMILY`）。
pub const DEFAULT_FONT_FAMILY: &str = "幼圆";
/// 默认字号。
pub const DEFAULT_FONT_SIZE: i32 = 9;

/// 收到的文件默认存到哪里。
///
/// Windows 与原版一致写 `C:/LANShare/`；类 Unix 上这个串会被当成当前目录里一个
/// 名叫 `C:` 的子目录，既难找也可能不可写，所以改存家目录下的 `LANShare`。
pub fn default_file_path() -> String {
    #[cfg(windows)]
    {
        DEFAULT_FILE_PATH.to_string()
    }
    #[cfg(not(windows))]
    {
        let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from);
        match home {
            Some(home) => format!("{}/", home.join("LANShare").to_string_lossy().replace('\\', "/")),
            None => DEFAULT_FILE_PATH.to_string(),
        }
    }
}

/// 运行期配置。`message_key` 存的是**明文真实密钥**，写回 ini 时会自动加密。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// 收到的文件保存目录。
    pub save_path: String,
    /// 本机在局域网里的显示名。
    pub client_name: String,
    /// 设备唯一码，首次启动生成后固定下来，否则每次启动都会被视为新设备。
    pub unique_uuid: String,
    pub tcp_port: i32,
    pub udp_port: i32,
    /// 消息文本密钥（明文）。
    pub message_key: String,
    /// 文件数据块是否逐字节混淆。
    pub enc_data: bool,
    /// 收到文件是否无需确认直接接收。
    pub accept_recv_files: bool,
    /// 是否响应静音/恢复音量广播。
    pub receive_mute: bool,
    /// 是否保存聊天记录到 SQLite。
    pub save_message: bool,
    /// 主题：空串 = 跟随系统，其余为 light / dark / emerald。
    pub theme: String,
    pub font_family: String,
    pub font_size: i32,
    /// 允许与本机重复测试：关掉「收到自己发的包就跳过」的过滤，方便同机双实例互发。
    pub allow_same_host_test: bool,
    /// 是否在 TCP 端口上接受 HTTP/WebSocket 访问（原版 `web_service`，默认开）。
    /// 关掉之后这个端口只认 LANShare 自己的魔数。
    pub web_service: bool,
    /// 是否免授权直接放行网页客户端（原版 `open_web_service`，默认关）。
    /// 开着时新 token 不弹授权框直接写进库；关着时要用户点「同意」才生效。
    pub open_web_service: bool,
    /// 消息列表点击图片是否走内置预览器（原版 `use_internal_image_preview`，默认开）。
    /// 关掉就退回系统默认图片程序，等于原版的 `QDesktopServices::openUrl`。
    pub use_internal_image_preview: bool,
    /// 点 × 关闭主窗口时收进系统托盘、进程后台常驻（默认开）；关掉则点 × 直接退出、
    /// 托盘里也不挂图标。**这一项改动要重启程序才生效**：它对应 windui 两个构建期声明：
    /// `App::tray` 装不装图标，`App::hide_on_close` 把获准的关闭转成窗口隐藏，运行期两样
    /// 都改不动（`TrayOp` 没有增删图标的口子，`NIM_DELETE` 只随平台宿主 drop；那个开关没有
    /// setter），取的就是启动那一刻的快照，保存时界面上会提示要重启。最小化不参与：原版 Qt
    /// 没有「最小化收进托盘」这回事。
    pub hide_on_close: bool,
    /// 用户在更新提示上点过「不更新」的那个数值版本号（原版 ini 键名 `force_version`，
    /// 默认 -1 = 一条也没拒过）。名字是原版留下的，存的其实是「已拒过的最高版本」：
    /// 自动检测遇到不大于它的版本就不再打扰，手动检测不看这一项。
    pub force_version: i64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            save_path: default_file_path(),
            client_name: default_client_name(),
            unique_uuid: String::new(),
            tcp_port: DEFAULT_TCP_PORT,
            udp_port: DEFAULT_UDP_PORT,
            // 与 Qt 一样：ini 里放的是密文，运行时的 `message_key` 是解出来的明文
            message_key: derive_message_key(DEFAULT_MESSAGE_KEY),
            enc_data: false,
            accept_recv_files: true,
            receive_mute: false,
            save_message: true,
            theme: String::new(),
            font_family: DEFAULT_FONT_FAMILY.to_string(),
            font_size: DEFAULT_FONT_SIZE,
            allow_same_host_test: false,
            web_service: true,
            open_web_service: false,
            use_internal_image_preview: true,
            hide_on_close: true,
            force_version: -1,
        }
    }
}

fn default_client_name() -> String {
    // USERNAME 是 Windows 的叫法，类 Unix 得问 USER
    for key in ["USERNAME", "USER"] {
        if let Some(name) = std::env::var(key).ok().filter(|name| !name.is_empty()) {
            return name;
        }
    }
    // 原版是 QDir::home().dirName()：拿家目录的最后一段
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from);
    match home {
        Some(path) => path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "LANShare".to_string()),
        None => "LANShare".to_string(),
    }
}

impl Config {
    /// 独立数据目录：Windows `%LOCALAPPDATA%\LANShareRust`，macOS `~/Library/Application Support/LANShareRust`，
    /// 其余 Unix `$XDG_DATA_HOME/LANShareRust`（默认 `~/.local/share`）；都拿不到才退回 `~/.LANShareRust`。
    /// 关键是不能与 Qt 版共用目录，否则两边的 `message_v2.db` 会互相损坏。
    pub fn dir() -> PathBuf {
        #[cfg(windows)]
        {
            if let Some(local) = std::env::var_os("LOCALAPPDATA") {
                return PathBuf::from(local).join("LANShareRust");
            }
        }
        #[cfg(target_os = "macos")]
        {
            if let Some(home) = std::env::var_os("HOME") {
                return PathBuf::from(home).join("Library").join("Application Support").join("LANShareRust");
            }
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            let base = std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local").join("share")));
            if let Some(base) = base {
                return base.join("LANShareRust");
            }
        }
        let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
        match home {
            Some(home) => PathBuf::from(home).join(".LANShareRust"),
            None => PathBuf::from("LANShareRust"),
        }
    }

    pub fn config_path() -> PathBuf {
        Self::dir().join("config.ini")
    }

    pub fn db_path() -> PathBuf {
        Self::dir().join("message_v2.db")
    }

    /// 读配置；文件不存在时返回默认值并立刻落盘（好让 `uniqueUUid` 稳定下来）。
    pub fn load() -> Self {
        let path = Self::config_path();
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let mut config = Self::parse(&text);
        if config.unique_uuid.is_empty() {
            config.unique_uuid = super::crypto::new_id();
        }
        if text.is_empty() {
            let _ = config.save();
        }
        config
    }

    /// 写配置（先建目录）。单元测试里一律跳过：测试用的是临时端口与临时目录，
    /// 真写进 `%LOCALAPPDATA%\LANShareRust` 会把用户下次启动的配置覆盖掉。
    pub fn save(&self) -> std::io::Result<()> {
        if cfg!(test) {
            return Ok(());
        }
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, self.render())
    }

    /// ini 文本 -> 配置。未知键忽略，坏值回落到默认。
    pub fn parse(text: &str) -> Self {
        let defaults = Self::default();
        let mut config = defaults.clone();
        let table = read_ini(text);
        let get = |key: &str| table.get(key).cloned();

        if let Some(value) = get("filePath") {
            if !value.is_empty() {
                config.save_path = value;
            }
        }
        if let Some(value) = get("userName") {
            if !value.is_empty() {
                config.client_name = value;
            }
        }
        if let Some(value) = get("uniqueUUid") {
            config.unique_uuid = value;
        }
        config.tcp_port = get("tcpPort").and_then(|value| value.parse().ok()).unwrap_or(defaults.tcp_port);
        config.udp_port = get("udpPort").and_then(|value| value.parse().ok()).unwrap_or(defaults.udp_port);
        config.enc_data = read_bool(get("enc_data"), false);
        config.accept_recv_files = read_bool(get("accept_recv_files"), true);
        config.receive_mute = read_bool(get("receive_mute"), false);
        config.save_message = read_bool(get("save_message"), true);
        config.allow_same_host_test = read_bool(get("allow_same_host_test"), false);
        config.web_service = read_bool(get("web_service"), true);
        config.open_web_service = read_bool(get("open_web_service"), false);
        config.use_internal_image_preview = read_bool(get("use_internal_image_preview"), true);
        config.hide_on_close = read_bool(get("hide_on_close"), true);
        config.force_version = get("force_version").and_then(|value| value.parse().ok()).unwrap_or(defaults.force_version);
        config.theme = get("theme").unwrap_or_default();
        config.font_family = get("font_family").filter(|value| !value.is_empty()).unwrap_or(defaults.font_family);
        config.font_size = get("font_size").and_then(|value| value.parse().ok()).unwrap_or(defaults.font_size);

        // 密文形态的配置值 -> 明文密钥；解不出来（被手改坏）时用默认常量
        let stored = get("messageKey").filter(|value| !value.is_empty()).unwrap_or(DEFAULT_MESSAGE_KEY.to_string());
        let derived = derive_message_key(&stored);
        config.message_key = if derived.is_empty() { derive_message_key(DEFAULT_MESSAGE_KEY) } else { derived };
        config
    }

    /// 配置 -> ini 文本。`messageKey` 按原版方式用固定 `KEY` AES 后 hex 存储。
    pub fn render(&self) -> String {
        let mut out = String::from("[General]\n");
        out.push_str(&format!("filePath={}\n", self.save_path));
        out.push_str(&format!("userName={}\n", self.client_name));
        out.push_str(&format!("uniqueUUid={}\n", self.unique_uuid));
        out.push_str(&format!("tcpPort={}\n", self.tcp_port));
        out.push_str(&format!("udpPort={}\n", self.udp_port));
        out.push_str(&format!("enc_data={}\n", bool_text(self.enc_data)));
        out.push_str(&format!("accept_recv_files={}\n", bool_text(self.accept_recv_files)));
        out.push_str(&format!("receive_mute={}\n", bool_text(self.receive_mute)));
        out.push_str(&format!("save_message={}\n", bool_text(self.save_message)));
        out.push_str(&format!("allow_same_host_test={}\n", bool_text(self.allow_same_host_test)));
        out.push_str(&format!("web_service={}\n", bool_text(self.web_service)));
        out.push_str(&format!("open_web_service={}\n", bool_text(self.open_web_service)));
        out.push_str(&format!("use_internal_image_preview={}\n", bool_text(self.use_internal_image_preview)));
        out.push_str(&format!("hide_on_close={}\n", bool_text(self.hide_on_close)));
        out.push_str(&format!("force_version={}\n", self.force_version));
        out.push_str(&format!("theme={}\n", self.theme));
        out.push_str(&format!("font_family={}\n", self.font_family));
        out.push_str(&format!("font_size={}\n", self.font_size));
        out.push_str(&format!("messageKey={}\n", self.stored_message_key()));
        out
    }

    /// ini 里该写的形态：明文密钥用固定 `KEY` AES-256-ECB/PKCS7 后 hex。
    pub fn stored_message_key(&self) -> String {
        hex_encode(&ecb_encrypt(self.message_key.as_bytes(), KEY.as_bytes()))
    }

    /// 保存目录（确保存在后的绝对路径）。
    pub fn ensure_save_path(&self) -> PathBuf {
        let path = PathBuf::from(&self.save_path);
        let _ = std::fs::create_dir_all(&path);
        path
    }

    /// 接收文件时的落盘根目录。
    pub fn save_root(&self) -> PathBuf {
        self.ensure_save_path()
    }

    /// 目标路径是否指向我们自己的数据目录，避免把文件写进续传目录里。
    pub fn is_reserved_root(&self, path: &Path) -> bool {
        path == Self::dir()
    }
}

fn bool_text(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

fn read_bool(value: Option<String>, default: bool) -> bool {
    match value {
        Some(text) => match text.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => true,
            "false" | "0" | "no" => false,
            _ => default,
        },
        None => default,
    }
}

/// 极简 ini 读取：只要 `key = value`，忽略分组与注释。
fn read_ini(text: &str) -> std::collections::BTreeMap<String, String> {
    let mut table = std::collections::BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') || line.starts_with('[') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        table.insert(key.trim().to_string(), unquote(value.trim()));
    }
    table
}

/// Qt 的 ini 写法会给含特殊字符的值加引号，读的时候剥掉。
fn unquote(value: &str) -> String {
    let trimmed = value.trim_matches('"');
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_dir_is_isolated_from_the_qt_app() {
        let dir = Config::dir();
        let text = dir.to_string_lossy().replace('\\', "/");
        assert!(text.contains("LANShareRust"), "目录应当独立: {text}");
        assert!(!text.contains("/.LANShare/"), "不应与 Qt 版同目录: {text}");
    }

    #[test]
    fn defaults_match_the_original_constants() {
        let config = Config::default();
        assert_eq!(config.save_path, default_file_path());
        assert_eq!(config.tcp_port, 5856);
        assert_eq!(config.udp_port, 4573);
        assert!(config.accept_recv_files);
        assert!(!config.enc_data);
        // 原版对内置预览器也是「默认开」：老用户的 ini 没这个键时不能突然变成外部程序
        assert!(config.use_internal_image_preview);
        assert_eq!(config.message_key, derive_message_key(DEFAULT_MESSAGE_KEY));
    }

    #[test]
    fn missing_file_yields_defaults_and_broken_values_fall_back() {
        let config = Config::parse("");
        assert_eq!(config, Config::default());
        let config = Config::parse("[General]\ntcpPort=abc\nenc_data=maybe\nmessageKey=不是十六进制\n");
        assert_eq!(config.tcp_port, DEFAULT_TCP_PORT);
        assert!(!config.enc_data);
        assert!(!config.message_key.is_empty());
    }

    #[test]
    fn render_and_parse_round_trip_preserves_every_field() {
        let mut config = Config::default();
        config.save_path = "D:/收文件/".to_string();
        config.client_name = "楼下的 PC".to_string();
        config.unique_uuid = "a1b2c3".to_string();
        config.tcp_port = 6000;
        config.udp_port = 6001;
        config.enc_data = true;
        config.accept_recv_files = false;
        config.theme = "dark".to_string();
        config.font_family = "微软雅黑".to_string();
        config.font_size = 12;
        config.allow_same_host_test = true;
        config.open_web_service = true;
        config.use_internal_image_preview = false;
        config.force_version = 2612001;
        let text = config.render();
        assert!(text.starts_with("[General]\n"), "{text}");
        let back = Config::parse(&text);
        assert_eq!(back, config);
        // 落盘的密钥一定是密文形态，且不是明文本身
        let stored = Config::parse(&text).stored_message_key();
        assert_ne!(stored, config.message_key);
        assert!(stored.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn quoted_values_written_by_qt_are_unwrapped() {
        let config = Config::parse("userName=\"某某的电脑\"\nfilePath=\"D:/a b/\"\n");
        assert_eq!(config.client_name, "某某的电脑");
        assert_eq!(config.save_path, "D:/a b/");
    }

    #[test]
    fn the_web_switches_keep_the_qt_defaults_and_ini_names() {
        // 默认值与原版相反的一个、相同的一个：网页服务开、免授权关
        let config = Config::default();
        assert!(config.web_service);
        assert!(!config.open_web_service);
        // 键名是下划线形态，不是驼峰——写错就等于读不到老用户的开关
        assert!(!Config::parse("web_service=false\nopen_web_service=true\n").web_service);
        // 预览开关同理：键名是下划线形态，默认开；写 false 要能读回 false
        assert!(Config::parse("").use_internal_image_preview);
        assert!(!Config::parse("use_internal_image_preview=false\n").use_internal_image_preview);
        assert!(Config::parse("web_service=false\nopen_web_service=true\n").open_web_service);
        assert!(Config::parse("").web_service, "缺键时按原版默认开启");
        assert!(Config::default().render().contains("web_service=true\n"));
    }

    #[test]
    fn boolean_forms_are_tolerated() {
        assert!(Config::parse("accept_recv_files=1\n").accept_recv_files);
        assert!(!Config::parse("accept_recv_files=0\n").accept_recv_files);
        assert!(Config::parse("enc_data=TRUE\n").enc_data);
    }

    /// 「已拒过的那一版」沿用原版的键名 `force_version`，默认 -1：那个默认值本身就是
    /// 「一个真实版本号都不会小于它」的哨兵，写成 0 会把服务端码为 0 的脏行当成已拒过。
    #[test]
    fn the_declined_update_version_keeps_the_original_ini_name() {
        assert_eq!(Config::default().force_version, -1);
        assert_eq!(Config::parse("").force_version, -1);
        assert_eq!(Config::parse("force_version=2612001\n").force_version, 2612001);
        // 负数也是合法取值（哨兵），不能被当成坏值丢掉
        assert_eq!(Config::parse("force_version=-1\n").force_version, -1);
        assert_eq!(Config::parse("force_version=abc\n").force_version, -1);
        assert!(Config::default().render().contains("force_version=-1\n"));
    }
}

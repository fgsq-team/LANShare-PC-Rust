//! 构建脚本：两件事。
//!
//! 1. 把数值版本号从 `Cargo.toml` 的 `[package.metadata.lanshare]` 搬进编译期，注成
//!    `LANSHARE_VERSION_CODE`（代码那头见 `utils::update::VERSION_CODE`）。那个数放不进
//!    `version` 字段（Cargo 只收三段 semver），而 `[package.metadata]` 又是 Cargo 明确不解析的
//!    自定义区，`env!` 那类内置变量读不到它，只能这里中转。
//! 2. 仅在 Windows 下把 LANShare 图标作为 Win32 资源嵌进 exe，让资源管理器里的 .exe、
//!    以及固定到任务栏/开始菜单的快捷方式显示正确的应用图标。
//!
//! 运行期的窗口标题栏 / 任务栏活动图标 / Alt-Tab 是另一条路径：由 `assets::window_icon()`
//! 在 `app.rs` 里通过 `App::icon(..)` 设置，不依赖这里。

use std::path::Path;

/// 数值版本号在清单里的那张表。表名与键名都是契约：写错任何一处，下面的扫描得**报错**，
/// 不能退回某个默认值——带错的版本号跑起来只会表现为「永远没更新」或「每次启动都提示有新版」。
const VERSION_TABLE: &str = "[package.metadata.lanshare]";
const VERSION_KEY: &str = "version-code";

fn main() {
    // `Cargo.toml` 必须挂在监视列表里：脚本只要输出过 `rerun-if-changed`，Cargo 就不再默认
    // 盯着整个包目录，那时改了数值版本号却不重跑本脚本，编出来的 exe 里还是旧值——
    // 比写错常量更难查，因为构建全程一声不吭。
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/win/LANShare.ico");

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("构建脚本总能拿到清单目录");
    let manifest = Path::new(&manifest_dir).join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest).unwrap_or_else(|error| panic!("读不了清单 {manifest:?}：{error}"));
    let code = version_code(&text).unwrap_or_else(|| panic!("{manifest:?} 里没读到可用的版本号，要长这样：\n{VERSION_TABLE}\n{VERSION_KEY} = 2611001（纯数字）"));
    println!("cargo:rustc-env=LANSHARE_VERSION_CODE={code}");

    #[cfg(windows)]
    {
        let icon = Path::new(&manifest_dir).join("assets/win/LANShare.ico");
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon(icon.to_str().expect("Windows 路径是有效 UTF-8"));
        // 嵌失败也只是没有 exe 图标，不该让整个构建中断——运行期图标仍照常生效。
        if let Err(error) = resource.compile() {
            println!("cargo:warning=嵌入 exe 图标失败：{error}");
        }
    }
}

/// 从清单里挑出数值版本号。不拉 `toml` 依赖：只要「目标表里那一个键」——认识得越宽，
/// 写错时越有可能撞上别的表里的同名键，拿到一个错的数。三种 `None` 都要让构建失败：
/// 表里没这个键、键找到了但等号后面的值不是纯数字、值是空的。
fn version_code(manifest: &str) -> Option<String> {
    let mut section = "";
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            section = line;
            continue;
        }
        if section != VERSION_TABLE {
            continue;
        }
        // 进了目标表就只等这一个键：在下一个 `[…]` 行之前还不中就是没有，不要接着往下猜
        // 键找到了却没紧跟 `=`：形状不对，直接判失败，不往下碰运气
        let Some(rest) = line.strip_prefix(VERSION_KEY) else { continue };
        let rest = rest.trim_start().strip_prefix('=')?;
        // 值可以写成 `2611001`、`"2611001"`，后面可以跟 `#` 注释
        let value = rest.split('#').next().unwrap_or_default().trim().trim_matches('"');
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        return Some(value.to_owned());
    }
    None
}

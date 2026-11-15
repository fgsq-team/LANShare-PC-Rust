//! 聊天列表模式与主题偏好：两个与界面强相关的小枚举及其下标 / 标签换算。

use crate::theme;

/// 聊天列表模式：合并（不区分设备、面向全部在线设备）或按设备分会话。
///
/// 合并对标 C++ 版默认的「发所有人」：主消息区把所有会话按时间拼成一条流，发消息与
/// 选中的文件都群发。两种模式共用同一份**按设备存储**（历史、分页、未读都不动），
/// 合并只是视图 + 发送目标的差别，所以来回切换不丢数据。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ChatMode {
    /// 全部设备：合并消息流 + 群发。
    #[default]
    Merged,
    /// 分设备：看选中的那台，发给那台。
    PerDevice,
}

impl ChatMode {
    /// 分段控件的下标（0 = 全部设备）。
    pub fn index(self) -> usize {
        match self {
            Self::Merged => 0,
            Self::PerDevice => 1,
        }
    }

    pub fn from_index(index: usize) -> Self {
        match index {
            1 => Self::PerDevice,
            _ => Self::Merged,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Merged => "全部设备",
            Self::PerDevice => "分设备",
        }
    }
}

/// 窗口主题偏好。一档对应 [`theme::Skin`] 里的一份配色档案，`System` 则按系统深浅色二选一。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ThemePref {
    #[default]
    System,
    Light,
    Dark,
    Emerald,
}

impl ThemePref {
    /// 工具栏快速切换的循环顺序：四档绕一圈回到「跟随系统」。
    pub(super) fn next(self) -> Self {
        match self {
            Self::System => Self::Light,
            Self::Light => Self::Dark,
            Self::Dark => Self::Emerald,
            Self::Emerald => Self::System,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::System => "跟随系统",
            Self::Light => "浅色",
            Self::Dark => "深色",
            Self::Emerald => "祖母绿",
        }
    }

    /// 分段控件的下标。
    pub fn index(self) -> usize {
        match self {
            Self::System => 0,
            Self::Light => 1,
            Self::Dark => 2,
            Self::Emerald => 3,
        }
    }

    pub fn from_index(index: usize) -> Self {
        match index {
            1 => Self::Light,
            2 => Self::Dark,
            3 => Self::Emerald,
            _ => Self::System,
        }
    }

    /// 配置里的 `theme` 字段：空串 = 跟随系统，与原版取值一致。
    pub fn from_theme(theme: &str) -> Self {
        match theme {
            "light" => Self::Light,
            "dark" => Self::Dark,
            "emerald" => Self::Emerald,
            _ => Self::System,
        }
    }

    pub fn as_theme(self) -> &'static str {
        match self {
            Self::System => "",
            Self::Light => "light",
            Self::Dark => "dark",
            Self::Emerald => "emerald",
        }
    }

    /// 解析成配色档案。祖母绿不参与「跟随系统」——它是一套显式选择的配色，
    /// 不是深浅方向，系统深色时也不能拿它当「绿色的深色」用。
    pub(super) fn skin(self, system_dark: bool) -> theme::Skin {
        match self {
            Self::Light => theme::Skin::LIGHT,
            Self::Dark => theme::Skin::DARK,
            Self::Emerald => theme::Skin::EMERALD,
            Self::System if system_dark => theme::Skin::DARK,
            Self::System => theme::Skin::LIGHT,
        }
    }
}

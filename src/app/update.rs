//! 更新检测的状态侧：把后台查回的版本信息与本地两个数（自身的数值码、已经拒过的那一版）
//! 对齐成一条提示，以及点「不更新」时把那一版记进配置。
//!
//! 判据本体在 [`crate::utils::update::decide`]——那个纯函数好测，也保证规则只有一处。这里只管
//! 「结果回来后动哪份状态、要不要落盘、该不该给用户说一句话」。

use crate::utils::update::{self, Prompt, Report};

use crate::app::ChatApp;

/// 一次检测回来该不该给用户说一句话。分成三档而不是 `Option<String>`，是因为
/// 「已是最新」与「检测失败」在界面上不是一回事：前者是好消息，后者得用报错色。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Verdict {
    /// 一声不吭。自动检测的默认结局：没新版、新版是拒过的那一版、查到一半网络不通，
    /// 都不该拦住正在传文件的用户。
    Silent,
    /// 「当前已是最新版本」。只有手动检测才说。
    Latest,
    /// 「检测更新失败：…」那句里的原因。只有手动检测才说。
    Failed(String),
}

impl ChatApp {
    /// 挂在屏幕上的更新提示；`None` = 不弹。
    pub fn update_prompt(&self) -> Option<&Prompt> {
        self.update.as_ref()
    }

    /// 后台查回的版本信息落进状态。返回的 [`Verdict`] 只在**手动**检测时才是非 `Silent`：
    /// 自动检测既不能拦住人，网不通也不该报错（原版的 `manual` 分支口径一模一样）。
    pub(super) fn report_update(&mut self, report: Result<Report, String>, manual: bool) -> Verdict {
        let report = match report {
            Ok(report) => report,
            Err(error) => return if manual { Verdict::Failed(format!("检测更新失败：{error}")) } else { Verdict::Silent },
        };
        // 「已拒过哪一版」从引擎里的现行配置读，不读 `self.settings`：那份是设置对话框
        // 正被编辑的草稿，可能还没落盘。
        let skipped = self.engine.config().force_version;
        match update::decide(&report, update::VERSION_CODE, skipped, manual) {
            Some(prompt) => {
                self.update = Some(prompt);
                self.touch();
                // 新版本身有提示面板要说，这里不再叠一句轻提示
                Verdict::Silent
            }
            // 不提就是「没新的」。注意拒过的版本走的是自动那条路，手动检测会无视拒绝记录
            // 重新提，所以这里不会把「有新版但你拒过」说成「已是最新」。
            None => {
                if manual { Verdict::Latest } else { Verdict::Silent }
            }
        }
    }

    /// 「打开下载链接」之后与按 ESC：只把提示收掉。原版这两条路都**不记**「已拒过」——
    /// 用户没说不更新，下次该照样提。
    pub(super) fn close_update(&mut self) {
        if self.update.take().is_some() {
            self.touch();
        }
    }

    /// 「不更新」：记下这一版的号，之后的自动检测不再提它（对应原版写 `FORCE_VERSION`）。
    /// 强更那一档没有这个出口：界面上给的是「退出」，压根不会发这条消息过来。
    pub(super) fn decline_update(&mut self) {
        let Some(prompt) = self.update.take() else { return };
        self.touch();
        if prompt.force {
            return;
        }
        let mut config = self.engine.config();
        if config.force_version == prompt.code {
            // 记过了就不要再写一遍盘
            return;
        }
        config.force_version = prompt.code;
        self.engine.set_config(config.clone());
        // 设置对话框正编辑的那份草稿也要跟着改：表单里没有这一项，用户下一秒点「保存设置」
        // 会把 `read_form` 抄出来的旧值整个写回去，这条记录就默默丢了。
        self.settings = config;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lanshare::Engine;
    use crate::lanshare::config::Config;
    use std::sync::Arc;

    /// 不碰真机历史库、不起网络服务的状态机。数值码用当前这个常量做基准，
    /// 用例里的「新/旧」都在它上下各挪一点。
    fn app() -> ChatApp {
        ChatApp::from_engine(&Arc::new(Engine::for_test(Config::default())))
    }

    /// 比当前大的码：该挂出提示，且不该顺手给用户一句话（那句由提示自己说）。
    fn newer(code: i64) -> Report {
        Report {
            version_code: code,
            version: "2.1".to_owned(),
            file_url: "http://a/b.zip".to_owned(),
            content: "修了些问题".to_owned(),
            force: false,
            force_min: 0,
            deleted: false,
        }
    }

    /// 手动检测有新码 -> 挂提示、不说话；无新码 -> 说一句「已是最新」。
    #[test]
    fn a_manual_check_either_shows_the_prompt_or_says_latest() {
        let mut app = app();
        assert_eq!(app.report_update(Ok(newer(update::VERSION_CODE + 1)), true), Verdict::Silent);
        let prompt = app.update_prompt().expect("手动检测到新版要挂出提示");
        assert_eq!(prompt.version, "2.1");
        assert_eq!(prompt.url, "http://a/b.zip", "绝对地址原样透出去");
        assert_eq!(prompt.body, "修了些问题");
        app.update = None;
        // 服务端码不比我们大：手动检测给一句实话
        assert_eq!(app.report_update(Ok(newer(update::VERSION_CODE)), true), Verdict::Latest);
        assert!(app.update_prompt().is_none());
    }

    /// 自动检测：没新版一声不吭，失败也不出声——只有手动才说话，而且失败得用报错色。
    #[test]
    fn an_auto_check_stays_quiet_when_there_is_nothing_to_show() {
        let mut app = app();
        assert_eq!(app.report_update(Ok(newer(update::VERSION_CODE)), false), Verdict::Silent);
        assert!(app.update_prompt().is_none());
        assert_eq!(app.report_update(Err("连不上 lanshares.com".to_owned()), false), Verdict::Silent);
        assert_eq!(app.report_update(Err("连不上 lanshares.com".to_owned()), true), Verdict::Failed("检测更新失败：连不上 lanshares.com".to_owned()));
    }

    /// 用户拒过这一版之后，自动检测闭嘴、手动检测照样提。这条只在状态机上验一遍链路，
    /// 判据本身在 `utils::update::decide` 里测。
    #[test]
    fn a_declined_version_is_skipped_by_the_next_auto_check() {
        let mut app = app();
        let code = update::VERSION_CODE + 5;
        assert_eq!(app.report_update(Ok(newer(code)), false), Verdict::Silent);
        app.decline_update();
        assert_eq!(app.engine.config().force_version, code, "「不更新」要把那一版记进配置");
        assert!(app.update_prompt().is_none(), "记下之后提示要收掉");
        assert_eq!(app.report_update(Ok(newer(code)), false), Verdict::Silent, "自动检测不能再提这一版");
        assert!(app.update_prompt().is_none());
        // 手动检测要无视那一条记录：不能拿「已拒过」当成已是最新
        assert_eq!(app.report_update(Ok(newer(code)), true), Verdict::Silent);
        assert!(app.update_prompt().is_some(), "手动检测把同一版重新挂了上来");
    }

    /// 强更档点「退出」不会走到 `decline_update`；真走到了也不能写记录——那条路本来就没退路。
    #[test]
    fn forcing_update_records_nothing() {
        let mut app = app();
        let mut report = newer(update::VERSION_CODE + 1);
        report.force = true;
        report.force_min = update::VERSION_CODE + 100;
        app.report_update(Ok(report), false);
        let prompt = app.update_prompt().expect("低于强更线的版本要挂出提示");
        assert!(prompt.force, "提示得带着强更标记，界面据此换按钮");
        app.decline_update();
        assert_eq!(app.engine.config().force_version, -1, "强更不要写「已拒过」");
    }

    /// 「打开下载链接」/ESC 只收提示，不写记录：用户没说不更新。
    #[test]
    fn closing_the_prompt_records_nothing() {
        let mut app = app();
        app.report_update(Ok(newer(update::VERSION_CODE + 2)), true);
        assert!(app.update_prompt().is_some());
        app.close_update();
        assert!(app.update_prompt().is_none());
        assert_eq!(app.engine.config().force_version, -1);
        // 没有提示时再收一次也不能改状态
        app.close_update();
    }

    /// 记下版本后，设置对话框那份草稿得同步：表单里没这一项，草稿带着旧值一保存就会把记录抹掉。
    #[test]
    fn the_declined_version_survives_a_settings_save() {
        let mut app = app();
        let code = update::VERSION_CODE + 9;
        app.report_update(Ok(newer(code)), false);
        app.decline_update();
        assert_eq!(app.settings().force_version, code, "引擎配置与对话框草稿必须是同一个值");
        let draft = app.settings().clone();
        // 模拟一次「保存设置」：草稿原样写回引擎
        app.engine.set_config(draft);
        assert_eq!(app.engine.config().force_version, code, "保存设置不能把这条记录冲掉");
    }
}

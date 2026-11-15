//! 网页客户端的授权表，对应原版 `src/TokenDBUtil.cpp`。
//!
//! 库文件与 Qt 版隔离：放在我们自己的数据目录下的 `token_list_new.db`，
//! 表结构与原版逐字一致（`token(id, ip, custom, isdel)`），这样以后要从 Qt 版
//! 直接搬库过来也能读。删除是软删（`isdel = 1`），查询只看 `isdel = 0`。

use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use rusqlite::{Connection, params};

use crate::lanshare::config::Config;

/// 一行授权记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    /// 客户端持有的令牌。
    pub id: String,
    pub ip: String,
    /// 用户在授权框里勾了「以后这个 IP 免确认」，原版叫 `custom`。
    pub custom: bool,
}

const CREATE_TABLE: &str = "CREATE TABLE IF NOT EXISTS token (id VARCHAR(32) PRIMARY KEY, ip VARCHAR(16), custom INTEGER, isdel INTEGER)";

/// 授权表。`Connection` 不是 `Sync`，整库访问都上互斥锁。
pub struct TokenDb {
    connection: Mutex<Connection>,
}

impl TokenDb {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let connection = Connection::open(path)?;
        connection.execute_batch(CREATE_TABLE)?;
        Ok(Self { connection: Mutex::new(connection) })
    }

    /// 新增或覆盖一条授权。
    pub fn add(&self, id: &str, custom: bool, ip: &str) -> rusqlite::Result<()> {
        let connection = self.connection.lock().expect("令牌库锁未被污染");
        connection.execute(
            "INSERT OR REPLACE INTO token (id, ip, custom, isdel) VALUES (?1, ?2, ?3, 0)",
            params![id, ip, i32::from(custom)],
        )?;
        Ok(())
    }

    /// 令牌是否还有效；有效时返回当初授权的那台 IP。
    pub fn query(&self, id: &str) -> Option<Grant> {
        if id.is_empty() {
            return None;
        }
        let connection = self.connection.lock().expect("令牌库锁未被污染");
        let mut statement = connection.prepare("SELECT id, ip, custom FROM token WHERE id = ?1 AND isdel = 0").ok()?;
        statement
            .query_map(params![id], |row| Ok(Grant { id: row.get(0)?, ip: row.get(1)?, custom: row.get::<_, i32>(2)? != 0 }))
            .ok()?
            .next()
            .and_then(|row| row.ok())
    }

    /// 这台 IP 之前被勾过「免确认」吗？命中就直接放行，不再弹授权框。
    pub fn query_custom_ip(&self, ip: &str) -> Option<Grant> {
        if ip.is_empty() {
            return None;
        }
        let connection = self.connection.lock().expect("令牌库锁未被污染");
        let mut statement = connection.prepare("SELECT id, ip, custom FROM token WHERE ip = ?1 AND custom = 1 AND isdel = 0").ok()?;
        statement
            .query_map(params![ip], |row| Ok(Grant { id: row.get(0)?, ip: row.get(1)?, custom: true }))
            .ok()?
            .next()
            .and_then(|row| row.ok())
    }

    /// 当前有效的全部授权，设置面板要能撤销。
    pub fn list(&self) -> Vec<Grant> {
        let Ok(connection) = self.connection.lock() else { return Vec::new() };
        let Ok(mut statement) = connection.prepare("SELECT id, ip, custom FROM token WHERE isdel = 0 ORDER BY ip") else {
            return Vec::new();
        };
        let rows = match statement.query_map([], |row| Ok(Grant { id: row.get(0)?, ip: row.get(1)?, custom: row.get::<_, i32>(2)? != 0 })) {
            Ok(rows) => rows,
            Err(_) => return Vec::new(),
        };
        rows.filter_map(|row| row.ok()).collect()
    }

    /// 软删一条授权：令牌立刻失效，网页端下一次请求就会被赶回首页重新授权。
    pub fn revoke(&self, id: &str) -> rusqlite::Result<()> {
        let connection = self.connection.lock().expect("令牌库锁未被污染");
        connection.execute("UPDATE token SET isdel = 1 WHERE id = ?1", params![id])?;
        Ok(())
    }
}

/// 进程级共享的授权表。打不开库（磁盘只读等）就返回 `None`，网页端一律按未授权处理，
/// 而不是让整个功能崩掉。
static SHARED: OnceLock<Option<Arc<TokenDb>>> = OnceLock::new();

/// 取共享授权表；路径固定为 `Config::dir()/token_list_new.db`。
pub fn shared() -> Option<Arc<TokenDb>> {
    SHARED
        .get_or_init(|| TokenDb::open(&Config::dir().join("token_list_new.db")).ok().map(Arc::new))
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一个用例一张库文件：`TokenDb` 背后是真 SQLite，共用一个文件时并行用例
    /// 会互相看见对方插进去的授权行，`list()` 的断言就没法写了。
    fn table(tag: &str) -> TokenDb {
        let path = std::env::temp_dir().join(format!("lanshare-token-{}-{tag}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        TokenDb::open(&path).expect("临时库里应当能建表")
    }

    #[test]
    fn a_fresh_token_is_valid_and_remembers_its_ip() {
        let db = table("fresh");
        assert!(db.query("nope").is_none());
        db.add("t1", false, "192.168.1.20").unwrap();
        let grant = db.query("t1").expect("刚发的令牌就该有效");
        assert_eq!(grant.ip, "192.168.1.20");
        assert!(!grant.custom);
    }

    /// 「免确认」是按 IP 记的，同一台机器刷新页面不该再弹一次框。
    #[test]
    fn custom_grants_are_matched_by_ip() {
        let db = table("custom");
        db.add("t1", true, "10.0.0.5").unwrap();
        db.add("t2", false, "10.0.0.6").unwrap();
        assert_eq!(db.query_custom_ip("10.0.0.5").map(|grant| grant.id), Some("t1".to_string()));
        assert!(db.query_custom_ip("10.0.0.6").is_none(), "普通授权不参与免确认匹配");
        assert!(db.query_custom_ip("").is_none());
    }

    #[test]
    fn revoking_hides_the_token_from_every_query() {
        let db = table("revoke");
        db.add("t1", true, "10.0.0.7").unwrap();
        db.revoke("t1").unwrap();
        assert!(db.query("t1").is_none());
        assert!(db.query_custom_ip("10.0.0.7").is_none());
        assert!(db.list().is_empty());
    }

    /// 同一张令牌重复授权要覆盖而不是主键冲突。
    #[test]
    fn adding_the_same_id_twice_overwrites() {
        let db = table("dup");
        db.add("t1", false, "1.1.1.1").unwrap();
        db.add("t1", true, "2.2.2.2").unwrap();
        let grant = db.query("t1").unwrap();
        assert_eq!(grant.ip, "2.2.2.2");
        assert!(grant.custom);
    }
}

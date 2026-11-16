//! 聊天记录库。表名与 20 个列名与原版 `MessageDB` 完全一致，只是文件放在我们自己的数据目录里。
//!
//! 存储约定（与原版对齐）：
//! - `messageType` 1 文本 / 2 文件 / 3 图片；
//! - 文本正文用固定 `KEY` AES 后 hex 存储，文件名明文存储；
//! - 删除是软删（`isdel`），查询只看 `isdel = 0`。

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, params};

use super::crypto::{KEY, dec_message, enc_message};
use super::device::now_ms;

/// 文本消息。
pub const TYPE_TEXT: i64 = 1;
/// 文件/文件夹消息。
pub const TYPE_FILE: i64 = 2;
/// 图片消息（与文件同构，只是展示成缩略图）。
pub const TYPE_IMAGE: i64 = 3;

/// 一行消息记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub kind: i64,
    /// 文本消息是明文正文，文件消息是展示名。落库时按原版规则决定是否加密。
    pub message: String,
    /// 设备名，仅供展示；不再参与设备归属判断。
    pub device_name: String,
    /// 设备唯一 id（`uuid`）：历史按它归属与查询，设备改名、换 IP 也不丢历史、不串号。
    pub device_id: String,
    pub data_version: i64,
    pub to_user: String,
    /// true = 对方发来的（左侧气泡）。
    pub is_left: bool,
    pub is_file: bool,
    pub dev_mode: i64,
    pub file_path: String,
    pub completed: bool,
    pub received: bool,
    pub is_video: bool,
    pub video_time: String,
    pub file_size: i64,
    pub size_str: String,
    pub create_time: String,
    pub time_stamp: i64,
    pub progress: i64,
}

impl Default for Entry {
    fn default() -> Self {
        Self {
            id: String::new(),
            kind: TYPE_TEXT,
            message: String::new(),
            device_name: String::new(),
            device_id: String::new(),
            data_version: 0,
            to_user: String::new(),
            is_left: false,
            is_file: false,
            dev_mode: 0,
            file_path: String::new(),
            completed: false,
            received: false,
            is_video: false,
            video_time: String::new(),
            file_size: 0,
            size_str: String::new(),
            create_time: String::new(),
            time_stamp: 0,
            progress: 0,
        }
    }
}

/// 历史库。`Connection` 不是 `Sync`，用互斥锁包住整库访问。
pub struct HistoryDb {
    connection: Mutex<Connection>,
}

const CREATE_TABLE: &str = "create table if not exists message(\
    id VARCHAR(32) primary key,\
    messageType INTEGER,\
    message text,\
    deviceName text,\
    dataVersion INTEGER,\
    toUser text,\
    isLeft INTEGER,\
    isfile INTEGER,\
    devMode INTEGER,\
    filepath text,\
    completed INTEGER,\
    recviced INTEGER,\
    isVideo INTEGER,\
    videoTime VARCHAR(19),\
    fileSize INTEGER,\
    fileSizeStr text,\
    createTime varchar,\
    timeStamp INTEGER,\
    isdel INTEGER,\
    progress INTEGER,\
    deviceId text)";

impl HistoryDb {
    /// 打开（或创建）数据库并确保表结构；早期库缺 `progress` 列时补一列。
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let connection = Connection::open(path)?;
        connection.execute_batch(CREATE_TABLE)?;
        let has_progress: i64 = connection
            .query_row(
                "select count(*) from pragma_table_info('message') where name = 'progress'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);
        if has_progress == 0 {
            connection.execute_batch("ALTER TABLE message ADD COLUMN progress INTEGER DEFAULT 0")?;
        }
        // 早期库没有 `deviceId` 列（旧版按 deviceName 归属）：补一列，旧行留空。
        let has_device_id: i64 = connection
            .query_row(
                "select count(*) from pragma_table_info('message') where name = 'deviceId'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);
        if has_device_id == 0 {
            connection.execute_batch("ALTER TABLE message ADD COLUMN deviceId TEXT DEFAULT ''")?;
        }
        Ok(Self { connection: Mutex::new(connection) })
    }

    /// 插入或覆盖一条记录（V5 的 fileId 可能命中同一主键，用 REPLACE 语义）。
    pub fn insert(&self, entry: &Entry) -> rusqlite::Result<()> {
        let connection = self.connection.lock().expect("历史库锁未被污染");
        let create_time = if entry.create_time.is_empty() { format_time(entry.moment()) } else { entry.create_time.clone() };
        connection.execute(
            "insert or replace into message values(\
                ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21)",
            params![
                entry.id,
                entry.kind,
                stored_message(entry),
                entry.device_name,
                entry.data_version,
                entry.to_user,
                bool_int(entry.is_left),
                bool_int(entry.is_file),
                entry.dev_mode,
                entry.file_path,
                bool_int(entry.completed),
                bool_int(entry.received),
                bool_int(entry.is_video),
                entry.video_time,
                entry.file_size,
                entry.size_str,
                create_time,
                entry.time_stamp,
                0,
                entry.progress,
                entry.device_id,
            ],
        )?;
        Ok(())
    }

    /// 更新进度/完成状态等字段（只覆盖非主键列，时间戳保持原值）。
    pub fn update(&self, entry: &Entry) -> rusqlite::Result<()> {
        let connection = self.connection.lock().expect("历史库锁未被污染");
        connection.execute(
            "update message set message=?1, messageType=?2, deviceName=?3, dataVersion=?4, isLeft=?5, \
             isfile=?6, devMode=?7, filePath=?8, completed=?9, recviced=?10, fileSize=?11, \
             fileSizeStr=?12, progress=?13, isdel=0 where id=?14",
            params![
                stored_message(entry),
                entry.kind,
                entry.device_name,
                entry.data_version,
                bool_int(entry.is_left),
                bool_int(entry.is_file),
                entry.dev_mode,
                entry.file_path,
                bool_int(entry.completed),
                bool_int(entry.received),
                entry.file_size,
                entry.size_str,
                entry.progress,
                entry.id,
            ],
        )?;
        Ok(())
    }

    /// 进行中的传输只更新进度列，其余字段（含发送侧已落库的源路径）保持原值。
    pub fn touch_progress(&self, id: &str, progress: i64) -> rusqlite::Result<()> {
        let connection = self.connection.lock().expect("历史库锁未被污染");
        connection.execute("update message set progress=?1 where id=?2", params![progress, id])?;
        Ok(())
    }

    /// 取某台设备最近的 `limit` 条（按 `deviceId` 归属），返回按时间正序。
    pub fn recent(&self, device_id: &str, limit: i64) -> rusqlite::Result<Vec<Entry>> {
        let connection = self.connection.lock().expect("历史库锁未被污染");
        let mut statement = connection.prepare(
            "select id, messageType, message, deviceName, dataVersion, toUser, isLeft, isfile, devMode, \
             filepath, completed, recviced, isVideo, videoTime, fileSize, fileSizeStr, createTime, \
             timeStamp, isdel, progress, deviceId from message where isdel = 0 and deviceId = ?1 \
             order by timeStamp desc limit ?2",
        )?;
        let mut rows = statement.query_map(params![device_id, limit], read_row)?;
        let mut list = Vec::new();
        while let Some(row) = rows.next() {
            list.push(row?);
        }
        list.reverse();
        Ok(list)
    }

    /// 取某台设备**早于** `before_ts` 的一页（自动翻页用，按 `deviceId` 归属），返回按时间正序。
    ///
    /// 边界 `<` 不含 `before_ts` 本身，这样上一页最旧的那条不会在下一页重复出现。
    pub fn older(&self, device_id: &str, before_ts: i64, limit: i64) -> rusqlite::Result<Vec<Entry>> {
        let connection = self.connection.lock().expect("历史库锁未被污染");
        let mut statement = connection.prepare(
            "select id, messageType, message, deviceName, dataVersion, toUser, isLeft, isfile, devMode, \
             filepath, completed, recviced, isVideo, videoTime, fileSize, fileSizeStr, createTime, \
             timeStamp, isdel, progress, deviceId from message where isdel = 0 and deviceId = ?1 and timeStamp < ?2 \
             order by timeStamp desc limit ?3",
        )?;
        let mut rows = statement.query_map(params![device_id, before_ts, limit], read_row)?;
        let mut list = Vec::new();
        while let Some(row) = rows.next() {
            list.push(row?);
        }
        list.reverse();
        Ok(list)
    }

    /// 全设备混合取最近 `limit` 条（首屏与调试用）。
    pub fn recent_all(&self, limit: i64) -> rusqlite::Result<Vec<Entry>> {
        let connection = self.connection.lock().expect("历史库锁未被污染");
        let mut statement = connection.prepare(
            "select id, messageType, message, deviceName, dataVersion, toUser, isLeft, isfile, devMode, \
             filepath, completed, recviced, isVideo, videoTime, fileSize, fileSizeStr, createTime, \
             timeStamp, isdel, progress, deviceId from message where isdel = 0 order by timeStamp desc limit ?1",
        )?;
        let mut rows = statement.query_map(params![limit], read_row)?;
        let mut list = Vec::new();
        while let Some(row) = rows.next() {
            list.push(row?);
        }
        list.reverse();
        Ok(list)
    }

    /// 全设备混合取**早于** `before_ts` 的一页（合并流跨设备往上翻页），返回按时间正序。
    pub fn older_all(&self, before_ts: i64, limit: i64) -> rusqlite::Result<Vec<Entry>> {
        let connection = self.connection.lock().expect("历史库锁未被污染");
        let mut statement = connection.prepare(
            "select id, messageType, message, deviceName, dataVersion, toUser, isLeft, isfile, devMode, \
             filepath, completed, recviced, isVideo, videoTime, fileSize, fileSizeStr, createTime, \
             timeStamp, isdel, progress, deviceId from message where isdel = 0 and timeStamp < ?1 \
             order by timeStamp desc limit ?2",
        )?;
        let mut rows = statement.query_map(params![before_ts, limit], read_row)?;
        let mut list = Vec::new();
        while let Some(row) = rows.next() {
            list.push(row?);
        }
        list.reverse();
        Ok(list)
    }

    /// 软删除一条。
    pub fn remove(&self, id: &str) -> rusqlite::Result<()> {
        let connection = self.connection.lock().expect("历史库锁未被污染");
        connection.execute("update message set isdel = 1 where id = ?1", params![id])?;
        Ok(())
    }

    /// 软删除某台设备的全部记录（清聊天用，按 `deviceId` 归属）。
    pub fn remove_device(&self, device_id: &str) -> rusqlite::Result<usize> {
        let connection = self.connection.lock().expect("历史库锁未被污染");
        Ok(connection.execute("update message set isdel = 1 where deviceId = ?1", params![device_id])?)
    }

    /// 软删除全部。
    pub fn clear(&self) -> rusqlite::Result<()> {
        let connection = self.connection.lock().expect("历史库锁未被污染");
        connection.execute("update message set isdel = 1", [])?;
        Ok(())
    }

    pub fn count(&self) -> rusqlite::Result<i64> {
        let connection = self.connection.lock().expect("历史库锁未被污染");
        Ok(connection.query_row("select count(1) from message where isdel = 0", [], |row| row.get(0))?)
    }
}

impl Entry {
    fn moment(&self) -> i64 {
        if self.time_stamp > 0 { self.time_stamp } else { now_ms() }
    }
}

fn bool_int(value: bool) -> i64 {
    i64::from(value)
}

/// 落库形态：文本加密，文件名明文。
fn stored_message(entry: &Entry) -> String {
    if entry.kind == TYPE_TEXT { enc_message(&entry.message, KEY) } else { entry.message.clone() }
}

/// 读取形态：把落库的值还原成可直接展示的内容。
fn plain_message(kind: i64, stored: &str) -> String {
    if kind == TYPE_TEXT { dec_message(stored, KEY).unwrap_or_default() } else { stored.to_string() }
}

fn read_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Entry> {
    let kind: i64 = row.get(1)?;
    let stored: String = row.get(2)?;
    Ok(Entry {
        id: row.get(0)?,
        kind,
        message: plain_message(kind, &stored),
        device_name: row.get(3)?,
        // 第 21 列。`deviceId` 是后期 `ALTER TABLE` 补在表尾的，故排在 `progress` 之后；
        // 旧版按设备名归属的行留空串，「清空消息」那类按设备删除碰不到它（也不该碰）。
        device_id: row.get(20)?,
        data_version: row.get(4)?,
        to_user: row.get(5)?,
        is_left: row.get::<_, i64>(6)? != 0,
        is_file: row.get::<_, i64>(7)? != 0,
        dev_mode: row.get(8)?,
        file_path: row.get(9)?,
        completed: row.get::<_, i64>(10)? != 0,
        received: row.get::<_, i64>(11)? != 0 || row.get::<_, i64>(10)? != 0,
        is_video: row.get::<_, i64>(12)? != 0,
        video_time: row.get(13)?,
        file_size: row.get(14)?,
        size_str: row.get(15)?,
        create_time: row.get(16)?,
        time_stamp: row.get(17)?,
        progress: row.get(19)?,
    })
}

/// `yyyy-MM-dd HH:mm:ss`（本地时区，与原版 `TimeTools::getFormatTime()` 同形）。
pub fn format_time(millis: i64) -> String {
    format_time_with_offset(millis, local_offset_ms())
}

/// 可注入时区偏移的版本，单测用它保证与时区无关。
pub fn format_time_with_offset(millis: i64, offset_ms: i64) -> String {
    let local = millis + offset_ms;
    let days = local.div_euclid(86_400_000);
    let second = local.rem_euclid(86_400_000) / 1000;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}",
        second / 3600,
        (second / 60) % 60,
        second % 60
    )
}

/// 当前时区相对 UTC 的偏移（毫秒，含夏令时），取不到时按 UTC 处理。
pub fn local_offset_ms() -> i64 {
    #[cfg(windows)]
    {
        use windows::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};
        let mut info = TIME_ZONE_INFORMATION::default();
        // 0 = UNKNOWN，1 = STANDARD，2 = DAYLIGHT，0xFFFFFFFF = 失败
        let kind = unsafe { GetTimeZoneInformation(&mut info) };
        if kind == u32::MAX {
            return 0;
        }
        let mut bias = info.Bias;
        if kind == 2 {
            bias += info.DaylightBias;
        }
        -(bias as i64) * 60_000
    }
    #[cfg(not(windows))]
    {
        date_offset_ms()
    }
}

/// 类 Unix 取时区偏移：std 没有本地时区 API，就问一句 `date +%z`（输出形如 `+0800`）。
/// 工具缺失、退出码非零或输出不合法时按 UTC。
#[cfg(not(windows))]
fn date_offset_ms() -> i64 {
    let output = match std::process::Command::new("date").arg("+%z").output() {
        Ok(output) if output.status.success() => output,
        _ => return 0,
    };
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let bytes = text.as_bytes();
    if bytes.len() != 5 || (bytes[0] != b'+' && bytes[0] != b'-') {
        return 0;
    }
    let hours: i64 = match std::str::from_utf8(&bytes[1..3]).ok().and_then(|value| value.parse().ok()) {
        Some(value) => value,
        None => return 0,
    };
    let minutes: i64 = match std::str::from_utf8(&bytes[3..5]).ok().and_then(|value| value.parse().ok()) {
        Some(value) => value,
        None => return 0,
    };
    let sign = if bytes[0] == b'-' { -1 } else { 1 };
    sign * (hours * 3600 + minutes * 60) * 1000
}

/// Howard Hinnant 的 civil-from-days：把 Unix 天数（1970-01-01 起）换成年月日。
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as i64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db(tag: &str) -> (HistoryDb, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!("lanshare-db-{}-{tag}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let db = HistoryDb::open(&path).expect("临时库可打开");
        (db, path)
    }

    #[test]
    fn columns_match_the_original_table() {
        let (db, path) = temp_db("columns");
        let names: Vec<String> = {
            let connection = db.connection.lock().unwrap();
            let mut statement = connection.prepare("pragma table_info(message)").unwrap();
            let rows = statement.query_map([], |row| row.get::<_, String>(1)).unwrap();
            rows.flatten().collect()
        };
        let expected = [
            "id",
            "messageType",
            "message",
            "deviceName",
            "dataVersion",
            "toUser",
            "isLeft",
            "isfile",
            "devMode",
            "filepath",
            "completed",
            "recviced",
            "isVideo",
            "videoTime",
            "fileSize",
            "fileSizeStr",
            "createTime",
            "timeStamp",
            "isdel",
            "progress",
            "deviceId",
        ];
        assert_eq!(names, expected);
        drop(db);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn text_is_encrypted_at_rest_and_plain_on_read() {
        let (db, path) = temp_db("cipher");
        let entry = Entry {
            id: "t1".into(),
            kind: TYPE_TEXT,
            message: "你好，局域网".into(),
            device_name: "手机".into(),
            device_id: "uuid-phone".into(),
            is_left: true,
            time_stamp: 1_700_000_000_000,
            ..Default::default()
        };
        db.insert(&entry).unwrap();
        let raw: String = {
            let connection = db.connection.lock().unwrap();
            connection.query_row("select message from message where id = 't1'", [], |row| row.get(0)).unwrap()
        };
        assert_ne!(raw, "你好，局域网", "文本必须密文存储");
        assert!(raw.chars().all(|c| c.is_ascii_hexdigit()), "密文应是 hex: {raw}");
        let back = db.recent("uuid-phone", 10).unwrap();
        assert_eq!(back[0].message, "你好，局域网");
        assert!(back[0].is_left);
        assert!(!back[0].create_time.is_empty());
        drop(db);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn file_entry_keeps_plain_name_and_progress() {
        let (db, path) = temp_db("file");
        let entry = Entry {
            id: "f1".into(),
            kind: TYPE_FILE,
            message: "照片.zip".into(),
            device_name: "PC".into(),
            device_id: "uuid-pc".into(),
            is_file: true,
            file_path: "C:/LANShare/照片.zip".into(),
            file_size: 2048,
            size_str: "2.0KB".into(),
            progress: 40,
            time_stamp: 1_700_000_000_001,
            ..Default::default()
        };
        db.insert(&entry).unwrap();
        let list = db.recent("uuid-pc", 10).unwrap();
        assert_eq!(list[0].message, "照片.zip", "文件名明文存储");
        assert_eq!(list[0].progress, 40);
        assert_eq!(list[0].file_size, 2048);
        assert!(!list[0].received);

        let mut done = entry.clone();
        done.completed = true;
        done.progress = 100;
        db.update(&done).unwrap();
        let list = db.recent("uuid-pc", 10).unwrap();
        assert!(list[0].completed);
        assert!(list[0].received, "完成后 received 必须为真");
        assert_eq!(list[0].progress, 100);
        drop(db);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn same_id_is_replaced_not_dropped() {
        let (db, path) = temp_db("replace");
        let mut entry = Entry { id: "k".into(), kind: TYPE_FILE, message: "a".into(), ..Default::default() };
        db.insert(&entry).unwrap();
        entry.message = "b".into();
        db.insert(&entry).unwrap();
        assert_eq!(db.count().unwrap(), 1);
        assert_eq!(db.recent_all(10).unwrap()[0].message, "b");
        drop(db);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn delete_is_soft_and_scoped_per_device() {
        let (db, path) = temp_db("delete");
        db.insert(&Entry { id: "1".into(), device_id: "A".into(), time_stamp: 1, ..Default::default() }).unwrap();
        db.insert(&Entry { id: "2".into(), device_id: "A".into(), time_stamp: 2, ..Default::default() }).unwrap();
        db.insert(&Entry { id: "3".into(), device_id: "B".into(), time_stamp: 3, ..Default::default() }).unwrap();
        assert_eq!(db.count().unwrap(), 3);
        db.remove("1").unwrap();
        assert_eq!(db.count().unwrap(), 2);
        assert_eq!(db.recent("A", 10).unwrap().len(), 1);
        db.remove_device("A").unwrap();
        assert_eq!(db.count().unwrap(), 1);
        db.clear().unwrap();
        assert_eq!(db.count().unwrap(), 0);
        drop(db);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn recent_returns_chronological_order_and_respects_limit() {
        let (db, path) = temp_db("order");
        for index in 1..=5 {
            db.insert(&Entry {
                id: format!("{index}"),
                kind: TYPE_FILE,
                message: format!("m{index}"),
                device_id: "A".into(),
                time_stamp: index,
                ..Default::default()
            })
            .unwrap();
        }
        let list = db.recent("A", 3).unwrap();
        let names: Vec<String> = list.iter().map(|entry| entry.message.clone()).collect();
        assert_eq!(names, ["m3", "m4", "m5"], "取最近 3 条且按时间正序: {names:?}");
        // 「清空消息」靠 `deviceId` 归属：读回的每一行必须带着它，否则合并流里的副本清不掉。
        let owners: Vec<String> = list.iter().map(|entry| entry.device_id.clone()).collect();
        assert!(owners.iter().all(|owner| owner == "A"), "读回应带 deviceId: {owners:?}");
        drop(db);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn older_returns_page_before_cursor_and_excludes_the_boundary() {
        let (db, path) = temp_db("older");
        for index in 1..=5 {
            db.insert(&Entry {
                id: format!("{index}"),
                kind: TYPE_FILE,
                message: format!("m{index}"),
                device_id: "A".into(),
                time_stamp: index,
                ..Default::default()
            })
            .unwrap();
        }
        // 早于第 4 条（time_stamp < 4）取 2 条：拿到 2、3，正序，不含边界 4
        let list = db.older("A", 4, 2).unwrap();
        let names: Vec<String> = list.iter().map(|entry| entry.message.clone()).collect();
        assert_eq!(names, ["m2", "m3"], "边界 < 不含 before_ts: {names:?}");
        // 再往前取一页，拿到最早的 m1
        let prev = db.older("A", 2, 2).unwrap();
        let prev_names: Vec<String> = prev.iter().map(|entry| entry.message.clone()).collect();
        assert_eq!(prev_names, ["m1"], "到顶时不足一页");
        drop(db);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn format_time_civil_conversion_is_correct() {
        const CST: i64 = 8 * 3600 * 1000;
        assert_eq!(format_time_with_offset(0, CST), "1970-01-01 08:00:00");
        assert_eq!(format_time_with_offset(1_700_000_000_000, CST), "2023-11-15 06:13:20");
        // 闰日边界：2024-02-29 00:00 UTC
        assert_eq!(format_time_with_offset(1_709_164_800_000, CST), "2024-02-29 08:00:00");
        // 年前分界：2025-01-01 00:00 UTC
        assert_eq!(format_time_with_offset(1_735_689_600_000, CST), "2025-01-01 08:00:00");
        // UTC 与负偏移也不能算错日
        assert_eq!(format_time_with_offset(1_735_689_600_000, 0), "2025-01-01 00:00:00");
        assert_eq!(format_time_with_offset(1_735_689_600_000, -5 * 3600 * 1000), "2024-12-31 19:00:00");
        // 当前时区不能抛出异常，且偏移应当落在 ±14 小时内
        let offset = local_offset_ms();
        assert!(offset.abs() <= 14 * 3600 * 1000, "{offset}");
    }
}

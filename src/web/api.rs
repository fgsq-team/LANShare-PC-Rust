//! 网页端的全部业务端点，逐条对应原版 `src/LHttpServer.cpp` 里的 `addPath` 列表。
//!
//! 契约都是照原版抄的：字段名、文案、状态码、以及几处「原版就是这样」的怪癖
//! （`/media` 的 `folderIndex` 缺键当 0、405 回的是 404 状态行、非空但查不到的
//! token 不再回 `token` 响应头）。要偏离的地方都在注释里写明了原因。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use serde_json::{json, Value};

use crate::lanshare::config::Config;
use crate::lanshare::crypto;
use crate::lanshare::db::format_time;
use crate::lanshare::device::Device;
use crate::lanshare::walk::size_str;
use crate::lanshare::{Engine, NetEvent};

use super::{embed, http, media, multipart, sessions, tokens, ws, zip};

/// 处理器统一签名，写起来短一点。
type Call = std::io::Result<()>;

/// 原版 `DEFAULT_WEB_ROOT_FILE_PATH`：Windows 从盘符根看起，其它平台从家目录看起。
pub fn root_path() -> String {
    #[cfg(windows)]
    {
        "/".to_string()
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(|home| home.to_string_lossy().into_owned())
            .unwrap_or_else(|| "/".to_string())
    }
}

/// 原版 `LHttpServer::webMenus`。`apps`（软件）与 `draw`（远程绘图）在原版就是注释掉的，
/// 背后的 `/apps`、`/apkfile`、`/appicon`、`DRAW_EVENT` 一个都没实现，这里同样不提供。
pub fn web_menus() -> Value {
    json!([
        { "key": "media", "text": "图片", "icon": "nav-item-media-img" },
        { "key": "files", "text": "文件列表", "icon": "nav-item-media-folder" },
        { "key": "chat", "text": "消息记录", "icon": "nav-item-media-record" },
    ])
}

/// 原版 `paths`：只有命中这些模式才要求带有效 token。
///
/// 注意 `/compressFiles` 与 `/downloadZipFile` 不在表里——原版就没给它们上守卫，
/// 保持原样（前者只能打包网页上已经能读到的路径，后者只发 `.cache` 里的临时包）。
pub const GUARDED: [&str; 10] = [
    "/apps",
    "/media",
    "/files",
    "/compressMedias",
    "/apkfile/*",
    "/file/*",
    "/wss",
    "/imageload/*",
    "/chatUploadFile",
    "/uploadFile",
];

/// 原版 `getThumnail(path, 200, 200)` 的边长。
const THUMBNAIL: u32 = 200;

/// 原版 `/uploadFile` 的落盘子目录名。
const WEB_UPLOAD_DIR: &str = "网页收到的文件";

/// 路由表。原版靠 `sortPath()` 保证短模式先匹配，这里交给 `http::sorted`。
pub fn routes() -> Vec<http::Route> {
    http::sorted(vec![
        http::route("/", None, index),
        http::route("/hello", None, hello),
        http::route("/files", None, files),
        http::route("/media", Some("POST"), media_list),
        http::route("/wss", None, socket),
        http::route("/drawable", None, drawable),
        http::route("/file/*", None, file_download),
        http::route("/images/*", None, asset),
        http::route("/favicon.ico", None, favicon),
        http::route("/css/*", None, asset),
        http::route("/js/*", None, asset),
        http::route("/initConfig", None, init_config),
        http::route("/checkPass", Some("POST"), check_pass),
        http::route("/uploadFile", Some("POST"), upload_file),
        http::route("/imageload/*", None, imageload),
        http::route("/compressFiles", Some("POST"), compress_files),
        http::route("/compressMedias", Some("POST"), compress_medias),
        http::route("/downloadZipFile", None, download_zip),
        http::route("/chatUploadFile", Some("POST"), chat_upload_file),
    ])
}

/// 原版的 `setRequestFilter`：免授权模式全放行，否则守卫路径要拿 token 查库。
///
/// POST 从 `token` 请求头取，其余从查询串取——浏览器的下载与 WebSocket 握手都只能走查询串。
pub fn guard(engine: &Arc<Engine>, request: &http::Request, response: &mut http::Response) -> std::io::Result<bool> {
    if engine.config().open_web_service {
        return Ok(true);
    }
    if !GUARDED.iter().any(|pattern| http::path_matches(pattern, &request.url)) {
        return Ok(true);
    }
    let token = if request.method == "POST" { request.header("token") } else { request.param("token") };
    if !token.is_empty() && tokens::shared().and_then(|db| db.query(token)).is_some() {
        return Ok(true);
    }
    response.write_302(&request.conn, "访问权限已失效，请刷新主页后授权", "/")?;
    Ok(false)
}

// —— 静态资源 ——————————————————————————————————————————————

/// `GET /`：主页。原版是 `writeFile(":/web/web/lanshare.html")`。
fn index(_engine: &Arc<Engine>, request: &mut http::Request, response: &mut http::Response) -> Call {
    response.write(&request.conn, embed::INDEX_HTML)
}

/// `GET /favicon.ico`：原版直接拿应用图标当 favicon。
fn favicon(_engine: &Arc<Engine>, request: &mut http::Request, response: &mut http::Response) -> Call {
    response.write_bytes(&request.conn, embed::FAVICON, "image/png")
}

/// `/images/*`、`/css/*`、`/js/*`：都从内嵌资源表按 URL 原样取。
fn asset(_engine: &Arc<Engine>, request: &mut http::Request, response: &mut http::Response) -> Call {
    match embed::lookup(&request.url) {
        Some(bytes) => {
            let kind = http::content_type_of(&request.url);
            response.write_bytes(&request.conn, bytes, &kind)
        }
        None => response.write_404(&request.conn),
    }
}

/// `GET /drawable?name=X`：聊天区那两个设备图标。
///
/// 原版查的是 `:/web/web/drawable/<name>.png`，`ic_image_un_select` 之类的键在 Qt 资源里
/// 本来就不存在，照样回 404，这里不替它补。
fn drawable(_engine: &Arc<Engine>, request: &mut http::Request, response: &mut http::Response) -> Call {
    let name = request.param("name").to_string();
    let key = format!("/drawable/{name}.png");
    match embed::lookup(&key) {
        Some(bytes) => response.write_bytes(&request.conn, bytes, "image/png"),
        None => response.write_404(&request.conn),
    }
}

/// `GET /hello`：探活。
fn hello(_engine: &Arc<Engine>, request: &mut http::Request, response: &mut http::Response) -> Call {
    response.write_text(&request.conn, "ok")
}

// —— 令牌与授权 ————————————————————————————————————————————

/// `POST /initConfig`：给网页回根目录、主题、菜单，并决定这次要不要弹授权框。
///
/// 三条分支与原版一一对应：这台 IP 勾过免确认 → 沿用旧令牌；没带 token → 发新令牌并回
/// `token` 响应头；带了但库里查不到 → 也发新令牌，但**不回**那个响应头（原版如此）。
fn init_config(engine: &Arc<Engine>, request: &mut http::Request, response: &mut http::Response) -> Call {
    let config = engine.config();
    let db = tokens::shared();
    let mut token = request.header("token").to_string();
    let mut pass = false;
    let mut add_header = false;
    let mut announce = false;
    match db.as_ref().and_then(|db| db.query_custom_ip(&request.client_ip)) {
        Some(grant) => {
            token = grant.id;
            pass = true;
        }
        None => {
            // 空 token 不去查库：原版 `queryToken("")` 必然为空，结果一样
            let known = !token.is_empty() && db.as_ref().and_then(|db| db.query(&token)).is_some();
            if known {
                pass = true;
            } else {
                add_header = token.is_empty();
                token = crypto::new_id();
                if config.open_web_service {
                    pass = true;
                } else {
                    announce = true;
                }
            }
        }
    }
    if add_header {
        response.header("token", &token);
    }
    if announce {
        engine.publish(NetEvent::WebGrant { token: token.clone(), ip: request.client_ip.clone() });
    }
    let body = json!({
        "rootPath": root_path(),
        // 配置里的空串在网页端是「跟随系统」，与 `sessions::push_theme` 的换算一致
        "theme": if config.theme.is_empty() { "follow_system" } else { config.theme.as_str() },
        "menus": web_menus(),
        "token": token,
        "pass": pass,
        "name": config.client_name,
    });
    response.write_json(&request.conn, &body.to_string())
}

/// `POST /checkPass`：前端每 2 秒问一次，用户点了「同意」就立刻放行。
fn check_pass(_engine: &Arc<Engine>, request: &mut http::Request, response: &mut http::Response) -> Call {
    let token = request.header("token").to_string();
    let pass = tokens::shared().and_then(|db| db.query(&token)).is_some();
    response.write_json(&request.conn, &json!({ "pass": pass }).to_string())
}

/// `GET|POST /wss`：升级成 WebSocket 后会话就在这里跑到连接断掉为止。
fn socket(engine: &Arc<Engine>, request: &mut http::Request, _response: &mut http::Response) -> Call {
    let key = request.header("sec-websocket-key").to_string();
    ws::accept(&request.conn, &key)?;
    let session = sessions::Session::new(request.conn.clone(), request.client_ip.clone());
    sessions::add(session.clone());
    // 原版 `LWebSocketServer` 一建好就 `sendDeviceList()`，前端才有「选择发送对象」的下拉
    sessions::push_devices(&engine.devices());
    while engine.is_running() && !session.is_closed() {
        let Ok(Some(frame)) = ws::read_frame(&session.conn) else { break };
        match frame.opcode {
            ws::OP_TEXT => on_text(engine, &session, &frame.payload),
            ws::OP_CLOSE => {
                ws::send_close(&session.conn);
                break;
            }
            ws::OP_PING => {
                let _ = ws::write_frame(&session.conn, ws::OP_PONG, &frame.payload);
            }
            _ => {}
        }
    }
    sessions::remove(&session);
    Ok(())
}

/// 网页发来的 `{"cmd":1,"message":…,"selectedDevice":"ip:port","isClip":…}`。
///
/// 剪贴板标记与目标设备都转交给引擎，桌面端的气泡与 UDP 发送都在那边统一处理。
fn on_text(engine: &Arc<Engine>, session: &sessions::Session, payload: &[u8]) {
    let Ok(value) = serde_json::from_slice::<Value>(payload) else { return };
    if value.get("cmd").and_then(Value::as_i64) != Some(sessions::SEND_MESSAGE) {
        return;
    }
    let message = value.get("message").and_then(Value::as_str).unwrap_or_default();
    let selected = value.get("selectedDevice").and_then(Value::as_str).unwrap_or_default();
    let clipboard = value.get("isClip").and_then(Value::as_bool).unwrap_or(false);
    let _ = engine.relay_from_web(selected, message, clipboard, &session.ip);
}

// —— 文件浏览 ——————————————————————————————————————————————

/// `POST /files`：`{"path":…,"isBack":…}` -> `{"list":[…],"path":…}`。
///
/// 首项固定是 `...`（返回上一级），Windows 的根目录列盘符而不是列 `C:\` 的内容。
fn files(_engine: &Arc<Engine>, request: &mut http::Request, response: &mut http::Response) -> Call {
    let text = request.request_text()?;
    let value = parse(&text);
    let mut path = value.get("path").and_then(Value::as_str).unwrap_or_default().to_string();
    let back = value.get("isBack").and_then(Value::as_bool).unwrap_or(false);
    if path.is_empty() {
        path = root_path();
    }
    if back {
        path = parent_of(&path).unwrap_or_else(root_path);
    }
    let mut list = vec![json!({
        "isDirectory": true,
        "isFile": false,
        "length": 0,
        "name": "...",
        // 原版把「当前目录」写给 `...` 这一行，前端再拿它配 `isBack=true` 跳一次
        "path": path.clone(),
        "time": 0,
    })];
    if cfg!(windows) && path == root_path() {
        list.extend(drives());
    } else {
        list.extend(listing(&path));
    }
    response.write_json(&request.conn, &json!({ "list": list, "path": path }).to_string())
}

/// `GET /file/*?path=<绝对路径>`：下载或预览单个文件，支持 Range 续传。
fn file_download(_engine: &Arc<Engine>, request: &mut http::Request, response: &mut http::Response) -> Call {
    let path = request.param("path").to_string();
    response.send_file(&request.conn, &path)
}

/// 盘符列表。`QDir::drives()` 只给真实存在的逻辑驱动器，这里用能否 `metadata` 来筛。
fn drives() -> Vec<Value> {
    let mut rows = Vec::new();
    for letter in b'A'..=b'Z' {
        let root = format!("{}:/", letter as char);
        let Ok(meta) = std::fs::metadata(&root) else { continue };
        rows.push(json!({
            "isDirectory": true,
            "isFile": false,
            "length": 0,
            // 原版 `absoluteFilePath().left(2)`，所以显示的是没有斜杠的 `C:`
            "name": format!("{}:", letter as char),
            "path": root,
            "time": stamp_of(&meta),
        }));
    }
    rows
}

/// 一个目录的内容：文件夹在前、同组按名字排，与原版 `listDirectory` 的比较器一致。
fn listing(path: &str) -> Vec<Value> {
    let Ok(reader) = std::fs::read_dir(path) else { return Vec::new() };
    let mut entries: Vec<std::fs::DirEntry> = reader.flatten().collect();
    entries.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    // 稳定排序按「是不是目录」再排一次，名字序在组内保留
    entries.sort_by_key(|entry| !entry.path().is_dir());
    entries
        .iter()
        .map(|entry| {
            let meta = entry.metadata().ok();
            let is_dir = meta.as_ref().map(|value| value.is_dir()).unwrap_or_else(|| entry.path().is_dir());
            let name = entry.file_name().to_string_lossy().into_owned();
            // 一律给正斜杠：这套字符串会回到前端再拼进 URL，也会被 ZIP 当条目名前缀比对
            let full = entry.path().to_string_lossy().replace('\\', "/");
            let size = meta.as_ref().map(|value| value.len() as i64).unwrap_or(0);
            let time = meta.as_ref().map(stamp_of).unwrap_or_default();
            json!({
                "isDirectory": is_dir,
                "isFile": !is_dir,
                "length": if is_dir { 0 } else { size },
                "name": name,
                "path": full,
                "time": time,
            })
        })
        .collect()
}

// —— 打包与上传 ————————————————————————————————————————————

/// `POST /compressMedias`：`{"list":[{"isDirectory":…,"index":…}]}` -> `{"tempFile":…}`。
///
/// 目录分支**不递归**，只装图库扫到的那些图片（原版的 `mediaFolder.mediaFiles` 就是这个集合），
/// 条目名是 `目录名/文件名`；单图分支的条目名就是文件名。
fn compress_medias(engine: &Arc<Engine>, request: &mut http::Request, response: &mut http::Response) -> Call {
    let text = request.body_text()?;
    let value = parse(&text);
    let store = media::snapshot();
    let mut items: Vec<(String, PathBuf)> = Vec::new();
    for row in list_of(&value) {
        let index = row.get("index").and_then(Value::as_i64).unwrap_or(-1);
        if row.get("isDirectory").and_then(Value::as_bool).unwrap_or(false) {
            let Some(folder) = slot(&store.folders, index) else { continue };
            let dir_name = file_name(&folder.path);
            for number in &folder.files {
                if let Some(file) = store.all.get(*number) {
                    items.push((format!("{dir_name}/{}", file_name(file)), PathBuf::from(file)));
                }
            }
        } else if let Some(file) = slot(&store.all, index) {
            items.push((file_name(file), PathBuf::from(file)));
        }
    }
    pack(engine, request, response, &items)
}

/// `POST /compressFiles`：`{"list":["绝对路径"]}` -> `{"tempFile":…}`。
///
/// 前缀长度都**不加 1**（与 `/compressMedias` 不同），切出来的条目名带前导斜杠，
/// 由 `zip::normalize` 吃掉；因此一个目录的内容会被摊平到包的根上，这也是原版行为。
fn compress_files(engine: &Arc<Engine>, request: &mut http::Request, response: &mut http::Response) -> Call {
    let text = request.body_text()?;
    let value = parse(&text);
    let mut items: Vec<(String, PathBuf)> = Vec::new();
    for row in list_of(&value) {
        let Some(path) = row.as_str() else { continue };
        let target = PathBuf::from(path);
        let Ok(meta) = std::fs::metadata(&target) else {
            return response.write_500(&request.conn, "路径不存在");
        };
        if meta.is_file() {
            zip::plan(&parent_of(path).unwrap_or_default(), &target, &mut items);
        } else if meta.is_dir() {
            zip::plan(&trim_separators(path), &target, &mut items);
        } else {
            return response.write_500(&request.conn, "不支持的文件类型");
        }
    }
    pack(engine, request, response, &items)
}

/// 把选中的条目打成 `.cache/<uuid>.zip`，回文件名给前端去 `/downloadZipFile` 取。
fn pack(engine: &Arc<Engine>, request: &mut http::Request, response: &mut http::Response, items: &[(String, PathBuf)]) -> Call {
    match zip::create(&cache_dir(&engine.config()), items) {
        Ok((name, _)) => response.write_json(&request.conn, &json!({ "tempFile": name }).to_string()),
        Err(_) => response.write_500(&request.conn, "创建压缩包失败"),
    }
}

/// `GET /downloadZipFile?tempFile=<uuid>.zip`：发完就删，`.cache` 不留垃圾。
fn download_zip(engine: &Arc<Engine>, request: &mut http::Request, response: &mut http::Response) -> Call {
    // 原版直接把 `tempFile` 拼到 `.cache` 后面；这里只取最后一段，挡掉 `../` 穿越
    let name = file_name(request.param("tempFile"));
    let path = cache_dir(&engine.config()).join(&name);
    response.header("Content-Disposition", &disposition(&name));
    response.send_file(&request.conn, &path.to_string_lossy())?;
    let _ = std::fs::remove_file(&path);
    Ok(())
}

/// `POST /uploadFile`：multipart 落到 `保存目录/网页收到的文件/`。
///
/// 与原版一致：这是「本机收到」，只建气泡、不往任何设备转发，也不回推 WS（推了等于
/// 让上传者收到自己的回声）。
fn upload_file(engine: &Arc<Engine>, request: &mut http::Request, response: &mut http::Response) -> Call {
    let uploads = match land(engine, request) {
        Ok(uploads) => uploads,
        Err(_) => return response.write_500(&request.conn, "上传文件失败"),
    };
    let mut total = 0i64;
    for upload in &uploads {
        total += upload.size as i64;
        engine.note_web_upload(&upload.name, &upload.path.to_string_lossy(), upload.size as i64);
    }
    response.write_text(&request.conn, &format!("文件上传成功，大小: {}", size_str(total)))
}

/// `POST /chatUploadFile?address=<ip:port>`：网页直接把文件发给某台在线设备。
///
/// 原版是把 HTTP 体包成 `LFile::STREAM` 走旧式帧直发、不落盘。这里先落盘再交给
/// `Engine::send_files`，好处是按对端的协议版本自动选 V4/V5、能在桌面端看到进度气泡；
/// 代价是磁盘上会多一份副本（就在 `网页收到的文件/`，和 `/uploadFile` 同一个目录）。
fn chat_upload_file(engine: &Arc<Engine>, request: &mut http::Request, response: &mut http::Response) -> Call {
    let address = request.param("address").to_string();
    if address.is_empty() {
        return response.write_500(&request.conn, "所选设备不存在或者不在线");
    }
    let Some(device) = device_by_address(engine, &address) else {
        return response.write_500(&request.conn, "文件上传失败");
    };
    let uploads = match land(engine, request) {
        Ok(uploads) => uploads,
        Err(_) => return response.write_500(&request.conn, "文件上传失败"),
    };
    let total: i64 = uploads.iter().map(|upload| upload.size as i64).sum();
    let paths: Vec<PathBuf> = uploads.iter().map(|upload| upload.path.clone()).collect();
    if engine.send_files(&device.ip, device.port, &paths).is_err() {
        return response.write_500(&request.conn, "文件上传失败");
    }
    response.write_text(&request.conn, &format!("文件上传成功，大小: {}", size_str(total)))
}

/// 把 multipart 请求体逐个部件落盘，返回 `Upload` 列表。
fn land(engine: &Arc<Engine>, request: &mut http::Request) -> std::io::Result<Vec<multipart::Upload>> {
    let dir = Path::new(&engine.config().save_path).join(WEB_UPLOAD_DIR);
    let boundary = boundary_of(request.header("content-type"));
    let mut body = request.body_source();
    multipart::read_upload(&mut body, &boundary, &dir)
}

// —— 图库 ————————————————————————————————————————————————

/// `POST /media`：`{"folderIndex":…}` -> 裸数组。
///
/// `-1` 列目录（名字带张数），否则列该目录里的每张图。键缺失时按 0 处理，
/// 因为原版用的是 `object["folderIndex"].toInt()`。
fn media_list(_engine: &Arc<Engine>, request: &mut http::Request, response: &mut http::Response) -> Call {
    let text = request.body_text()?;
    let folder_index = folder_index(&text);
    let rows = media_rows(&media::snapshot(), folder_index);
    response.write_json(&request.conn, &Value::Array(rows).to_string())
}

/// `/media` 的正文：`-1` 列目录（名字带张数），否则列该目录里的每张图。
///
/// 拆成纯函数是为了能喂一份本地表做单测——全局图库在测试里从来没扫过。
pub fn media_rows(store: &media::Store, folder_index: i64) -> Vec<Value> {
    let mut rows: Vec<Value> = Vec::new();
    if folder_index == -1 {
        for (number, folder) in store.folders.iter().enumerate() {
            // 原版对空目录是 `continue`，一个张数为 0 的卡片都不给
            let Some(first) = folder.files.first().and_then(|slot| store.all.get(*slot)) else { continue };
            rows.push(json!({
                "name": format!("{}({})", file_name(&folder.path), folder.files.len()),
                "path": first,
                "isDirectory": true,
                "count": folder.files.len(),
                "index": number,
                "imgIndex": folder.files.first().copied().unwrap_or_default(),
                "isVideo": false,
            }));
        }
    } else if let Some(folder) = slot(&store.folders, folder_index) {
        for number in &folder.files {
            let Some(file) = store.all.get(*number) else { continue };
            rows.push(json!({
                // 原版这里给的是 `QDir(mediaPath)` 的 `dirName()`（末段文件名）与
                // `absolutePath()`；后者是 QDir 原样存下的那个路径，也就是**完整文件路径**，
                // 不是父目录。前端拿它们拼 `/file/<name>?path=<完整路径>` 交给 `/file` 打开，
                // 写成父目录的话点图片就是一脸 404。
                "name": file_name(file),
                "path": file,
                "isDirectory": false,
                "count": 0,
                "index": number,
                "imgIndex": number,
                "isVideo": false,
            }));
        }
    }
    rows
}

/// `/media` 的目录号：缺键按 0，因为原版用的是 `object["folderIndex"].toInt()`。
pub fn folder_index(text: &str) -> i64 {
    parse(text).get("folderIndex").and_then(Value::as_i64).unwrap_or(0)
}

/// `GET /imageload/*?index=<全局图片号>`：等比缩到 200 方块内的 JPEG。
fn imageload(_engine: &Arc<Engine>, request: &mut http::Request, response: &mut http::Response) -> Call {
    // 负数与非数字都当查不到：原版 `index.toInt()` 得到 0 也可能命中第一张，
    // 但前端只会传它自己从 `/media` 拿到的非负下标
    let index = request.param("index").parse::<usize>().unwrap_or(usize::MAX);
    match media::path_at(index).as_deref().and_then(|path| media::thumbnail_jpeg(path, THUMBNAIL)) {
        Some(jpeg) => response.write_bytes(&request.conn, &jpeg, "image/jpeg"),
        None => response.write_404(&request.conn),
    }
}

// —— 小工具 ————————————————————————————————————————————————

/// JSON 请求体解析：坏 JSON 当成空对象，字段缺省走各端点自己的默认值。
fn parse(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|_| json!({}))
}

/// `{"list":[…]}` 里的那个数组，缺省给空切片。
fn list_of(value: &Value) -> &[Value] {
    value.get("list").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default()
}

/// 下标越界或为负都当查不到，不能像 C++ 那样直接 UB。
fn slot<T>(list: &[T], index: i64) -> Option<&T> {
    usize::try_from(index).ok().and_then(|slot| list.get(slot))
}

/// 去掉结尾的路径分隔符，空串说明本来就在根上。
fn trim_separators(path: &str) -> String {
    path.trim_end_matches(['/', '\\']).to_string()
}

/// 父目录。到顶（`/`、`C:`）返回 `None`，由调用方决定回落根目录。
fn parent_of(path: &str) -> Option<String> {
    let body = trim_separators(path);
    let at = body.rfind(['/', '\\'])?;
    let head = &body[..at];
    if head.is_empty() {
        return Some("/".to_string());
    }
    // Windows 的盘符根再往上就没了，补回斜杠才是合法路径
    if head.ends_with(':') {
        return Some(format!("{head}/"));
    }
    Some(head.to_string())
}

/// 路径最后一段。`"C:/"` 这种根目录没有「最后一段」，返回空串。
fn file_name(path: &str) -> String {
    let body = trim_separators(path);
    match body.rsplit(['/', '\\']).next() {
        Some(tail) if tail.contains(':') => String::new(),
        Some(tail) => tail.to_string(),
        None => String::new(),
    }
}

/// 从 `Content-Type` 里取 multipart 分隔符，取不到返回空串（`read_upload` 会报错）。
fn boundary_of(content_type: &str) -> String {
    content_type
        .split(';')
        .find_map(|piece| piece.trim().strip_prefix("boundary=").map(|value| trim_quotes(value.trim()).to_string()))
        .unwrap_or_default()
}

fn trim_quotes(text: &str) -> &str {
    text.trim_matches('"')
}

/// 压缩包存放处：`保存目录/.cache`，与原版 `CACHE_DIR` 同一个位置。
fn cache_dir(config: &Config) -> PathBuf {
    Path::new(&config.save_path).join(zip::CACHE_DIR.trim_start_matches('/'))
}

/// 文件时间戳，`yyyy-MM-dd HH:mm:ss` 本地时区——直接复用历史库那份换算。
fn stamp_of(meta: &std::fs::Metadata) -> String {
    let millis = meta
        .modified()
        .ok()
        .and_then(|when| when.duration_since(UNIX_EPOCH).ok())
        .map(|span| span.as_millis() as i64)
        .unwrap_or(0);
    format_time(millis)
}

/// 在线表按 `ip:port` 找设备，与网页下拉里 `device.address` 同一个键。
fn device_by_address(engine: &Arc<Engine>, address: &str) -> Option<Device> {
    engine.devices().into_iter().find(|device| device.key() == address)
}

/// 下载文件名：给 ASCII 的一份、给 UTF-8 的一份，老浏览器与新浏览器都能拿到中文名。
fn disposition(name: &str) -> String {
    format!("attachment; filename=\"{name}\"; filename*=UTF-8''{}", http::percent_encode(name))
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};

    use super::*;
    use crate::lanshare::stream::Conn;

    fn config() -> Config {
        let mut config = Config::default();
        config.save_path = std::env::temp_dir().join(format!("lanshare-web-{}", std::process::id())).to_string_lossy().into_owned();
        config
    }

    fn engine() -> Arc<Engine> {
        Arc::new(Engine::for_test(config()))
    }

    /// 走完整条链路：分流 → 解析 → 守卫 → 路由 → 关闭，收回全部响应字节。
    fn exchange(engine: Arc<Engine>, raw: &str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let mut client = TcpStream::connect(address).unwrap();
        let (server, _) = listener.accept().unwrap();
        let bytes = raw.as_bytes();
        let head: [u8; 4] = bytes[..4].try_into().unwrap();
        // 那 4 个字节在真链路上已经被 `tcp::handle` 读走了，这里只把它们交给 `serve` 当行首
        // 前缀，正文从第 5 个字节起才写进 socket
        client.write_all(&bytes[4..]).unwrap();
        // 客户端先半关，服务端的排干才不会白等两秒
        client.shutdown(std::net::Shutdown::Write).unwrap();
        crate::web::serve(engine, Conn::new(server).unwrap(), head).ok();
        // 打包下载那条路会读到二进制的 ZIP 正文，只能按有损方式转成字符串
        let mut raw = Vec::new();
        client.read_to_end(&mut raw).unwrap();
        String::from_utf8_lossy(&raw).into_owned()
    }

    /// 请求行 + 头部，`body` 非空时补上 `Content-Length`。
    fn request_line(method: &str, url: &str, extra: &str, body: &str) -> String {
        format!("{method} {url} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n{extra}Content-Length: {}\r\n\r\n{body}", body.len())
    }

    #[test]
    fn menus_carry_the_three_live_pages() {
        let menus = web_menus().to_string();
        for needle in ["media", "图片", "files", "文件列表", "chat", "消息记录", "nav-item-media-img"] {
            assert!(menus.contains(needle), "菜单里缺 {needle}: {menus}");
        }
        assert!(!menus.contains("apps"), "软件页在原版就没实现，不该出现在菜单里");
    }

    #[test]
    fn parent_and_name_walk_the_two_path_styles() {
        assert_eq!(parent_of("/home/user").as_deref(), Some("/home"));
        assert_eq!(parent_of("/home").as_deref(), Some("/"));
        assert_eq!(parent_of("/"), None, "到顶了");
        assert_eq!(parent_of("C:/Users").as_deref(), Some("C:/"));
        assert_eq!(parent_of("C:/"), None, "盘符根不能再上");
        assert_eq!(file_name("/home/user/a.txt"), "a.txt");
        assert_eq!(file_name(r"C:\a\b.txt"), "b.txt");
        assert_eq!(file_name("/"), "", "根目录没有名字");
    }

    #[test]
    fn the_multipart_boundary_is_taken_from_the_content_type() {
        assert_eq!(boundary_of("multipart/form-data; boundary=----WebKitFormBoundaryAB12"), "----WebKitFormBoundaryAB12");
        assert_eq!(boundary_of(r#"multipart/form-data; boundary="simple""#), "simple");
        assert_eq!(boundary_of("application/json"), "");
    }

    #[test]
    fn slots_reject_negative_and_out_of_range_indexes() {
        let table = ["a", "b"];
        assert_eq!(slot(&table, 0), Some(&"a"));
        assert_eq!(slot(&table, 1), Some(&"b"));
        assert_eq!(slot(&table, 2), None);
        assert_eq!(slot(&table, -1), None, "JSON 里的 -1 不能读成最后一项");
    }

    #[test]
    fn the_zip_name_is_strip_of_any_directory_part() {
        assert_eq!(file_name("../../etc/passwd"), "passwd");
        let header = disposition("年度报告.zip");
        assert!(header.contains("attachment; filename=\"年度报告.zip\""), "{header}");
        assert!(header.contains("filename*=UTF-8''%"), "{header}");
    }

    #[test]
    fn the_cache_dir_lives_under_the_save_dir() {
        let dir = cache_dir(&config());
        assert!(dir.to_string_lossy().ends_with(".cache"), "{}", dir.display());
    }

    #[test]
    fn hello_and_index_are_served_without_a_token() {
        let text = exchange(engine(), "GET /hello HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(text.contains("200 OK"), "{text}");
        assert!(text.ends_with("ok"), "{text}");
        let page = exchange(engine(), "GET / HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(page.contains("lanshare") || page.len() > 2000, "主页应当是内嵌的那份 HTML");
    }

    /// 未授权的守卫路径要 302 回首页，前端据此弹授权框并轮询 `/checkPass`。
    #[test]
    fn an_unknown_token_is_bounced_back_to_the_index() {
        let text = exchange(engine(), "GET /files?token=nope HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(text.starts_with("HTTP/1.1 302"), "{text}");
        assert!(text.contains("Location: /"), "{text}");
        assert!(text.contains("访问权限已失效"), "{text}");
    }

    #[test]
    fn the_unguarded_paths_never_ask_for_a_token() {
        let text = exchange(engine(), "GET /downloadZipFile?tempFile=x.zip HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(text.starts_with("HTTP/1.1 404"), "不存在的包应当 404: {text}");
    }

    /// 第一次访问：发新令牌、不回 `pass`，并给桌面端丢一个授权请求事件。
    #[test]
    fn a_brand_new_client_gets_a_token_and_a_grant_request() {
        let engine = engine();
        let text = exchange(Arc::clone(&engine), &request_line("POST", "/initConfig", "", "{}"));
        assert!(text.contains("\"pass\":false"), "{text}");
        assert!(text.contains("\"token\":\""), "{text}");
        assert!(text.contains("follow_system") || text.contains("\"theme\""), "{text}");
        let events = engine.take_events(std::time::Duration::from_millis(50));
        assert!(events.iter().any(|event| matches!(event, NetEvent::WebGrant { .. })), "应当请求用户授权");
    }

    /// 同意后令牌进库，`/checkPass` 才该放行。
    #[test]
    fn check_pass_follows_the_grant() {
        let engine = engine();
        exchange(Arc::clone(&engine), &request_line("POST", "/initConfig", "", "{}"));
        let Some(NetEvent::WebGrant { token, ip }) = engine
            .take_events(std::time::Duration::from_millis(50))
            .into_iter()
            .find(|event| matches!(event, NetEvent::WebGrant { .. }))
        else {
            panic!("应当先有一条授权请求");
        };
        let undecided = exchange(Arc::clone(&engine), &request_line("POST", "/checkPass", &format!("token: {token}\r\n"), "{}"));
        assert!(undecided.contains("\"pass\":false"), "{undecided}");
        engine.resolve_web_grant(&token, &ip, true);
        let decided = exchange(Arc::clone(&engine), &request_line("POST", "/checkPass", &format!("token: {token}\r\n"), "{}"));
        assert!(decided.contains("\"pass\":true"), "{decided}");
        // 这张令牌是真写进了共享授权库的，测完撤销掉，别给用户留下一条幽灵授权
        if let Some(db) = tokens::shared() {
            let _ = db.revoke(&token);
        }
    }

    /// 免授权模式连守卫都不走。
    #[test]
    fn the_open_mode_skips_the_whole_guard() {
        let mut config = config();
        config.open_web_service = true;
        let engine = Arc::new(Engine::for_test(config));
        let text = exchange(engine, &request_line("POST", "/media", "", r#"{"folderIndex":-1}"#));
        assert!(text.starts_with("HTTP/1.1 200"), "{text}");
        assert!(text.ends_with("[]"), "还没扫过图库应当是空数组: {text}");
    }

    /// `/media` 缺 `folderIndex` 时按 0 处理，不是 -1——原版是 `toInt()` 的默认值。
    ///
    /// 这条差异很重要：`-1` 是「列全部目录」，`0` 是「列第一个目录里的图」。
    #[test]
    fn a_missing_folder_index_reads_as_zero() {
        assert_eq!(folder_index("{}"), 0, "缺键按 0");
        assert_eq!(folder_index("不是 JSON"), 0, "坏 JSON 也按 0");
        assert_eq!(folder_index(r#"{"folderIndex":-1}"#), -1);
        assert_eq!(folder_index(r#"{"folderIndex":3}"#), 3);
    }

    /// `/media` 里图片行的 `path` 必须是**完整文件路径**：前端把它原样塞进
    /// `/file/<name>?path=…`，只给父目录的话点开就是一脸 404。
    #[test]
    fn media_file_rows_carry_the_full_path() {
        let root = std::env::temp_dir().join(format!("lanshare-media-rows-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("b.png"), b"second").unwrap();
        std::fs::write(root.join("a.png"), b"first").unwrap();
        let store = media::scan_into(&[root.clone()]);
        let rows = media_rows(&store, 0);
        assert_eq!(rows.len(), 2, "{rows:?}");
        // 同目录内按文件名排，所以第一行是 `a.png`
        assert_eq!(rows[0].get("name").and_then(Value::as_str), Some("a.png"));
        let full = root.join("a.png").to_string_lossy().replace('\\', "/");
        assert_eq!(rows[0].get("path").and_then(Value::as_str), Some(full.as_str()), "path 要指到文件本身");
        assert!(std::fs::metadata(rows[0].get("path").and_then(Value::as_str).unwrap()).unwrap().is_file());
        // 目录行也顺着同一个口径：给的是第一张图的完整路径
        let dirs = media_rows(&store, -1);
        assert_eq!(dirs[0].get("path").and_then(Value::as_str), Some(full.as_str()));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 网页上点一张图就是这条请求（`lanshare.js` 的 `openFile`），得真把字节发出来。
    #[test]
    fn a_media_image_downloads_through_the_file_endpoint() {
        let mut config = config();
        config.open_web_service = true;
        let engine = Arc::new(Engine::for_test(config));
        let dir = std::env::temp_dir().join(format!("lanshare-web-file-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("照片.jpg"), b"jpeg-bytes").unwrap();
        let full = dir.join("照片.jpg").to_string_lossy().replace('\\', "/");
        let url = format!("/file/{}?path={}", http::percent_encode("照片.jpg"), http::percent_encode(&full));
        let text = exchange(Arc::clone(&engine), &format!("GET {url} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"));
        assert!(text.starts_with("HTTP/1.1 200"), "{text}");
        assert!(text.contains("image/jpeg"), "按后缀给类型: {text}");
        assert!(text.ends_with("jpeg-bytes"), "正文要在响应里: {text}");
        // 只给目录（我之前回错过 `/media` 的 `path`）就该 404，与原版在 Windows 上一样
        let parent = http::percent_encode(&dir.to_string_lossy().replace('\\', "/"));
        let missed = exchange(engine, &format!("GET /file/x.jpg?path={parent} HTTP/1.1\r\nHost: x\r\n\r\n"));
        assert!(missed.starts_with("HTTP/1.1 404"), "{missed}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `/files` 要能列出临时目录里刚建的文件。
    #[test]
    fn files_lists_a_directory_with_the_up_row_first() {
        let dir = std::env::temp_dir().join(format!("lanshare-web-list-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("子目录")).unwrap();
        std::fs::write(dir.join("周报.txt"), b"123").unwrap();
        let path = dir.to_string_lossy().replace('\\', "/");
        let body = json!({ "path": path, "isBack": false }).to_string();
        let text = exchange(engine(), &request_line("POST", "/files", &format!("token: {}\r\n", crypto::new_id()), &body));
        // 没授权会被 302 拦下，这正是守卫该做的；改用免授权模式再验一次列表
        assert!(text.starts_with("HTTP/1.1 302"), "{text}");

        let mut config = config();
        config.open_web_service = true;
        let listed = exchange(Arc::new(Engine::for_test(config)), &request_line("POST", "/files", "", &body));
        assert!(listed.starts_with("HTTP/1.1 200"), "{listed}");
        assert!(listed.contains("\"name\":\"...\""), "{listed}");
        assert!(listed.contains("周报.txt"), "{listed}");
        assert!(listed.contains("子目录"), "{listed}");
        assert!(listed.contains(&format!("\"path\":\"{path}\"")), "首项要带回当前目录: {listed}");
        // 文件夹要排在文件前面
        let dir_at = listed.find("子目录").unwrap();
        let file_at = listed.find("周报.txt").unwrap();
        assert!(dir_at < file_at, "{listed}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 传了不存在的目录也不该崩，返回只有 `...` 一行的空列表。
    #[test]
    fn a_missing_directory_lists_nothing() {
        let mut config = config();
        config.open_web_service = true;
        let engine = Arc::new(Engine::for_test(config));
        let body = json!({ "path": "/肯定不存在/呀", "isBack": false }).to_string();
        let text = exchange(engine, &request_line("POST", "/files", "", &body));
        assert!(text.starts_with("HTTP/1.1 200"), "{text}");
        // 请求的路径会被回显在 `"path"` 字段里，所以只能数条目：只有 `...` 那一行是目录
        assert_eq!(text.matches("\"isDirectory\":true").count(), 1, "列表里不该有任何条目: {text}");
        assert_eq!(text.matches("\"name\"").count(), 1, "{text}");
    }

    /// `/compressFiles` 的路径不存在时回 500，文案照原版。
    #[test]
    fn compressing_a_missing_path_is_a_500() {
        let mut config = config();
        config.open_web_service = true;
        let engine = Arc::new(Engine::for_test(config));
        let text = exchange(engine, &request_line("POST", "/compressFiles", "", r#"{"list":["/没有这个文件"]}"#));
        assert!(text.starts_with("HTTP/1.1 500"), "{text}");
        assert!(text.contains("路径不存在"), "{text}");
    }

    /// 打包一个目录并把包下回来，验证 ZIP 与 `.cache` 这条链是通的。
    #[test]
    fn a_packed_directory_can_be_downloaded_again() {
        let mut config = config();
        config.open_web_service = true;
        // 先算好缓存目录：`config` 会被引擎接走
        let cache = cache_dir(&config);
        let engine = Arc::new(Engine::for_test(config));
        let dir = std::env::temp_dir().join(format!("lanshare-web-pack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("box")).unwrap();
        std::fs::write(dir.join("box").join("内.txt"), b"zip me").unwrap();
        let box_dir = dir.join("box").to_string_lossy().replace('\\', "/");
        let packed = exchange(Arc::clone(&engine), &request_line("POST", "/compressFiles", "", &json!({ "list": [box_dir] }).to_string()));
        assert!(packed.starts_with("HTTP/1.1 200"), "{packed}");
        let name = packed.rsplit('"').nth(1).unwrap_or_default().to_string();
        assert!(name.ends_with(".zip"), "{packed}");
        let downloaded = exchange(Arc::clone(&engine), &format!("GET /downloadZipFile?tempFile={name} HTTP/1.1\r\nHost: x\r\n\r\n"));
        assert!(downloaded.starts_with("HTTP/1.1 200"), "{downloaded}");
        assert!(downloaded.contains("Content-Disposition: attachment"), "{downloaded}");
        assert!(downloaded.contains("zip me"), "正文要在响应里");
        // 发完即删，`.cache` 不该留东西
        assert!(!cache.join(&name).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `/uploadFile` 的 multipart 部件要落到 `网页收到的文件/`。
    #[test]
    fn an_uploaded_file_lands_in_the_web_folder() {
        let mut config = config();
        config.open_web_service = true;
        let engine = Arc::new(Engine::for_test(config));
        let boundary = "----WebKitFormBoundaryXY";
        let body = format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"上传.txt\"\r\nContent-Type: text/plain\r\n\r\n内容\r\n--{boundary}--\r\n");
        let head = format!("Content-Type: multipart/form-data; boundary={boundary}\r\n");
        let text = exchange(Arc::clone(&engine), &request_line("POST", "/uploadFile", &head, &body));
        assert!(text.starts_with("HTTP/1.1 200"), "{text}");
        assert!(text.contains("文件上传成功"), "{text}");
        let landed = Path::new(&engine.config().save_path).join(WEB_UPLOAD_DIR).join("上传.txt");
        assert_eq!(std::fs::read_to_string(&landed).unwrap(), "内容");
        let events = engine.take_events(std::time::Duration::from_millis(50));
        assert!(events.iter().any(|event| matches!(event, NetEvent::Start(_))), "桌面端应当长出气泡");
        let _ = std::fs::remove_file(&landed);
    }

    /// 设备不在线时 `/chatUploadFile` 直接 500，不碰请求体。
    #[test]
    fn chat_upload_without_a_matching_device_is_a_500() {
        let mut config = config();
        config.open_web_service = true;
        let engine = Arc::new(Engine::for_test(config));
        let text = exchange(engine, &request_line("POST", "/chatUploadFile?address=1.2.3.4:5856", "", "x"));
        assert!(text.starts_with("HTTP/1.1 500"), "{text}");
        assert!(text.contains("文件上传失败"), "{text}");
    }

    /// 内嵌资源要能按 URL 直接取到，取不到的就是 404。
    #[test]
    fn embedded_assets_resolve_by_url() {
        let text = exchange(engine(), "GET /css/lanshare.css HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(text.contains("text/css"), "{text}");
        let missing = exchange(engine(), "GET /js/nope.js HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(missing.starts_with("HTTP/1.1 404"), "{missing}");
        let drawable = exchange(engine(), "GET /drawable?name=ic_win HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(drawable.contains("image/png"), "{drawable}");
    }

    /// 方法不对要 405，且状态行沿用原版的 404 写法。
    #[test]
    fn a_wrong_method_is_rejected_like_the_original() {
        let text = exchange(engine(), "GET /checkPass HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(text.starts_with("HTTP/1.1 404"), "{text}");
        assert!(text.contains("405 Method Not Allowed"), "{text}");
    }
}

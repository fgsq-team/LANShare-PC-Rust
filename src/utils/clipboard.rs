//! 把位图写进系统剪贴板。
//!
//! 文本不用这里：windui 自带 `EventCtx::clipboard_set(&str)`，三平台都实现好了。
//! 图片它没有，而 `arboard` 这类跨平台剪贴板 crate 在当前环境装不上（cargo 到镜像源的
//! TLS 握手失败，本地缓存里也没有对应的 crate），所以走 Win32 的 `CF_DIB`——
//! `windows` crate 本项目本来就有，只多开三个 feature，零下载。
//!
//! 封装成「一个函数、一个 `Result`」的形状：日后网络恢复想换回 arboard，只改本文件，
//! 调用点一行都不用动。

/// 写剪贴板失败的原因。
///
/// 「这个平台没有这条通路」与「Win32 调用失败了」必须分开说：调用方原先把两者都
/// 渲染成一句「该平台暂不支持复制图片」，于是 Windows 上真出错时也在把用户往
/// 「平台不行」上引——本项目第一版就是这么被带偏的。
#[derive(Debug)]
pub enum Error {
    /// 非 Windows：这里只有 `CF_DIB` 一条通路，其余平台的 API 没实现。
    Unsupported,
    /// Win32 某一步没成，带上是哪一步与它的错误。
    Failed { stage: &'static str, message: String },
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unsupported => f.write_str("该平台暂不支持复制图片"),
            Self::Failed { stage, message } => write!(f, "复制图片失败（{stage}）：{message}"),
        }
    }
}

/// 把非预乘 RGBA8 位图写进剪贴板。
///
/// 只有 Windows 有实现；其余平台直接返 `Err(Unsupported)`，由调用方把原因透给用户，
/// 而不是静默什么都不做——用户点了「复制」却什么都没发生，比报错更难排查。
pub fn set_image(rgba: &[u8], width: u32, height: u32) -> Result<(), Error> {
    #[cfg(windows)]
    {
        win32_set_image(rgba, width, height)
    }
    #[cfg(not(windows))]
    {
        let _ = (rgba, width, height);
        Err(Error::Unsupported)
    }
}

/// 40 字节 `BITMAPINFOHEADER`。
///
/// 类型钉在 `u32`：它同时是头里 `biSize` 那一个 DWORD 的值，而 `usize::to_le_bytes()`
/// 在 64 位上吐 8 字节——多出的 4 字节会把整张头撑到 44，写最后一个字段时越界 panic。
#[cfg(windows)]
const INFO_HEADER_SIZE: u32 = 40;

/// BI_RGB：32 位无压缩 DIB，通道顺序是 BGRA。
#[cfg(windows)]
const BI_RGB: u32 = 0;

/// Windows：`CF_DIB` 的内容是「`BITMAPINFOHEADER` 紧跟像素」，必须放在一块
/// `GMEM_MOVEABLE` 的全局内存里，因为 `SetClipboardData` 会**接管**这块内存的所有权。
///
/// 四个必须照办的细节，错了就表现为「粘出来是黑的 / 花色 / 直接失败」：
/// - 成功后绝不能再 `GlobalFree`——那时它已经是剪贴板的内容，释放就是踩别人的内存；
///   只有失败路径才由我们回收。
/// - `biHeight` 取**正值**，即自底向上的传统形态，所以填像素时把行序倒过来。负值
///   （top-down）虽然合法，但剪贴板的老消费者里有一批不认，倒行的成本极低，不值得赌。
/// - 我们的像素是 RGBA，DIB 要的是 BGRA，故逐像素换 R 与 B；alpha 在 `CF_DIB` 里被忽略。
/// - `GlobalUnlock` 的返回值**不能直接当成败判据**，见下面那段。
#[cfg(windows)]
fn win32_set_image(rgba: &[u8], width: u32, height: u32) -> Result<(), Error> {
    use windows::Win32::Foundation::{
        GetLastError, GlobalFree, SetLastError, ERROR_NOT_LOCKED, ERROR_SUCCESS, HANDLE,
    };
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    use windows::Win32::System::Ole::CF_DIB;

    // 把 `GetLastError` 包装成给用户看的那一句：失败阶段 + 系统给出的原因。
    let failed = |stage: &'static str| Error::Failed {
        stage,
        message: windows::core::Error::from_thread().to_string(),
    };

    let (rows, cols) = (height as usize, width as usize);
    let Some(pixels_len) = cols.checked_mul(rows).and_then(|count| count.checked_mul(4)) else {
        return Err(mismatch(width, height, rgba.len()));
    };
    if cols == 0 || rows == 0 || rgba.len() < pixels_len {
        return Err(mismatch(width, height, rgba.len()));
    }
    let Some(total) = pixels_len.checked_add(INFO_HEADER_SIZE as usize) else {
        return Err(mismatch(width, height, rgba.len()));
    };

    // 先分配再开剪贴板：`OpenClipboard` 期间整台机器的剪贴板都被我们占着，
    // 分配失败之类的小错没必要在它里面反复进出。
    let memory = match unsafe { GlobalAlloc(GMEM_MOVEABLE, total) } {
        Ok(memory) => memory,
        Err(_) => return Err(failed("GlobalAlloc")),
    };

    // SAFETY: `GlobalLock` 返回的指针在本函数内使用，长度按 `total` 算好后一次性切出
    // 两段互不重叠的子切片；用完立刻 `GlobalUnlock`。剪贴板调用逐条判返回。
    unsafe {
        let data = GlobalLock(memory);
        if data.is_null() {
            let _ = GlobalFree(Some(memory));
            return Err(failed("GlobalLock"));
        }
        let buffer = std::slice::from_raw_parts_mut(data as *mut u8, total);
        let (header, body) = buffer.split_at_mut(INFO_HEADER_SIZE as usize);
        write_header(header, width as i32, height as i32, pixels_len as u32);
        write_pixels(body, rgba, cols, rows);

        // `GlobalUnlock` 的返回值**不能**直接当成败判据：按文档，把锁计数从 1 减到 0——也就是
        // 我们只 `GlobalLock` 一次时唯一会走到的情况——返回的就是 0（本机实测：报 `Err` 时 last
        // error 正是 `0x00000000`），只有减完还剩锁才返回非零。
        // 于是旧写法 `if GlobalUnlock(memory).is_err()` 在 Windows 上**每次复制都判死**，内存被
        // 提前回收、调用方拿到 false，而提示语把它念成「该平台暂不支持复制图片」——问题从来不是
        // 平台。真失败的特征只写在错误码上：文档规定减到 0 时错误码是 `NO_ERROR`、对象本来就没锁
        // 时是 `ERROR_NOT_LOCKED`，两者都无害；其余值才是 Handle 无效之类的硬失败。
        SetLastError(ERROR_SUCCESS);
        if GlobalUnlock(memory).is_err() {
            let code = GetLastError();
            if code != ERROR_SUCCESS && code != ERROR_NOT_LOCKED {
                let _ = GlobalFree(Some(memory));
                return Err(failed("GlobalUnlock"));
            }
        }

        // 不绑窗口句柄：`None` 即传 NULL，剪贴板不归某一台窗口所有，关掉本窗后内容仍在。
        if OpenClipboard(None).is_err() {
            let _ = GlobalFree(Some(memory));
            return Err(failed("OpenClipboard"));
        }
        // 清空失败不致命：紧随的 `SetClipboardData` 成功即由我们接管内容。
        let _ = EmptyClipboard();
        let delivered = SetClipboardData(CF_DIB.0 as u32, Some(HANDLE(memory.0)));
        let _ = CloseClipboard();
        match delivered {
            Ok(_) => Ok(()),
            Err(error) => {
                // 失败才由我们回收：成功时它已经是剪贴板的内容。
                let _ = GlobalFree(Some(memory));
                Err(Error::Failed { stage: "SetClipboardData", message: error.to_string() })
            }
        }
    }
}

/// 尺寸与像素数对不上时的诊断。走到这一步说明连一块像样的 DIB 都拼不出来，
/// 一个 Win32 调用都不用碰。
#[cfg(windows)]
fn mismatch(width: u32, height: u32, have: usize) -> Error {
    Error::Failed {
        stage: "像素缓冲区",
        message: format!("{width}×{height} 的 RGBA8 数据不完整（只有 {have} 字节）"),
    }
}

/// 就地写好 40 字节的 `BITMAPINFOHEADER`（小端，与内存布局一致）。
///
/// 11 个字段合计正好 40：4 + 4 + 4 + 2 + 2 + 4 + 4 + 4 + 4 + 4 + 4。写完恰好填满，
/// 少一格都会留脏字节，多一格就是当初那场越界 panic。
#[cfg(windows)]
fn write_header(header: &mut [u8], width: i32, height: i32, image_size: u32) {
    let mut at = 0usize;
    let mut put = |value: &[u8]| {
        header[at..at + value.len()].copy_from_slice(value);
        at += value.len();
    };
    put(&INFO_HEADER_SIZE.to_le_bytes()); // biSize
    put(&width.to_le_bytes()); // biWidth
    put(&height.to_le_bytes()); // biHeight：正值 = 自底向上
    put(&1u16.to_le_bytes()); // biPlanes
    put(&32u16.to_le_bytes()); // biBitCount
    put(&BI_RGB.to_le_bytes()); // biCompression
    put(&image_size.to_le_bytes()); // biSizeImage
    put(&0i32.to_le_bytes()); // biXPelsPerMeter
    put(&0i32.to_le_bytes()); // biYPelsPerMeter
    put(&0u32.to_le_bytes()); // biClrUsed
    put(&0u32.to_le_bytes()); // biClrImportant
    debug_assert_eq!(at, INFO_HEADER_SIZE as usize, "字段长度算错了");
}

/// RGBA -> BGRA，并按自底向上的行序写进 `body`。
#[cfg(windows)]
fn write_pixels(body: &mut [u8], rgba: &[u8], cols: usize, rows: usize) {
    for (row, line) in body.chunks_exact_mut(cols * 4).enumerate() {
        // 第 `row` 行（自底向上）取原图自顶向下的第 `rows - 1 - row` 行。
        let source = (rows - 1 - row) * cols * 4;
        for (index, pixel) in line.chunks_exact_mut(4).enumerate() {
            let at = source + index * 4;
            pixel[0] = rgba[at + 2];
            pixel[1] = rgba[at + 1];
            pixel[2] = rgba[at];
            pixel[3] = rgba[at + 3];
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    fn u32_at(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
    }

    fn i32_at(bytes: &[u8], at: usize) -> i32 {
        i32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
    }

    /// 头必须正好填满 40 字节，且每个字段落在 `BITMAPINFOHEADER` 规定的偏移上。
    ///
    /// 这条测试是为了当初那场 panic 立的：`biSize` 拿 `usize::to_le_bytes()` 写，
    /// 64 位下多写 4 字节，最后一个字段越到 44 > 40。缓冲区按 40 定长给，写超就会在这里崩。
    #[test]
    fn header_fields_land_on_the_documented_offsets() {
        let mut header = [0xAAu8; INFO_HEADER_SIZE as usize];
        write_header(&mut header, 3, 2, 24);

        assert_eq!(u32_at(&header, 0), 40, "biSize");
        assert_eq!(i32_at(&header, 4), 3, "biWidth");
        assert_eq!(i32_at(&header, 8), 2, "biHeight：正值即自底向上");
        assert_eq!(u16::from_le_bytes(header[12..14].try_into().unwrap()), 1, "biPlanes");
        assert_eq!(u16::from_le_bytes(header[14..16].try_into().unwrap()), 32, "biBitCount");
        assert_eq!(u32_at(&header, 16), BI_RGB, "biCompression");
        assert_eq!(u32_at(&header, 20), 24, "biSizeImage");
        // 剩下的分辨率与调色板字段全是 0：留 0xAA 就说明有字段没写、或写漏了一格。
        assert!(header[24..].iter().all(|&byte| byte == 0), "尾部字段该被清零");
    }

    /// 像素要同时做两件事：RGBA 换成 BGRA、行序倒过来。任何一件做反了，粘出来的图
    /// 要么颜色错，要么上下颠倒。
    #[test]
    fn pixels_are_swapped_to_bgra_and_flipped_upside_down() {
        let rgba = [
            10, 20, 30, 255, 40, 50, 60, 255, // 第一行（自顶向下）
            70, 80, 90, 255, 100, 110, 120, 255, // 第二行
        ];
        let mut body = [0u8; 16];
        write_pixels(&mut body, &rgba, 2, 2);

        assert_eq!(&body[0..8], &[90, 80, 70, 255, 120, 110, 100, 255], "DIB 首行 = 原图末行");
        assert_eq!(&body[8..16], &[30, 20, 10, 255, 60, 50, 40, 255], "DIB 末行 = 原图首行");
    }

    /// 端到端：真把一块 2×2 写进系统剪贴板。
    ///
    /// `#[ignore]`：它会覆盖用户当前的剪贴板，不能混在 `cargo test` 里默认跑。
    /// 怀疑剪贴板通路坏了就手动跑一次（错误信息里带着是哪一步、什么错）：
    /// `cargo test --lib -- --ignored clipboard`
    #[test]
    #[ignore = "会写用户真实的剪贴板"]
    fn a_two_by_two_bitmap_reaches_the_clipboard() {
        let rgba: [u8; 16] = [
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255,
        ];
        set_image(&rgba, 2, 2).expect("剪贴板写入失败");
    }
}

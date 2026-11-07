//! 安装 / 卸载向导。
//!
//! 同一个 exe 既是番茄钟，也是安装程序：
//!
//! ```text
//! LiquidGlass.exe --install     打开安装向导
//! LiquidGlass.exe --uninstall   卸载
//! ```
//!
//! 非 debug 构建从安装目录外启动时也会自动弹出向导；点“取消”后记住
//! “以绿色版运行”，下次不再询问（需要安装时仍可用 `--install`）。
//!
//! 安装内容：复制 exe 到 `%LOCALAPPDATA%\Programs\LiquidGlass\`，按选项创建
//! 桌面 / 开始菜单快捷方式、开机启动项和“应用和功能”里的卸载项。

use std::ffi::c_void;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use winreg::RegKey;
use winreg::enums::HKEY_CURRENT_USER;

use windows::core::{HSTRING, Interface, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    COLOR_WINDOW, DEFAULT_GUI_FONT, GetStockObject, GetSysColorBrush, HDC, HGDIOBJ, InvalidateRect,
    SetBkMode, TRANSPARENT,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
    IPersistFile,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
use windows::Win32::UI::WindowsAndMessaging::{
    BM_GETCHECK, BM_SETCHECK, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GWLP_USERDATA, GetMessageW, GetSystemMetrics, GetWindowLongPtrW, GetWindowTextLengthW,
    GetWindowTextW, HCURSOR, HICON, HMENU, IDC_ARROW, IDYES, LoadCursorW, LoadIconW, MB_ICONERROR,
    MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK, MB_YESNO, MESSAGEBOX_RESULT, MESSAGEBOX_STYLE, MSG,
    MessageBoxW, PostMessageW, PostQuitMessage, RegisterClassW, SM_CXSCREEN, SM_CYSCREEN, SW_SHOW,
    SendMessageW, SetWindowLongPtrW, SetWindowTextW, ShowWindow, TranslateMessage, WM_CLOSE,
    WM_COMMAND, WM_CTLCOLORSTATIC, WM_DESTROY, WM_SETFONT, WNDCLASSW, WNDCLASS_STYLES,
    WINDOW_EX_STYLE, WINDOW_STYLE, WS_CAPTION, WS_MINIMIZEBOX, WS_SYSMENU,
};

/// 安装目录里的固定文件名
const EXE_NAME: &str = "LiquidGlass.exe";
/// 标记这个目录是安装向导创建的（卸载时用来防止误删）
const MARKER: &str = ".liquid-glass-install";
const SHORTCUT_NAME: &str = "液态玻璃番茄钟.lnk";
const POS_FILE: &str = "window-pos.txt";
const LOG_FILE: &str = "liquid-glass-error.log";

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "LiquidGlass";
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\LiquidGlass";
const SHELL_FOLDERS: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\Shell Folders";

const CLASS_NAME: &str = "LiquidGlassInstallerWizard";

const WIZARD_W: i32 = 560;
const WIZARD_H: i32 = 480;
const MARGIN: i32 = 24;
const CONTENT_W: i32 = WIZARD_W - MARGIN * 2;

const ID_NEXT: usize = 2001;
const ID_BACK: usize = 2002;
const ID_CANCEL: usize = 2003;
const ID_INSTALL: usize = 2004;
const ID_FINISH: usize = 2005;
const ID_RETRY: usize = 2006;

const WM_INSTALL_PROGRESS: u32 = 0x8000 + 1;
const WM_INSTALL_DONE: u32 = 0x8000 + 2;

/// 卸载后的清理进程标记（临时目录里的副本身份）
const CLEANUP_FLAG: &str = "--uninstall-cleanup";

// 控件样式用原始数值，避免 ES_* / BS_* / SS_* 在不同模块里的类型差异
const S_CHILD: u32 = 0x4000_0000;
const S_VISIBLE: u32 = 0x1000_0000;
const S_TABSTOP: u32 = 0x0001_0000;
const S_BORDER: u32 = 0x0080_0000;
const S_AUTOHSCROLL: u32 = 0x0080;
const S_AUTOCHECKBOX: u32 = 0x0000_0003;
const S_DEFPUSHBUTTON: u32 = 0x0000_0001;
const S_PUSHBUTTON: u32 = 0x0000_0000;
const S_ETCHEDHORZ: u32 = 0x0000_0010;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum WizardResult {
    /// 安装完成（是否立即运行由完成页的勾选决定）
    Installed,
    /// 用户取消了安装
    Cancelled,
}

/// 命令行入口。返回 true 表示这次进程只做安装 / 卸载，随后退出。
pub fn handle_command_line() -> bool {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(index) = args.iter().position(|a| a == CLEANUP_FLAG) {
        if let Some(dir) = args.get(index + 1) {
            cleanup_install_dir(Path::new(dir));
        }
        return true;
    }
    if args.iter().any(|a| a == "--install") {
        let _ = run_wizard(false);
        return true;
    }
    if args.iter().any(|a| a == "--uninstall") {
        uninstall();
        return true;
    }
    false
}

/// 首次从安装目录外启动时是否自动打开向导。
pub fn should_offer_first_run() -> bool {
    // 开发时 cargo run 不弹向导；--portable 和调试参数也不弹
    if cfg!(debug_assertions) {
        return false;
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| {
        matches!(
            a.as_str(),
            "--portable"
                | "--offscreen"
                | "--dump-frame"
                | "--glass-only"
                | "--time"
                | "--phase"
                | "--sim-drag"
                | "--no-edge-hide"
                | "--no-exclude"
        )
    }) {
        return false;
    }
    if marker_in_exe_dir() {
        return false;
    }
    !skip_flag().map(|p| p.exists()).unwrap_or(false)
}

// ------------------------------------------------------------------ 数据 --

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Welcome,
    Location,
    Options,
    Progress,
    Finish,
    Failed,
}

struct WizardState {
    hwnd: HWND,
    instance: HINSTANCE,
    font: HGDIOBJ,
    scale: f32,
    page: Page,
    from_first_run: bool,
    controls: Vec<HWND>,
    edit_dir: HWND,
    check_desktop: HWND,
    check_start: HWND,
    check_autostart: HWND,
    check_launch: HWND,
    status: HWND,
    install_dir: PathBuf,
    desktop: bool,
    start_menu: bool,
    autostart: bool,
    installing: bool,
    error: String,
    log: Arc<Mutex<Vec<String>>>,
    installed: Arc<AtomicBool>,
}

#[derive(Clone)]
struct InstallOptions {
    exe: PathBuf,
    dir: PathBuf,
    desktop: bool,
    start_menu: bool,
    autostart: bool,
}

// ------------------------------------------------------------- 窗口过程 --

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as usize;
            if let Some(st) = state_mut(hwnd) {
                handle_command(hwnd, st, id);
            }
            LRESULT(0)
        }
        WM_INSTALL_PROGRESS => {
            if let Some(st) = state_mut(hwnd) {
                update_log_text(st);
            }
            LRESULT(0)
        }
        WM_INSTALL_DONE => {
            if let Some(st) = state_mut(hwnd) {
                let ok = wparam.0 != 0;
                st.installing = false;
                if ok {
                    st.installed.store(true, Ordering::SeqCst);
                    remove_skip_flag();
                    st.page = Page::Finish;
                } else {
                    st.error = st
                        .log
                        .lock()
                        .ok()
                        .and_then(|log| log.last().cloned())
                        .unwrap_or_else(|| "未知错误".to_owned());
                    st.page = Page::Failed;
                }
                show_page(hwnd, st);
            }
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC => {
            // 让静态文字背景透明，跟窗口底色一致
            unsafe {
                SetBkMode(HDC(lparam.0 as *mut c_void), TRANSPARENT);
                LRESULT(GetSysColorBrush(COLOR_WINDOW).0 as isize)
            }
        }
        WM_CLOSE => {
            if let Some(st) = state_mut(hwnd) {
                if st.installing {
                    return LRESULT(0); // 安装中不允许关窗
                }
                if st.from_first_run && st.page != Page::Failed {
                    write_skip_flag();
                }
            }
            let _ = unsafe { DestroyWindow(hwnd) };
            LRESULT(0)
        }
        WM_DESTROY => {
            let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
            if pointer != 0 {
                unsafe {
                    drop(Box::from_raw(pointer as *mut WizardState));
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                }
            }
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn state_mut(hwnd: HWND) -> Option<&'static mut WizardState> {
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
    if pointer == 0 {
        None
    } else {
        Some(unsafe { &mut *(pointer as *mut WizardState) })
    }
}

// --------------------------------------------------------------- 页面 --

impl WizardState {
    fn add_control(
        &mut self,
        class: &str,
        text: &str,
        style: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        id: usize,
    ) -> HWND {
        let class = wide(class);
        let text = wide(text);
        let menu = if id == 0 {
            None
        } else {
            Some(HMENU(id as isize as *mut c_void))
        };
        let created = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                PCWSTR(class.as_ptr()),
                PCWSTR(text.as_ptr()),
                WINDOW_STYLE(style),
                x,
                y,
                width,
                height,
                Some(self.hwnd),
                menu,
                Some(self.instance),
                None,
            )
        };
        let hwnd = created.unwrap_or(HWND(std::ptr::null_mut()));
        if !hwnd.0.is_null() {
            unsafe {
                SendMessageW(
                    hwnd,
                    WM_SETFONT,
                    Some(WPARAM(self.font.0 as usize)),
                    Some(LPARAM(1)),
                );
            }
            self.controls.push(hwnd);
        }
        hwnd
    }

    fn add_label(&mut self, text: &str, x: i32, y: i32, width: i32, height: i32) -> HWND {
        self.add_control(
            "STATIC",
            text,
            S_CHILD | S_VISIBLE,
            x,
            y,
            width,
            height,
            0,
        )
    }

    fn add_rule(&mut self, x: i32, y: i32, width: i32) -> HWND {
        let height = scaled(2.0, self.scale).max(1);
        self.add_control(
            "STATIC",
            "",
            S_CHILD | S_VISIBLE | S_ETCHEDHORZ,
            x,
            y,
            width,
            height,
            0,
        )
    }

    fn add_edit(&mut self, text: &str, x: i32, y: i32, width: i32, height: i32) -> HWND {
        self.add_control(
            "EDIT",
            text,
            S_CHILD | S_VISIBLE | S_TABSTOP | S_BORDER | S_AUTOHSCROLL,
            x,
            y,
            width,
            height,
            0,
        )
    }

    fn add_check(
        &mut self,
        text: &str,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        checked: bool,
    ) -> HWND {
        let hwnd = self.add_control(
            "BUTTON",
            text,
            S_CHILD | S_VISIBLE | S_TABSTOP | S_AUTOCHECKBOX,
            x,
            y,
            width,
            height,
            0,
        );
        if !hwnd.0.is_null() {
            unsafe {
                SendMessageW(
                    hwnd,
                    BM_SETCHECK,
                    Some(WPARAM(if checked { 1 } else { 0 })),
                    None,
                );
            }
        }
        hwnd
    }

    fn add_button(
        &mut self,
        text: &str,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        id: usize,
        default: bool,
    ) -> HWND {
        let style = S_CHILD
            | S_VISIBLE
            | S_TABSTOP
            | if default {
                S_DEFPUSHBUTTON
            } else {
                S_PUSHBUTTON
            };
        self.add_control("BUTTON", text, style, x, y, width, height, id)
    }
}

fn show_page(hwnd: HWND, st: &mut WizardState) {
    for control in st.controls.drain(..) {
        unsafe {
            let _ = DestroyWindow(control);
        }
    }
    st.edit_dir = HWND(std::ptr::null_mut());
    st.check_desktop = HWND(std::ptr::null_mut());
    st.check_start = HWND(std::ptr::null_mut());
    st.check_autostart = HWND(std::ptr::null_mut());
    st.check_launch = HWND(std::ptr::null_mut());
    st.status = HWND(std::ptr::null_mut());

    let scale = st.scale;
    let m = scaled(MARGIN as f32, scale);
    let cw = scaled(CONTENT_W as f32, scale);

    // 顶部标题 + 分隔线
    st.add_label(
        "液态玻璃番茄钟 · 安装向导",
        m,
        scaled(16.0, scale),
        cw,
        scaled(26.0, scale),
    );
    st.add_rule(m, scaled(50.0, scale), cw);

    match st.page {
        Page::Welcome => {
            st.add_label(
                "欢迎使用液态玻璃番茄钟",
                m,
                scaled(72.0, scale),
                cw,
                scaled(26.0, scale),
            );
            st.add_label(
                "这是一个跑在 Windows 上的番茄钟，外壳是折射真实桌面的液态玻璃。\n\
                 安装向导会把程序装进当前用户目录，并创建快捷方式，全程不需要管理员权限。",
                m,
                scaled(112.0, scale),
                cw,
                scaled(66.0, scale),
            );
            st.add_label(
                &format!("版本 {} · GPL-3.0-or-later", env!("CARGO_PKG_VERSION")),
                m,
                scaled(206.0, scale),
                cw,
                scaled(20.0, scale),
            );
            st.add_label(
                "点击“下一步”继续；点击“取消”直接以绿色版运行。",
                m,
                scaled(240.0, scale),
                cw,
                scaled(20.0, scale),
            );
        }
        Page::Location => {
            st.add_label(
                "选择安装位置",
                m,
                scaled(72.0, scale),
                cw,
                scaled(24.0, scale),
            );
            let path = st.install_dir.display().to_string();
            st.edit_dir = st.add_edit(&path, m, scaled(110.0, scale), cw, scaled(26.0, scale));
            st.add_label(
                "建议保留默认路径（当前用户目录，无需管理员权限）。",
                m,
                scaled(148.0, scale),
                cw,
                scaled(20.0, scale),
            );
            st.add_label(
                "如果目录已存在，会覆盖同名程序文件；窗口位置等数据会保留。",
                m,
                scaled(174.0, scale),
                cw,
                scaled(20.0, scale),
            );
        }
        Page::Options => {
            st.add_label(
                "安装选项",
                m,
                scaled(72.0, scale),
                cw,
                scaled(24.0, scale),
            );
            st.check_desktop = st.add_check(
                "创建桌面快捷方式",
                m,
                scaled(112.0, scale),
                cw,
                scaled(24.0, scale),
                st.desktop,
            );
            st.check_start = st.add_check(
                "添加到开始菜单",
                m,
                scaled(144.0, scale),
                cw,
                scaled(24.0, scale),
                st.start_menu,
            );
            st.check_autostart = st.add_check(
                "开机自动启动液态玻璃番茄钟",
                m,
                scaled(176.0, scale),
                cw,
                scaled(24.0, scale),
                st.autostart,
            );
            st.add_label(
                "开机启动会写入当前用户的启动项，随时可以在任务管理器里关掉。",
                m,
                scaled(216.0, scale),
                cw,
                scaled(40.0, scale),
            );
        }
        Page::Progress => {
            st.add_label(
                "正在安装，请稍候……",
                m,
                scaled(72.0, scale),
                cw,
                scaled(24.0, scale),
            );
            st.status = st.add_label(
                "",
                m,
                scaled(106.0, scale),
                cw,
                scaled(280.0, scale),
            );
            update_log_text(st);
        }
        Page::Finish => {
            st.add_label(
                "安装完成",
                m,
                scaled(72.0, scale),
                cw,
                scaled(24.0, scale),
            );
            st.add_label(
                &format!("程序已安装到：{}", st.install_dir.display()),
                m,
                scaled(108.0, scale),
                cw,
                scaled(40.0, scale),
            );
            st.add_label(
                "快捷键：空格 开始/暂停 · R 重置 · S 跳过 · T 点击穿透 · ESC 退出",
                m,
                scaled(160.0, scale),
                cw,
                scaled(40.0, scale),
            );
            st.check_launch = st.add_check(
                "立即运行液态玻璃番茄钟",
                m,
                scaled(220.0, scale),
                cw,
                scaled(24.0, scale),
                true,
            );
            st.add_label(
                "以后可以从桌面、开始菜单或安装目录启动。",
                m,
                scaled(252.0, scale),
                cw,
                scaled(20.0, scale),
            );
        }
        Page::Failed => {
            st.add_label(
                "安装没有完成",
                m,
                scaled(72.0, scale),
                cw,
                scaled(24.0, scale),
            );
            let error = st.error.clone();
            st.add_label(&error, m, scaled(108.0, scale), cw, scaled(180.0, scale));
            st.add_label(
                "可以返回上一步修改安装位置或选项后重试。",
                m,
                scaled(300.0, scale),
                cw,
                scaled(20.0, scale),
            );
        }
    }

    // 底部按钮（右下角对齐）
    let by = scaled(408.0, scale);
    let bh = scaled(30.0, scale);
    let bw = scaled(88.0, scale);
    let gap = scaled(8.0, scale);
    let right = scaled((WIZARD_W - MARGIN) as f32, scale);
    let third = right - bw;
    let second = third - gap - bw;
    let first = second - gap - bw;
    match st.page {
        Page::Welcome => {
            st.add_button("下一步", second, by, bw, bh, ID_NEXT, true);
            st.add_button("取消", third, by, bw, bh, ID_CANCEL, false);
        }
        Page::Location => {
            st.add_button("上一步", first, by, bw, bh, ID_BACK, false);
            st.add_button("下一步", second, by, bw, bh, ID_NEXT, true);
            st.add_button("取消", third, by, bw, bh, ID_CANCEL, false);
        }
        Page::Options => {
            st.add_button("上一步", first, by, bw, bh, ID_BACK, false);
            st.add_button("安装", second, by, bw, bh, ID_INSTALL, true);
            st.add_button("取消", third, by, bw, bh, ID_CANCEL, false);
        }
        Page::Progress => {}
        Page::Finish => {
            st.add_button("完成", third, by, bw, bh, ID_FINISH, true);
        }
        Page::Failed => {
            st.add_button("重试", second, by, bw, bh, ID_RETRY, false);
            st.add_button("关闭", third, by, bw, bh, ID_CANCEL, true);
        }
    }

    st.add_label(
        &format!("v{} · GPL-3.0-or-later", env!("CARGO_PKG_VERSION")),
        m,
        scaled(414.0, scale),
        scaled(260.0, scale),
        scaled(18.0, scale),
    );

    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, true);
    }
}

fn handle_command(hwnd: HWND, st: &mut WizardState, id: usize) {
    match id {
        ID_NEXT => match st.page {
            Page::Welcome => {
                st.page = Page::Location;
                show_page(hwnd, st);
            }
            Page::Location => match validate_install_dir(&get_text(st.edit_dir)) {
                Ok(dir) => {
                    st.install_dir = dir;
                    st.page = Page::Options;
                    show_page(hwnd, st);
                }
                Err(message) => message_box(hwnd, &message, MB_OK | MB_ICONERROR),
            },
            _ => {}
        },
        ID_BACK => {
            st.page = if st.page == Page::Options {
                Page::Location
            } else {
                Page::Welcome
            };
            show_page(hwnd, st);
        }
        ID_INSTALL => start_install(hwnd, st),
        ID_CANCEL => {
            if !st.installing {
                if st.from_first_run && st.page != Page::Failed {
                    write_skip_flag();
                }
                let _ = unsafe { DestroyWindow(hwnd) };
            }
        }
        ID_FINISH => {
            if checked(st.check_launch) {
                let exe = st.install_dir.join(EXE_NAME);
                let _ = std::process::Command::new(exe).spawn();
            }
            let _ = unsafe { DestroyWindow(hwnd) };
        }
        ID_RETRY => {
            st.page = Page::Options;
            show_page(hwnd, st);
        }
        _ => {}
    }
}

fn update_log_text(st: &WizardState) {
    if st.status.0.is_null() {
        return;
    }
    let text = match st.log.lock() {
        Ok(log) => log
            .iter()
            .rev()
            .take(10)
            .rev()
            .cloned()
            .collect::<Vec<_>>()
            .join("\r\n"),
        Err(_) => String::new(),
    };
    let text = wide(&text);
    unsafe {
        let _ = SetWindowTextW(st.status, PCWSTR(text.as_ptr()));
    }
}

fn start_install(hwnd: HWND, st: &mut WizardState) {
    if st.installing {
        return;
    }
    st.desktop = checked(st.check_desktop);
    st.start_menu = checked(st.check_start);
    st.autostart = checked(st.check_autostart);
    if let Ok(mut log) = st.log.lock() {
        log.clear();
    }
    st.error.clear();
    st.installing = true;
    st.page = Page::Progress;
    show_page(hwnd, st);
    update_log_text(st);

    let options = InstallOptions {
        exe: std::env::current_exe().unwrap_or_default(),
        dir: st.install_dir.clone(),
        desktop: st.desktop,
        start_menu: st.start_menu,
        autostart: st.autostart,
    };
    let log = st.log.clone();
    let hwnd_raw = hwnd.0 as isize;
    std::thread::spawn(move || run_install(hwnd_raw, log, options));
}

// ------------------------------------------------------------- 安装逻辑 --

fn run_install(hwnd_raw: isize, log: Arc<Mutex<Vec<String>>>, options: InstallOptions) {
    // 快捷方式需要 COM；在这个工作线程里初始化（主线程是消息循环）
    let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    let hwnd = HWND(hwnd_raw as *mut c_void);

    let mut report = |line: String| {
        if let Ok(mut log) = log.lock() {
            log.push(line);
        }
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_INSTALL_PROGRESS, WPARAM(0), LPARAM(0));
        }
    };

    let result = perform_install(&options, &mut report);

    match &result {
        Ok(()) => report("安装完成。".to_owned()),
        Err(error) => report(format!("安装失败：{error}")),
    }
    unsafe {
        let _ = PostMessageW(
            Some(hwnd),
            WM_INSTALL_DONE,
            WPARAM(if result.is_ok() { 1 } else { 0 }),
            LPARAM(0),
        );
    }
    if hr.0 >= 0 {
        unsafe { CoUninitialize() };
    }
}

fn perform_install(
    options: &InstallOptions,
    report: &mut dyn FnMut(String),
) -> Result<(), String> {
    if options.exe.as_os_str().is_empty() {
        return Err("拿不到当前程序路径".to_owned());
    }

    report("创建安装目录…".to_owned());
    fs::create_dir_all(&options.dir).map_err(|e| format!("创建目录失败：{e}"))?;

    report("复制程序文件…".to_owned());
    let dest = options.dir.join(EXE_NAME);
    if !same_path(&options.exe, &dest) {
        fs::copy(&options.exe, &dest)
            .map_err(|e| format!("复制程序失败：{e}（如果程序正在运行，请先退出）"))?;
    }
    fs::write(
        options.dir.join(MARKER),
        format!("version={}\n", env!("CARGO_PKG_VERSION")),
    )
    .map_err(|e| format!("写入安装标记失败：{e}"))?;

    report("创建快捷方式…".to_owned());
    if options.desktop {
        match desktop_dir() {
            Some(dir) => {
                if let Err(error) = create_shortcut(&dir.join(SHORTCUT_NAME), &dest, &options.dir) {
                    report(format!("桌面快捷方式创建失败：{error}"));
                }
            }
            None => report("找不到桌面目录，跳过桌面快捷方式。".to_owned()),
        }
    } else if let Some(dir) = desktop_dir() {
        let _ = fs::remove_file(dir.join(SHORTCUT_NAME));
    }

    if options.start_menu {
        match start_menu_dir() {
            Some(dir) => {
                if let Err(error) = create_shortcut(&dir.join(SHORTCUT_NAME), &dest, &options.dir) {
                    report(format!("开始菜单快捷方式创建失败：{error}"));
                }
            }
            None => report("找不到开始菜单目录，跳过快捷方式。".to_owned()),
        }
    } else if let Some(dir) = start_menu_dir() {
        let _ = fs::remove_file(dir.join(SHORTCUT_NAME));
    }

    report("写入启动项和卸载信息…".to_owned());
    if options.autostart {
        if let Err(error) = set_autostart(&dest) {
            report(format!("开机启动写入失败：{error}"));
        }
    } else {
        remove_autostart();
    }
    if let Err(error) = write_uninstall_entry(&dest, &options.dir) {
        report(format!("卸载信息写入失败：{error}"));
    }

    Ok(())
}

fn create_shortcut(lnk: &Path, target: &Path, workdir: &Path) -> Result<(), String> {
    let target_text = target.to_string_lossy().to_string();
    let workdir_text = workdir.to_string_lossy().to_string();
    let lnk_text = lnk.to_string_lossy().to_string();
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| format!("创建 IShellLink 失败：{e}"))?;
        let target_h = HSTRING::from(target_text.as_str());
        let workdir_h = HSTRING::from(workdir_text.as_str());
        let desc_h = HSTRING::from("液态玻璃番茄钟");
        link.SetPath(PCWSTR(target_h.as_ptr()))
            .map_err(|e| format!("设置快捷方式目标失败：{e}"))?;
        link.SetWorkingDirectory(PCWSTR(workdir_h.as_ptr()))
            .map_err(|e| format!("设置工作目录失败：{e}"))?;
        link.SetDescription(PCWSTR(desc_h.as_ptr()))
            .map_err(|e| format!("设置描述失败：{e}"))?;
        link.SetIconLocation(PCWSTR(target_h.as_ptr()), 0)
            .map_err(|e| format!("设置图标失败：{e}"))?;
        let file: IPersistFile = link
            .cast()
            .map_err(|e| format!("获取 IPersistFile 失败：{e}"))?;
        let lnk_h = HSTRING::from(lnk_text.as_str());
        file.Save(PCWSTR(lnk_h.as_ptr()), true)
            .map_err(|e| format!("写入快捷方式失败：{e}"))?;
    }
    Ok(())
}

fn set_autostart(exe: &Path) -> Result<(), String> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = hkcu
        .create_subkey(RUN_KEY)
        .map_err(|e| format!("打开启动项失败：{e}"))?;
    let value = format!("\"{}\"", exe.display());
    key.set_value(RUN_VALUE, &value)
        .map_err(|e| format!("写入启动项失败：{e}"))
}

fn remove_autostart() {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    if let Ok(key) = hkcu.open_subkey(RUN_KEY) {
        let _ = key.delete_value(RUN_VALUE);
    }
}

fn write_uninstall_entry(exe: &Path, dir: &Path) -> Result<(), String> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = hkcu
        .create_subkey(UNINSTALL_KEY)
        .map_err(|e| format!("创建卸载信息失败：{e}"))?;
    let exe_text = exe.to_string_lossy().to_string();
    let dir_text = dir.to_string_lossy().to_string();

    let put = |name: &str, value: &str| -> Result<(), String> {
        key.set_value(name, &value.to_owned())
            .map_err(|e| format!("写入卸载信息 {name} 失败：{e}"))
    };
    put("DisplayName", "液态玻璃番茄钟")?;
    put("DisplayIcon", &format!("{exe_text},0"))?;
    put("DisplayVersion", env!("CARGO_PKG_VERSION"))?;
    put("Publisher", "XiangFu")?;
    put("InstallLocation", &dir_text)?;
    put("UninstallString", &format!("\"{exe_text}\" --uninstall"))?;
    key.set_value("NoModify", &1u32)
        .map_err(|e| format!("写入卸载信息 NoModify 失败：{e}"))?;
    key.set_value("NoRepair", &1u32)
        .map_err(|e| format!("写入卸载信息 NoRepair 失败：{e}"))?;
    Ok(())
}

// ------------------------------------------------------------- 卸载逻辑 --

fn uninstall() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Some(dir) = exe.parent().map(Path::to_path_buf) else {
        return;
    };
    if !dir.join(MARKER).exists() {
        message_box(
            HWND(std::ptr::null_mut()),
            "没有找到安装标记。\n\n当前程序不像是通过安装向导安装的，未做任何修改。",
            MB_OK | MB_ICONINFORMATION,
        );
        return;
    }
    let question = format!(
        "确定要卸载液态玻璃番茄钟吗？\n\n安装位置：{}\n\n程序文件、快捷方式和启动项都会被删除。",
        dir.display()
    );
    if message_yes_no(&question).0 != IDYES.0 {
        return;
    }

    remove_autostart();
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let _ = hkcu.delete_subkey_all(UNINSTALL_KEY);

    if let Some(desktop) = desktop_dir() {
        let _ = fs::remove_file(desktop.join(SHORTCUT_NAME));
    }
    if let Some(menu) = start_menu_dir() {
        let _ = fs::remove_file(menu.join(SHORTCUT_NAME));
    }

    // 正在运行的 exe 删不掉、目录也收不掉，所以复制一份到临时目录，
    // 等本进程退出后由副本清理（不用 cmd，避免引号转义问题）。
    let temp = std::env::temp_dir().join(format!(
        "LiquidGlass-uninstall-{}.exe",
        std::process::id()
    ));
    match fs::copy(&exe, &temp) {
        Ok(_) => {
            message_box(
                HWND(std::ptr::null_mut()),
                "卸载完成。\n\n程序目录会在几秒后自动删除。",
                MB_OK | MB_ICONINFORMATION,
            );
            let _ = std::process::Command::new(&temp)
                .arg(CLEANUP_FLAG)
                .arg(&dir)
                .spawn();
        }
        Err(error) => {
            message_box(
                HWND(std::ptr::null_mut()),
                &format!(
                    "卸载完成，但自动清理没能启动：{error}\n\n请手动删除这个目录：\n{}",
                    dir.display()
                ),
                MB_OK | MB_ICONINFORMATION,
            );
        }
    }
}

/// 清理进程：等原程序退出后删掉安装目录里的文件（只删我们自己的文件，
/// 目录里如果有用户放的其他东西，就不删目录本身）。
fn cleanup_install_dir(dir: &Path) {
    // 没有安装标记就不动，防止参数被误用
    if !dir.join(MARKER).exists() {
        return;
    }
    std::thread::sleep(std::time::Duration::from_millis(1200));

    // 等原进程完全退出；删不掉就保留现场，下次还能再卸载
    let exe = dir.join(EXE_NAME);
    let mut removed = false;
    for _ in 0..10 {
        if !exe.exists() || fs::remove_file(&exe).is_ok() {
            removed = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    if !removed {
        return;
    }

    let _ = fs::remove_file(dir.join(POS_FILE));
    let _ = fs::remove_file(dir.join(LOG_FILE));
    let _ = fs::remove_file(dir.join(MARKER));
    let _ = fs::remove_dir(dir);
}

// --------------------------------------------------------------- 工具 --

pub(crate) fn run_wizard(from_first_run: bool) -> WizardResult {
    let class_name = wide(CLASS_NAME);
    let instance = match unsafe { GetModuleHandleW(None) } {
        Ok(module) => HINSTANCE(module.0),
        Err(_) => HINSTANCE(std::ptr::null_mut()),
    };
    let wc = WNDCLASSW {
        style: WNDCLASS_STYLES(0x0001 | 0x0002),
        lpfnWndProc: Some(wndproc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: instance,
        hIcon: unsafe { LoadIconW(Some(instance), PCWSTR(1 as *const u16)) }
            .unwrap_or(HICON(std::ptr::null_mut())),
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }
            .unwrap_or(HCURSOR(std::ptr::null_mut())),
        hbrBackground: unsafe { GetSysColorBrush(COLOR_WINDOW) },
        lpszMenuName: PCWSTR::null(),
        lpszClassName: PCWSTR(class_name.as_ptr()),
    };
    unsafe {
        RegisterClassW(&wc);
    }

    let scale = dpi_scale();
    let width = scaled(WIZARD_W as f32, scale);
    let height = scaled(WIZARD_H as f32, scale);
    let x = (unsafe { GetSystemMetrics(SM_CXSCREEN) } - width) / 2;
    let y = (unsafe { GetSystemMetrics(SM_CYSCREEN) } - height) / 2;
    let title = wide("液态玻璃番茄钟 安装向导");
    let created = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class_name.as_ptr()),
            PCWSTR(title.as_ptr()),
            WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
            x.max(0),
            y.max(0),
            width,
            height,
            None,
            None,
            Some(instance),
            None,
        )
    };
    let hwnd = match created {
        Ok(hwnd) => hwnd,
        Err(_) => return WizardResult::Cancelled,
    };

    let installed = Arc::new(AtomicBool::new(false));
    let font = unsafe { GetStockObject(DEFAULT_GUI_FONT) };
    let state = Box::new(WizardState {
        hwnd,
        instance,
        font,
        scale,
        page: Page::Welcome,
        from_first_run,
        controls: Vec::new(),
        edit_dir: HWND(std::ptr::null_mut()),
        check_desktop: HWND(std::ptr::null_mut()),
        check_start: HWND(std::ptr::null_mut()),
        check_autostart: HWND(std::ptr::null_mut()),
        check_launch: HWND(std::ptr::null_mut()),
        status: HWND(std::ptr::null_mut()),
        install_dir: default_install_dir(),
        desktop: true,
        start_menu: true,
        autostart: false,
        installing: false,
        error: String::new(),
        log: Arc::new(Mutex::new(Vec::new())),
        installed: installed.clone(),
    });
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(state) as isize);
    }
    if let Some(st) = state_mut(hwnd) {
        show_page(hwnd, st);
    }
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
    }

    let mut msg: MSG = unsafe { std::mem::zeroed() };
    loop {
        let result = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if result.0 <= 0 {
            break;
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    if installed.load(Ordering::SeqCst) {
        WizardResult::Installed
    } else {
        WizardResult::Cancelled
    }
}

fn validate_install_dir(text: &str) -> Result<PathBuf, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("安装位置不能为空。".to_owned());
    }
    let path = PathBuf::from(trimmed);
    if !path.is_absolute() {
        return Err("请输入完整的绝对路径，例如 C:\\Users\\你\\AppData\\Local\\Programs\\LiquidGlass".to_owned());
    }
    if path.parent().is_none() {
        return Err("不能直接安装到磁盘根目录。".to_owned());
    }
    Ok(path)
}

fn default_install_dir() -> PathBuf {
    if let Some(base) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(base)
            .join("Programs")
            .join("LiquidGlass");
    }
    if let Some(home) = std::env::var_os("USERPROFILE") {
        return PathBuf::from(home)
            .join("AppData")
            .join("Local")
            .join("Programs")
            .join("LiquidGlass");
    }
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn marker_in_exe_dir() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|dir| dir.join(MARKER).exists()))
        .unwrap_or(false)
}

fn skip_flag() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(|base| PathBuf::from(base).join("LiquidGlass").join("wizard-skipped"))
}

fn write_skip_flag() {
    if let Some(path) = skip_flag() {
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let _ = fs::write(path, "portable");
    }
}

fn remove_skip_flag() {
    if let Some(path) = skip_flag() {
        let _ = fs::remove_file(path);
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    // 尽量用规范化路径比较（用户手输的路径可能带正反斜杠差异）；
    // dest 还不存在时 canonicalize 会失败，退回字符串比较。
    let a = a.canonicalize().unwrap_or_else(|_| a.to_path_buf());
    let b = b.canonicalize().unwrap_or_else(|_| b.to_path_buf());
    a.to_string_lossy()
        .eq_ignore_ascii_case(&b.to_string_lossy())
}

fn desktop_dir() -> Option<PathBuf> {
    shell_folder("Desktop").or_else(|| {
        std::env::var_os("USERPROFILE").map(|profile| PathBuf::from(profile).join("Desktop"))
    })
}

fn start_menu_dir() -> Option<PathBuf> {
    shell_folder("Programs").or_else(|| {
        std::env::var_os("APPDATA").map(|appdata| {
            PathBuf::from(appdata)
                .join("Microsoft")
                .join("Windows")
                .join("Start Menu")
                .join("Programs")
        })
    })
}

fn shell_folder(name: &str) -> Option<PathBuf> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let key = hkcu.open_subkey(SHELL_FOLDERS).ok()?;
    let value: String = key.get_value(name).ok()?;
    if value.is_empty() {
        None
    } else {
        Some(PathBuf::from(value))
    }
}

fn message_box(hwnd: HWND, text: &str, style: MESSAGEBOX_STYLE) {
    let text = wide(text);
    let caption = wide("液态玻璃番茄钟");
    unsafe {
        MessageBoxW(
            Some(hwnd),
            PCWSTR(text.as_ptr()),
            PCWSTR(caption.as_ptr()),
            style,
        );
    }
}

fn message_yes_no(text: &str) -> MESSAGEBOX_RESULT {
    let text = wide(text);
    let caption = wide("卸载液态玻璃番茄钟");
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(text.as_ptr()),
            PCWSTR(caption.as_ptr()),
            MB_YESNO | MB_ICONQUESTION,
        )
    }
}

fn checked(hwnd: HWND) -> bool {
    if hwnd.0.is_null() {
        return false;
    }
    unsafe { SendMessageW(hwnd, BM_GETCHECK, None, None).0 == 1 }
}

fn get_text(hwnd: HWND) -> String {
    if hwnd.0.is_null() {
        return String::new();
    }
    let length = unsafe { GetWindowTextLengthW(hwnd) };
    if length <= 0 {
        return String::new();
    }
    let mut buffer = vec![0u16; length as usize + 1];
    let copied = unsafe { GetWindowTextW(hwnd, &mut buffer) };
    String::from_utf16_lossy(&buffer[..copied.max(0) as usize])
        .trim()
        .to_owned()
}

fn dpi_scale() -> f32 {
    let dpi = unsafe { GetDpiForSystem() };
    (dpi as f32 / 96.0).clamp(1.0, 2.0)
}

fn scaled(value: f32, scale: f32) -> i32 {
    (value * scale).round() as i32
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

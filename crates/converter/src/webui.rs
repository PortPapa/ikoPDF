//! HTML로 그린 화면을 보여 주는 창 (Windows에 들어 있는 WebView2 사용).
//!
//! 화면(`ui/*.html`)은 `window.ipc.postMessage(JSON)` 으로 프로그램에 말을 걸고,
//! 프로그램은 `window.app.함수(값)` 을 불러 화면을 바꾼다.
//! WebView2를 쓸 수 없는 PC에서는 [`run`] 이 `Err(Unavailable)` 을 돌려주며,
//! 그때는 기존 Win32 창(`settings_dialog`, `order_dialog`)을 대신 띄운다.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::ffi::c_void;
use std::num::NonZeroIsize;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{channel, Receiver, Sender};

use serde_json::Value;
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};

use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;
use wry::raw_window_handle::{
    HandleError, HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle,
};
use wry::{WebContext, WebView, WebViewBuilder, WebViewBuilderExtWindows};

use crate::win;

/// 두 화면이 함께 쓰는 모양.
const STYLE: &str = include_str!("../ui/style.css");
const STYLE_LINK: &str = r#"<link rel="stylesheet" href="style.css">"#;
/// 작업 스레드가 결과를 넘겼음을 알리는 메시지.
const WM_WORK_DONE: u32 = WM_APP + 1;
/// 화면 바탕색 (style.css 의 --bg 와 같게). 화면이 뜨기 전 잠깐 보이는 색이다.
const BACKGROUND: (u8, u8, u8) = (0xf4, 0xf0, 0xe8);
/// 이코 캐릭터 그림. HTML 안의 `src="iko.png"` 자리에 넣는다.
const MASCOT: &[u8] = include_bytes!("../ui/iko.png");
const MASCOT_SRC: &str = r#"src="iko.png""#;

/// 이 PC에서는 WebView2 화면을 쓸 수 없다(기존 창으로 대신할 것).
#[derive(Debug)]
pub struct Unavailable;

pub struct Options<'a> {
    pub class_name: PCWSTR,
    pub title: &'a str,
    /// 안쪽 크기와 최소 크기 (96 DPI 기준).
    pub size: (i32, i32),
    pub min_size: (i32, i32),
    pub resizable: bool,
    /// `ui/` 폴더의 HTML 내용.
    pub html: &'static str,
}

/// 화면과 이야기하는 창구. 메시지 처리 함수에 넘겨진다.
pub struct Host {
    window: HWND,
    webview: WebView,
    results: Sender<String>,
}

impl Host {
    /// 화면의 `window.app.<function>(argument)` 을 부른다.
    pub fn call(&self, function: &str, argument: &Value) {
        self.eval(&call_script(function, &[argument]));
    }

    fn eval(&self, script: &str) {
        let _ = self.webview.evaluate_script(script);
    }

    pub fn window(&self) -> HWND {
        self.window
    }

    pub fn close(&self) {
        unsafe {
            let _ = DestroyWindow(self.window);
        }
    }

    /// 오래 걸리는 일을 다른 스레드에서 하고, 돌려준 스크립트를 화면에서 실행한다.
    pub fn spawn(&self, work: impl FnOnce() -> String + Send + 'static) {
        let results = self.results.clone();
        let window = self.window.0 as isize;
        std::thread::spawn(move || {
            let script = work();
            if results.send(script).is_ok() {
                unsafe {
                    let _ = PostMessageW(Some(HWND(window as *mut c_void)), WM_WORK_DONE, WPARAM(0), LPARAM(0));
                }
            }
        });
    }
}

/// `window.app.<function>(인자들);` 스크립트를 만든다.
pub fn call_script(function: &str, arguments: &[&Value]) -> String {
    let arguments: Vec<String> = arguments.iter().map(|value| value.to_string()).collect();
    format!("window.app.{function}({});", arguments.join(","))
}

struct RawWindow(HWND);

impl HasWindowHandle for RawWindow {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let handle = NonZeroIsize::new(self.0 .0 as isize).ok_or(HandleError::Unavailable)?;
        let raw = RawWindowHandle::Win32(Win32WindowHandle::new(handle));
        Ok(unsafe { WindowHandle::borrow_raw(raw) })
    }
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut text = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16
            | (*chunk.get(1).unwrap_or(&0) as u32) << 8
            | *chunk.get(2).unwrap_or(&0) as u32;
        text.push(TABLE[(n >> 18) as usize & 63] as char);
        text.push(TABLE[(n >> 12) as usize & 63] as char);
        text.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
        text.push(if chunk.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
    }
    text
}

/// 화면 파일 하나에 스타일과 그림을 모두 넣어, 따로 읽어 올 파일이 없게 만든다.
fn assemble(html: &str) -> String {
    html.replace(STYLE_LINK, &format!("<style>{STYLE}</style>")).replace(
        MASCOT_SRC,
        &format!(r#"src="data:image/png;base64,{}""#, base64(MASCOT)),
    )
}
/// WebView2가 쓰는 캐시 폴더.
fn web_data_dir() -> PathBuf {
    crate::setup::data_dir().join("WebView")
}

/// 창마다 붙여 두는 값 (GWLP_USERDATA).
struct WindowData {
    min_width: i32,
    min_height: i32,
    /// 창이 닫힐 때 메시지 루프를 끝낼지. 화면을 만들지 못해 창을 지울 때는 끝내지 않는다.
    quit_on_destroy: bool,
}

unsafe extern "system" fn window_proc(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let data = (GetWindowLongPtrW(window, GWLP_USERDATA) as *const WindowData).as_ref();
    match message {
        WM_GETMINMAXINFO => {
            if let Some(data) = data {
                let info = &mut *(lparam.0 as *mut MINMAXINFO);
                info.ptMinTrackSize = POINT { x: data.min_width, y: data.min_height };
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(window);
            LRESULT(0)
        }
        WM_DESTROY => {
            if data.map(|data| data.quit_on_destroy).unwrap_or(false) {
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(window, message, wparam, lparam),
    }
}

/// 창 전체 크기와 위치: 마우스가 있는 모니터 가운데, 작업 영역을 넘지 않게.
unsafe fn frame_for(client: (i32, i32), dpi: u32, style: WINDOW_STYLE) -> (i32, i32, i32, i32) {
    let mut frame = RECT {
        left: 0,
        top: 0,
        right: win::scale(client.0, dpi),
        bottom: win::scale(client.1, dpi),
    };
    let _ = AdjustWindowRectEx(&mut frame, style, false, WINDOW_EX_STYLE(0));
    let (mut width, mut height) = (frame.right - frame.left, frame.bottom - frame.top);

    let mut cursor = POINT::default();
    let _ = GetCursorPos(&mut cursor);
    let mut monitor = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !GetMonitorInfoW(MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST), &mut monitor).as_bool() {
        return (CW_USEDEFAULT, CW_USEDEFAULT, width, height);
    }
    let work = monitor.rcWork;
    width = width.min(work.right - work.left);
    height = height.min(work.bottom - work.top);
    (
        work.left + ((work.right - work.left) - width) / 2,
        work.top + ((work.bottom - work.top) - height) / 2,
        width,
        height,
    )
}

/// 창을 띄우고 닫힐 때까지 머문다. `on_message` 는 화면이 보낸 메시지(JSON)마다 불린다.
pub fn run(options: Options, mut on_message: impl FnMut(&Host, Value)) -> Result<(), Unavailable> {
    if std::env::var_os("IKOPDF_CLASSIC_UI").is_some() {
        return Err(Unavailable);
    }
    unsafe {
        let instance: HINSTANCE = GetModuleHandleW(None).map_err(|_| Unavailable)?.into();
        let (r, g, b) = BACKGROUND;

        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hIcon: win::app_icon(instance),
            hbrBackground: CreateSolidBrush(COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)),
            lpszClassName: options.class_name,
            ..Default::default()
        };
        RegisterClassExW(&class);

        let dpi = win::system_dpi();
        let mut style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN;
        if options.resizable {
            style |= WS_THICKFRAME | WS_MAXIMIZEBOX;
        }
        let (x, y, width, height) = frame_for(options.size, dpi, style);
        let window = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            options.class_name,
            &HSTRING::from(options.title),
            style,
            x,
            y,
            width,
            height,
            None,
            None,
            Some(instance),
            None,
        )
        .map_err(|_| Unavailable)?;

        let mut min_frame = RECT {
            left: 0,
            top: 0,
            right: win::scale(options.min_size.0, dpi),
            bottom: win::scale(options.min_size.1, dpi),
        };
        let _ = AdjustWindowRectEx(&mut min_frame, style, false, WINDOW_EX_STYLE(0));
        let mut data = Box::new(WindowData {
            min_width: (min_frame.right - min_frame.left).min(width),
            min_height: (min_frame.bottom - min_frame.top).min(height),
            quit_on_destroy: false,
        });
        SetWindowLongPtrW(window, GWLP_USERDATA, &mut *data as *mut WindowData as isize);

        // 화면이 보낸 메시지는 일단 모아 두었다가 메시지 루프에서 처리한다.
        let inbox: Rc<RefCell<VecDeque<String>>> = Rc::new(RefCell::new(VecDeque::new()));
        let html = assemble(options.html);
        let mut context = WebContext::new(Some(web_data_dir()));
        let built = {
            let inbox = Rc::clone(&inbox);
            WebViewBuilder::new_with_web_context(&mut context)
                .with_html(html)
                .with_ipc_handler(move |request| inbox.borrow_mut().push_back(request.into_body()))
                .with_background_color((r, g, b, 255))
                .with_devtools(false)
                .with_hotkeys_zoom(false)
                .with_default_context_menus(false)
                .with_browser_accelerator_keys(false)
                .with_focused(true)
                .build(&RawWindow(window))
        };
        let webview = match built {
            Ok(webview) => webview,
            Err(_) => {
                let _ = DestroyWindow(window);
                return Err(Unavailable);
            }
        };
        data.quit_on_destroy = true;

        let (sender, receiver): (Sender<String>, Receiver<String>) = channel();
        let host = Host {
            window,
            webview,
            results: sender,
        };
        win::show_in_front(window);
        let _ = host.webview.focus();

        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);

            loop {
                let next = inbox.borrow_mut().pop_front();
                let Some(text) = next else { break };
                if let Ok(value) = serde_json::from_str::<Value>(&text) {
                    let is_ready = value["cmd"] == "ready";
                    on_message(&host, value);
                    // 개발용 빌드에서만: 화면이 준비되면 IKOPDF_UI_TEST 의 스크립트를 실행한다(자동 시험).
                    #[cfg(debug_assertions)]
                    if is_ready {
                        if let Ok(script) = std::env::var("IKOPDF_UI_TEST") {
                            host.eval(&script);
                        }
                    }
                    let _ = is_ready;
                }
            }
            while let Ok(script) = receiver.try_recv() {
                host.eval(&script);
            }
        }
        drop(host);
        drop(data);
        Ok(())
    }
}

/// 화면 파일 이름표. `w!` 로 만든 창 클래스 이름을 한곳에 둔다.
pub const SETTINGS_CLASS: PCWSTR = w!("IkoPdfSettingsWeb");
pub const ORDER_CLASS: PCWSTR = w!("IkoPdfOrderWeb");

//! 설정 창 (`ui/settings.html`) — 우클릭 메뉴 등록/해제, 자동 업데이트.
//!
//! 화면이 보내는 명령: `ready` `register` `unregister` `autostart{value}` `check` `install`
//! `open{target}` `close`. 결과는 `window.app.state(…)` 와 `window.app.done(명령, 결과)` 로 돌려준다.

use ikopdf::APP_NAME;
use serde_json::{json, Value};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::setup::{self, MenuState};
use crate::update;
use crate::webui::{self, call_script, Host, Options, Unavailable};

const BLOG_URL: &str = "https://blog.naver.com/event_code";

fn state() -> Value {
    json!({
        "version": update::current_version(),
        "installedVersion": setup::installed_version(),
        "menu": match setup::menu_state() {
            MenuState::Modern => "modern",
            MenuState::Classic => "classic",
            MenuState::NotRegistered => "none",
        },
        "autostart": setup::autostart_enabled(),
    })
}

/// 상태를 다시 알리고 작업 결과를 전하는 스크립트.
fn finished(command: &str, result: Value) -> String {
    call_script("state", &[&state()]) + &call_script("done", &[&json!(command), &result])
}

fn failure(error: &anyhow::Error) -> Value {
    json!({ "ok": false, "message": format!("{error:#}") })
}

fn open_url(host: &Host, url: &str) {
    unsafe {
        ShellExecuteW(
            Some(host.window()),
            w!("open"),
            &HSTRING::from(url),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

fn handle(host: &Host, message: Value) {
    match message["cmd"].as_str().unwrap_or_default() {
        "ready" => host.call("state", &state()),
        "register" => host.spawn(|| {
            let result = match setup::register_menu(true) {
                Ok(registration) => {
                    let _ = setup::set_autostart(true);
                    json!({ "ok": true, "fallback": registration.fallback_reason })
                }
                Err(error) => failure(&error),
            };
            finished("register", result)
        }),
        "unregister" => host.spawn(|| {
            let closing = setup::running_from_install_dir();
            let result = match setup::unregister_menu() {
                Ok(()) => json!({ "ok": true, "closing": closing }),
                Err(error) => json!({ "ok": false, "message": format!("{error:#}"), "closing": closing }),
            };
            finished("unregister", result)
        }),
        "autostart" => {
            let wanted = message["value"].as_bool().unwrap_or(false);
            let result = match setup::set_autostart(wanted) {
                Ok(()) => json!({ "ok": true }),
                Err(error) => failure(&error),
            };
            host.call("state", &state());
            if result["ok"] == false {
                host.spawn(move || call_script("done", &[&json!("autostart"), &result]));
            }
        }
        "check" => host.spawn(|| {
            let result = match update::latest_version() {
                Ok(latest) => {
                    let installed = setup::installed_version();
                    let baseline = installed
                        .clone()
                        .unwrap_or_else(|| update::current_version().to_string());
                    json!({
                        "ok": true,
                        "latest": latest,
                        "newer": update::is_newer(&latest, &baseline),
                        "canInstall": installed.is_some(),
                    })
                }
                Err(error) => failure(&error),
            };
            finished("check", result)
        }),
        "install" => host.spawn(|| {
            let result = match update::download_and_apply() {
                Ok(()) => json!({ "ok": true, "latest": setup::installed_version() }),
                Err(error) => failure(&error),
            };
            finished("install", result)
        }),
        "open" => match message["target"].as_str() {
            Some("releases") => open_url(host, &update::releases_page()),
            Some("github") => open_url(host, &update::repository_page()),
            Some("blog") => open_url(host, BLOG_URL),
            _ => {}
        },
        "close" => host.close(),
        _ => {}
    }
}

/// 설정 창을 띄운다. WebView2를 쓸 수 없으면 `Err` (기존 창으로 대신할 것).
pub fn show() -> Result<(), Unavailable> {
    setup::clean_leftovers();
    webui::run(
        Options {
            class_name: webui::SETTINGS_CLASS,
            title: &format!("{APP_NAME} 설정"),
            size: (540, 690),
            min_size: (540, 420),
            resizable: false,
            html: include_str!("../ui/settings.html"),
        },
        handle,
    )
}

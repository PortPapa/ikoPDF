//! 합치기 전에 파일 순서를 정하는 창 (`ui/order.html`).
//!
//! 화면이 보내는 명령: `ready` `ok{order:[번호…]}` `cancel`.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::webui::{self, Options, Unavailable};

fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let bytes = bytes as f64;
    if bytes >= KB * KB * KB {
        format!("{:.1} GB", bytes / (KB * KB * KB))
    } else if bytes >= KB * KB {
        format!("{:.1} MB", bytes / (KB * KB))
    } else {
        format!("{:.0} KB", (bytes / KB).max(1.0))
    }
}

fn describe(index: usize, path: &Path) -> Value {
    let text = |part: Option<&std::ffi::OsStr>| {
        part.map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
    };
    json!({
        "id": index,
        "name": text(path.file_name()),
        "dir": path.parent().map(|dir| dir.to_string_lossy().into_owned()).unwrap_or_default(),
        "ext": text(path.extension()),
        "size": std::fs::metadata(path).map(|meta| human_size(meta.len())).unwrap_or_default(),
    })
}

/// 순서 조정 창을 띄운다. "합치기"를 누르면 정한 순서를, 취소하면 `None` 을 돌려준다.
/// WebView2를 쓸 수 없으면 `Err` (기존 창으로 대신할 것).
pub fn choose_order(title: &str, items: &[PathBuf]) -> Result<Option<Vec<PathBuf>>, Unavailable> {
    let mut chosen: Option<Vec<PathBuf>> = None;
    webui::run(
        Options {
            class_name: webui::ORDER_CLASS,
            title,
            size: (680, 600),
            min_size: (520, 400),
            resizable: true,
            html: include_str!("../ui/order.html"),
        },
        |host, message| match message["cmd"].as_str().unwrap_or_default() {
            "ready" => {
                let list: Vec<Value> = items
                    .iter()
                    .enumerate()
                    .map(|(index, path)| describe(index, path))
                    .collect();
                host.call("init", &json!({ "items": list }));
            }
            "ok" => {
                let order: Vec<PathBuf> = message["order"]
                    .as_array()
                    .map(|ids| {
                        ids.iter()
                            .filter_map(|id| id.as_u64())
                            .filter_map(|id| items.get(id as usize).cloned())
                            .collect()
                    })
                    .unwrap_or_default();
                if !order.is_empty() {
                    chosen = Some(order);
                    host.close();
                }
            }
            "cancel" => host.close(),
            _ => {}
        },
    )?;
    Ok(chosen)
}

//! GitHub 릴리스에서 새 버전을 받아 설치 폴더의 실행 파일을 바꾼다.
//!
//! 릴리스에는 `ikoPDF.exe` 와 버전만 적힌 `version.txt` 가 올라가 있다(scripts\release.ps1).
//! 내려받은 파일의 출처 확인은 HTTPS(GitHub)에만 의존한다.

use std::ffi::c_void;
use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders,
    WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetTimeouts,
    INTERNET_DEFAULT_HTTPS_PORT, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE,
    WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
};

use crate::setup;

/// 릴리스를 올리는 GitHub 저장소 (`소유자/이름`).
pub const REPOSITORY: &str = "PortPapa/ikoPDF";

const MAX_VERSION_BYTES: usize = 64;
const MAX_EXE_BYTES: usize = 200 * 1024 * 1024;
const MIN_EXE_BYTES: usize = 200 * 1024;

pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

pub fn repository_page() -> String {
    format!("https://github.com/{REPOSITORY}")
}

pub fn releases_page() -> String {
    format!("https://github.com/{REPOSITORY}/releases/latest")
}

fn asset_url(name: &str) -> String {
    format!("https://github.com/{REPOSITORY}/releases/latest/download/{name}")
}

// ───────────────────────── HTTP ─────────────────────────

struct Handle(*mut c_void);

impl Handle {
    fn new(raw: *mut c_void, what: &str) -> Result<Handle> {
        if raw.is_null() {
            bail!("{what}: {}", windows::core::Error::from_thread().message());
        }
        Ok(Handle(raw))
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = WinHttpCloseHandle(self.0);
        }
    }
}

/// `https://호스트/경로` 를 내려받는다(리디렉션은 Windows가 따라간다). 시스템 프록시 설정을 쓴다.
fn http_get(url: &str, limit: usize) -> Result<Vec<u8>> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| anyhow!("https 주소만 지원합니다"))?;
    let (host, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    let network = |e: windows::core::Error| anyhow!("인터넷에 연결할 수 없습니다: {}", e.message());

    unsafe {
        let session = Handle::new(
            WinHttpOpen(
                w!("ikoPDF-updater"),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                PCWSTR::null(),
                PCWSTR::null(),
                0,
            ),
            "연결을 시작할 수 없습니다",
        )?;
        let _ = WinHttpSetTimeouts(session.0, 8_000, 8_000, 15_000, 30_000);
        let connection = Handle::new(
            WinHttpConnect(session.0, &HSTRING::from(host), INTERNET_DEFAULT_HTTPS_PORT, 0),
            "서버에 연결할 수 없습니다",
        )?;
        let request = Handle::new(
            WinHttpOpenRequest(
                connection.0,
                w!("GET"),
                &HSTRING::from(path),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                WINHTTP_FLAG_SECURE,
            ),
            "요청을 만들 수 없습니다",
        )?;
        WinHttpSendRequest(request.0, None, None, 0, 0, 0).map_err(network)?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut()).map_err(network)?;

        let mut status = 0u32;
        let mut status_size = std::mem::size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut c_void),
            &mut status_size,
            std::ptr::null_mut(),
        )
        .map_err(network)?;
        if status == 404 {
            bail!("아직 올라온 버전이 없습니다");
        }
        if status != 200 {
            bail!("서버 응답이 올바르지 않습니다 (HTTP {status})");
        }

        let mut body = Vec::new();
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            let mut read = 0u32;
            WinHttpReadData(
                request.0,
                chunk.as_mut_ptr() as *mut c_void,
                chunk.len() as u32,
                &mut read,
            )
            .map_err(network)?;
            if read == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..read as usize]);
            if body.len() > limit {
                bail!("내려받은 파일이 예상보다 큽니다");
            }
        }
        Ok(body)
    }
}

// ───────────────────────── 버전 ─────────────────────────

fn parse_version(text: &str) -> Option<Vec<u32>> {
    let text = text.trim().trim_start_matches(['v', 'V']);
    if text.is_empty() {
        return None;
    }
    text.split('.').map(|part| part.trim().parse().ok()).collect()
}

/// `candidate` 가 `current` 보다 새 버전인지.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(candidate), Some(current)) => candidate > current,
        _ => false,
    }
}

/// 릴리스에 올라와 있는 최신 버전 번호.
pub fn latest_version() -> Result<String> {
    let bytes = http_get(&asset_url("version.txt"), MAX_VERSION_BYTES)?;
    let text = String::from_utf8_lossy(&bytes).trim().trim_start_matches('\u{feff}').to_string();
    if parse_version(&text).is_none() {
        bail!("버전 정보를 읽을 수 없습니다");
    }
    Ok(text.trim_start_matches(['v', 'V']).to_string())
}

fn update_exe() -> PathBuf {
    setup::install_dir().join("update").join(setup::EXE_NAME)
}

/// 새 실행 파일을 내려받아 `--apply-update` 로 실행한다. 그 프로세스가 설치 폴더를 갱신한다.
pub fn download_and_apply() -> Result<()> {
    let bytes = http_get(&asset_url(setup::EXE_NAME), MAX_EXE_BYTES)?;
    if bytes.len() < MIN_EXE_BYTES || !bytes.starts_with(b"MZ") {
        bail!("내려받은 파일이 실행 파일이 아닙니다");
    }
    let target = update_exe();
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).context("업데이트 폴더를 만들 수 없습니다")?;
    }
    std::fs::write(&target, bytes).context("업데이트 파일을 저장할 수 없습니다")?;
    let status = std::process::Command::new(&target)
        .arg("--apply-update")
        .status()
        .context("업데이트를 실행할 수 없습니다")?;
    if !status.success() {
        bail!("업데이트를 적용하지 못했습니다");
    }
    Ok(())
}

/// 로그인 때 조용히 실행되는 확인. 새 버전이 있으면 설치하고, 문제가 있으면 아무것도 하지 않는다.
pub fn startup_check() {
    setup::clean_leftovers();
    let installed = setup::installed_version().unwrap_or_else(|| current_version().to_string());
    if let Ok(latest) = latest_version() {
        if is_newer(&latest, &installed) {
            let _ = download_and_apply();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions_numerically() {
        assert!(is_newer("1.0.10", "1.0.9"));
        assert!(is_newer("v1.1.0", "1.0.99"));
        assert!(is_newer("2.0", "1.9.9"));
        assert!(!is_newer("1.0.0", "1.0.0"));
        assert!(!is_newer("0.9.9", "1.0.0"));
        assert!(!is_newer("이상한 값", "1.0.0"));
    }
}

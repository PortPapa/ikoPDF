//! 실행 파일 하나로 배포할 수 있도록 필요한 것들을 함께 넣는다.
//!
//! - Windows 매니페스트(최신 모양의 버튼·목록, 고해상도 화면, 긴 경로)
//! - 우클릭 메뉴 DLL (`target\<profile>\ikopdf_shell.dll` — 먼저 빌드되어 있어야 한다)
//! - 서명된 메뉴 패키지와 공개 인증서 (`package\dist\`, scripts\make-package.ps1 이 만든다)
//!
//! 아직 없는 파일은 빈 파일로 대신한다. 그런 빌드는 변환은 되지만 메뉴 등록은 할 수 없다.

use std::path::{Path, PathBuf};

use embed_manifest::{embed_manifest, new_manifest};

fn stage(source: &Path, out_dir: &Path, name: &str) {
    println!("cargo:rerun-if-changed={}", source.display());
    let bytes = std::fs::read(source).unwrap_or_default();
    if bytes.is_empty() {
        println!("cargo:warning={} 이(가) 없어 빈 파일로 넣습니다", source.display());
    }
    std::fs::write(out_dir.join(name), bytes).expect("OUT_DIR에 쓸 수 없습니다");
}

/// AppxManifest.xml 의 `<Identity … Version="a.b.c.d">` 값.
fn package_version(manifest: &Path) -> String {
    let text = std::fs::read_to_string(manifest).unwrap_or_default();
    text.split("<Identity")
        .nth(1)
        .and_then(|rest| rest.split("Version=\"").nth(1))
        .and_then(|rest| rest.split('"').next())
        .unwrap_or("0.0.0.0")
        .to_string()
}

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let profile = std::env::var("PROFILE").unwrap();
    let workspace = manifest_dir.join("..").join("..");
    let package = workspace.join("package");

    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        embed_manifest(new_manifest("ikoPDF")).expect("매니페스트를 넣지 못했습니다");
    }

    stage(
        &workspace.join("target").join(&profile).join("ikopdf_shell.dll"),
        &out_dir,
        "shell.dll",
    );
    stage(&package.join("dist").join("ikoPDF.msix"), &out_dir, "package.msix");
    stage(&package.join("dist").join("ikoPDF.cer"), &out_dir, "package.cer");
    stage(&package.join("Assets").join("menu.ico"), &out_dir, "menu.ico");

    let appx_manifest = package.join("AppxManifest.xml");
    println!("cargo:rerun-if-changed={}", appx_manifest.display());
    println!(
        "cargo:rustc-env=IKOPDF_PACKAGE_VERSION={}",
        package_version(&appx_manifest)
    );
    println!("cargo:rerun-if-changed=build.rs");
}

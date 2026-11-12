//! 실행 파일 하나로 배포할 수 있도록 필요한 것들을 함께 넣는다.
//!
//! - 프로그램 아이콘(이코 캐릭터), 버전 정보, Windows 매니페스트
//!   (최신 모양의 버튼·목록, 고해상도 화면, 긴 경로) — Windows SDK의 rc.exe 가 필요하다.
//! - 우클릭 메뉴 DLL (`target\<profile>\ikopdf_shell.dll` — 먼저 빌드되어 있어야 한다)
//! - 서명된 메뉴 패키지와 공개 인증서 (`package\dist\`, scripts\make-package.ps1 이 만든다)
//!
//! 아직 없는 파일은 빈 파일로 대신한다. 그런 빌드는 변환은 되지만 메뉴 등록은 할 수 없다.

use std::path::{Path, PathBuf};

const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" xmlns:asmv3="urn:schemas-microsoft-com:asm.v3" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="ikoPDF" version="1.0.0.0"/>
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
    </application>
  </compatibility>
  <asmv3:application>
    <asmv3:windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2, PerMonitor</dpiAwareness>
      <longPathAware xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">true</longPathAware>
      <activeCodePage xmlns="http://schemas.microsoft.com/SMI/2019/WindowsSettings">UTF-8</activeCodePage>
    </asmv3:windowsSettings>
  </asmv3:application>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
</assembly>
"#;

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
        let icon = package.join("Assets").join("app.ico");
        println!("cargo:rerun-if-changed={}", icon.display());
        let mut resources = winresource::WindowsResource::new();
        resources
            .set_manifest(MANIFEST)
            .set("ProductName", "iko PDF 변환")
            .set("FileDescription", "iko PDF 변환")
            .set("CompanyName", "event_code")
            .set("LegalCopyright", "event_code (blog.naver.com/event_code)")
            .set_language(0x0412);
        if icon.exists() {
            resources.set_icon(&icon.to_string_lossy());
        }
        resources.compile().expect("아이콘·매니페스트를 넣지 못했습니다 (Windows SDK 필요)");
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

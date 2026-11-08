use std::path::{Path, PathBuf};
use std::process::Command;

/// Windows resources compiled by `zig rc`: the icon and manifest for both executables
/// (current Windows controls, sharp on high-DPI screens).
fn main() {
    // A local preview can package new binaries without overwriting the signed release in dist/.
    println!("cargo:rerun-if-env-changed=CLEARWAY_BUNDLE_DIR");
    let bundle = std::env::var_os("CLEARWAY_BUNDLE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR")).join("dist")
        });
    println!("cargo:rustc-env=CLEARWAY_BUNDLE_DIR={}", bundle.display());
    println!("cargo:rerun-if-changed=assets/icon/clearway.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let ico = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
        .join("assets/icon/clearway.ico");
    let escape = |p: &Path| p.display().to_string().replace('\\', "\\\\");
    let manifest = out.join("setup.manifest");
    std::fs::write(&manifest, SETUP_MANIFEST).expect("setup.manifest");
    let app_manifest = out.join("clearway.manifest");
    std::fs::write(
        &app_manifest,
        SETUP_MANIFEST.replace("AW-HTK.Clearway.Setup", "AW-HTK.Clearway"),
    )
    .expect("clearway.manifest");
    compile(
        &out,
        "clearway",
        &format!("1 ICON \"{}\"\n1 24 \"{}\"\n", escape(&ico), escape(&app_manifest)),
    );
    compile(
        &out,
        "clearway-setup",
        &format!("1 ICON \"{}\"\n1 24 \"{}\"\n", escape(&ico), escape(&manifest)),
    );
}

fn compile(out: &Path, bin: &str, script: &str) {
    let rc = out.join(format!("{bin}.rc"));
    let res = out.join(format!("{bin}.res"));
    std::fs::write(&rc, script).expect("rc");
    match Command::new("zig").arg("rc").arg("/fo").arg(&res).arg(&rc).status() {
        Ok(s) if s.success() => println!("cargo:rustc-link-arg-bin={bin}={}", res.display()),
        _ => println!("cargo:warning=zig rc unavailable: {bin}.exe is built without its resources"),
    }
}

const SETUP_MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="AW-HTK.Clearway.Setup" version="1.0.0.0"/>
  <dependency><dependentAssembly><assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/></dependentAssembly></dependency>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3"><security><requestedPrivileges><requestedExecutionLevel level="asInvoker" uiAccess="false"/></requestedPrivileges></security></trustInfo>
  <application xmlns="urn:schemas-microsoft-com:asm.v3"><windowsSettings><dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true</dpiAware></windowsSettings></application>
</assembly>
"#;

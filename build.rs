//! Build script: embeds a Windows application manifest and version resource.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_ARCH");

    // Only embed resources on Windows
    #[cfg(windows)]
    embed_windows_resources();
}

#[cfg(windows)]
fn embed_windows_resources() {
    const DESCRIPTION: &str = "A command line utility for managing webcam configuration on windows";

    // Windows manifests require a numeric 4-part version, so drop any
    // pre-release/build suffix ("0.4.0-rc1" -> "0.4.0") before appending ".0".
    let version = env!("CARGO_PKG_VERSION");
    let numeric_version = version
        .split(['-', '+'])
        .next()
        .expect("split always yields at least one element");
    let manifest_version = format!("{numeric_version}.0");

    // The manifest's processorArchitecture must match the build target;
    // "amd64" and "arm64" are the identifiers Windows expects.
    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH")
        .expect("cargo always sets CARGO_CFG_TARGET_ARCH for build scripts");
    let processor_architecture = match target_arch.as_str() {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "x86" => "x86",
        _ => "*",
    };

    let manifest = format!(
        r#"
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity
    version="{manifest_version}"
    processorArchitecture="{processor_architecture}"
    name="wincamcfg"
    type="win32"
  />
  <description>{DESCRIPTION}</description>

  <!-- Execution Level -->
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>

  <!-- Application Compatibility -->
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <!-- Windows 10 and Windows 11 (well-known supportedOS GUID) -->
      <supportedOS Id="{{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}}"/>
    </application>
  </compatibility>

  <!-- Console Application Settings -->
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <heapType xmlns="http://schemas.microsoft.com/SMI/2020/WindowsSettings">SegmentHeap</heapType>
    </windowsSettings>
  </application>
</assembly>
"#,
    );

    let mut res = winresource::WindowsResource::new();
    res.set_manifest(&manifest);
    res.set("ProductName", "wincamcfg")
        .set("FileDescription", DESCRIPTION)
        .set("CompanyName", "wincamcfg contributors")
        .set(
            "LegalCopyright",
            "Copyright (C) wincamcfg contributors. Licensed under the MIT License.",
        )
        .set("ProductVersion", version)
        .set("FileVersion", version);

    res.compile()
        .expect("failed to compile Windows resources (is rc.exe or llvm-rc available?)");
}

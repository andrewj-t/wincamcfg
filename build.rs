// Build script to embed Windows manifest and resources

fn main() {
    // Only embed resources on Windows
    #[cfg(windows)]
    {
        // Get version from Cargo.toml
        let version = env!("CARGO_PKG_VERSION");
        
        let mut res = winresource::WindowsResource::new();
        res.set_manifest(r#"
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity
    version="0.1.0.0"
    processorArchitecture="amd64"
    name="wincamcfg"
    type="win32"
  />
  <description>A command line utility for managing webcam configuration on windows</description>
  
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
      <!-- Windows 10 and Windows 11 -->
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
    </application>
  </compatibility>
  
  <!-- Console Application Settings -->
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <heapType xmlns="http://schemas.microsoft.com/SMI/2020/WindowsSettings">SegmentHeap</heapType>
    </windowsSettings>
  </application>
</assembly>
"#);
        
        res.set("ProductName", "wincamcfg")
           .set("FileDescription", "A command line utility for managing webcam configuration on windows")
           .set("CompanyName", "Open Source")
           .set("LegalCopyright", "Copyright (C) 2025. Licensed under the MIT License.")
           .set("ProductVersion", version)
           .set("FileVersion", version);
        
        res.compile().unwrap();
    }
}

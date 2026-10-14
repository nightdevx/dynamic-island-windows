//! Embeds the app icon and the version information (shown in the file's Properties and the
//! installer) into island.exe. The resource script is written here so its version always
//! matches Cargo.toml.

fn main() {
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let numbers: Vec<&str> = version.split(['.', '-']).take(3).collect();
    let numeric = format!("{},{},{},0", numbers[0], numbers[1], numbers[2]);
    let root = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let icon = root.join("assets").join("island.ico");
    let script = format!(
        r#"#pragma code_page(65001)
1 ICON "{icon}"
1 VERSIONINFO
FILEVERSION {numeric}
PRODUCTVERSION {numeric}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "041f04b0"
    BEGIN
      VALUE "CompanyName", "nightdijital"
      VALUE "FileDescription", "Dynamic Island"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "island"
      VALUE "LegalCopyright", "© 2026 nightdijital"
      VALUE "OriginalFilename", "island.exe"
      VALUE "ProductName", "Dynamic Island"
      VALUE "ProductVersion", "{version}"
      VALUE "Comments", "https://nightdijital.com"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x41f, 1200
  END
END
"#,
        icon = icon.display().to_string().replace('\\', "\\\\"),
    );
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("island.rc");
    std::fs::write(&out, script).unwrap();
    embed_resource::compile(&out, embed_resource::NONE).manifest_optional().unwrap();
    println!("cargo:rerun-if-changed=assets/island.ico");
}

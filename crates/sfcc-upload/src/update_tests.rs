use super::*;

#[test]
fn the_latest_tag_comes_from_where_github_redirects() {
    assert_eq!(
        tag_of("https://github.com/salva-sm/sfcc-tools/releases/tag/v0.18.0").as_deref(),
        Some("v0.18.0")
    );
    assert_eq!(
        tag_of("https://github.com/salva-sm/sfcc-tools/releases"),
        None
    );
    assert_eq!(
        tag_of("https://github.com/salva-sm/sfcc-tools/releases/tag/nightly"),
        None
    );
}

#[test]
fn releases_compare_by_number_not_by_text() {
    assert!(is_newer("v0.18.0", "v0.17.0"));
    assert!(is_newer("v0.10.0", "v0.9.3"));
    assert!(is_newer("v1.0.0", "v0.99.0"));
    assert!(!is_newer("v0.18.0", "v0.18.0"));
    assert!(!is_newer("v0.17.2", "v0.18.0"));
}

#[test]
fn what_is_not_a_release_is_never_newer() {
    assert!(!is_newer("nightly", "v0.18.0"));
    assert!(!is_newer("v0.19.0", "main"));
}

#[test]
fn assets_are_named_as_release_yml_names_them() {
    assert_eq!(
        asset_name("sfcc-upload", "x86_64-windows"),
        "sfcc-upload-x86_64-windows.exe"
    );
    assert_eq!(
        asset_name("sfcc-tui", "x86_64-windows"),
        "sfcc-tui-x86_64-windows.exe"
    );
    assert_eq!(
        asset_name("isml-lsp", "x86_64-windows"),
        "isml-lsp-x86_64-windows.zip"
    );
    assert_eq!(
        asset_name("log-diff", "aarch64-macos"),
        "log-diff-aarch64-macos.tar.gz"
    );
    assert_eq!(
        asset_name("sfcc-dap", "x86_64-linux"),
        "sfcc-dap-x86_64-linux.tar.gz"
    );
}

#[test]
fn a_zipped_binary_is_taken_out_of_its_archive() {
    let mut zipped = Vec::new();
    {
        let mut writer = zip::ZipWriter::new(Cursor::new(&mut zipped));
        writer
            .start_file("isml-lsp.exe", zip::write::SimpleFileOptions::default())
            .unwrap();
        std::io::Write::write_all(&mut writer, b"binary").unwrap();
        writer.finish().unwrap();
    }
    let dir = std::env::temp_dir();
    assert_eq!(
        unpack("isml-lsp-x86_64-windows.zip", zipped, "isml-lsp.exe", &dir).unwrap(),
        b"binary"
    );
}

#[test]
fn the_new_binary_takes_the_old_ones_place() {
    let dir = std::env::temp_dir().join(format!("sfcc-upload-update-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let target = dir.join(binary_name("log-diff"));
    std::fs::write(&target, b"old").unwrap();

    replace(&target, b"new").unwrap();

    assert_eq!(std::fs::read(&target).unwrap(), b"new");
    assert!(!suffixed(&target, ".new").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

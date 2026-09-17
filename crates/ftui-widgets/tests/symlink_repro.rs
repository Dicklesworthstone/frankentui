#[cfg(unix)]
#[test]
fn symlink_to_dir_is_navigable() {
    use ftui_widgets::file_picker::FilePickerState;

    let mut attempt = 0_u64;
    let root = loop {
        let dir =
            std::env::temp_dir().join(format!("ftui-symlink-{}-{attempt}", std::process::id()));
        match std::fs::create_dir(&dir) {
            Ok(()) => break dir,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => attempt += 1,
            Err(error) => panic!("create retained fixture {}: {error}", dir.display()),
        }
    };

    let target_dir = root.join("target_dir");
    std::fs::create_dir(&target_dir).unwrap();

    let link_path = root.join("link_to_dir");
    std::os::unix::fs::symlink(&target_dir, &link_path).unwrap();
    let state = FilePickerState::from_path(&root).unwrap();
    let link_entry = state
        .entries
        .iter()
        .find(|e| e.name == "link_to_dir")
        .unwrap();
    assert!(link_entry.is_dir, "Symlink to dir should be treated as dir");
}

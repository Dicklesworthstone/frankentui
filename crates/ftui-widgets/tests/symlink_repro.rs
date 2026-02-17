use ftui_widgets::file_picker::FilePickerState;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
#[test]
fn symlink_to_dir_is_navigable() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after unix epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "ftui_symlink_test_{}_{}",
        std::process::id(),
        nonce
    ));
    if root.exists() {
        std::fs::remove_dir_all(&root).unwrap();
    }
    std::fs::create_dir(&root).unwrap();

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

    std::fs::remove_dir_all(&root).unwrap();
}

use serde_json::Value;
use std::process::Command;

#[test]
fn unavailable_credential_store_fails_closed_and_anonymous_mode_bypasses_it() {
    let root = tempfile::tempdir().unwrap();
    let sessions = root.path().join("youtube-music-core/session");
    std::fs::create_dir_all(&sessions).unwrap();
    // Presence of an unreadable credential must not silently become signed out.
    std::fs::write(sessions.join("default.gpg"), b"synthetic-invalid-entry").unwrap();
    let run = |anonymous: bool| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ytmusic"));
        command
            .env("PASSWORD_STORE_DIR", root.path())
            .env("PATH", root.path())
            .env_remove("YTMUSIC_CONFIG")
            .args(["--store", "pass"]);
        if anonymous {
            command.arg("--anonymous");
        }
        let output = command.args(["auth", "status"]).output().unwrap();
        let json: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(!String::from_utf8_lossy(&output.stdout).contains("synthetic-invalid-entry"));
        (output.status.success(), json)
    };
    let (ok, result) = run(false);
    assert!(!ok);
    assert_eq!(result["error"]["code"], "credential_storage");
    let (ok, result) = run(true);
    assert!(ok);
    assert_eq!(result["data"]["state"], "signed_out");
    std::fs::remove_file(sessions.join("default.gpg")).unwrap();
    let (ok, result) = run(false);
    assert!(ok);
    assert_eq!(result["data"]["state"], "signed_out");
}

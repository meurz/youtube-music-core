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

#[test]
fn login_prints_official_web_url_and_requires_cookie_import_without_contacting_oauth() {
    let root = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_ytmusic"))
        .env("PASSWORD_STORE_DIR", root.path())
        .env("PATH", root.path())
        .env_remove("YTMUSIC_CONFIG")
        .args(["--store", "pass", "auth", "login", "--no-open"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["data"]["state"], "import_required");
    assert_eq!(result["data"]["method"], "browser_cookie");
    let url = result["data"]["login_url"].as_str().unwrap();
    assert!(url.starts_with("https://accounts.google.com/ServiceLogin?"));
    assert!(url.contains("music.youtube.com"));
    assert!(result["data"]["next"]
        .as_str()
        .unwrap()
        .contains("auth import"));
    assert!(result["data"].get("user_code").is_none());
}

#[test]
fn legacy_oauth_config_requires_import_but_explicit_anonymous_ignores_it() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.json");
    for field in ["oauth", "music_oauth"] {
        std::fs::write(&config, serde_json::to_vec(&serde_json::json!({
            field:{"access_token":"synthetic-private-access","refresh_token":"synthetic-private-refresh"}
        })).unwrap()).unwrap();
        for anonymous in [false, true] {
            let mut command = Command::new(env!("CARGO_BIN_EXE_ytmusic"));
            command
                .env("PASSWORD_STORE_DIR", root.path())
                .env("PATH", root.path())
                .env_remove("YTMUSIC_CONFIG")
                .args(["--store", "pass", "--config"])
                .arg(&config);
            if anonymous {
                command.arg("--anonymous");
            }
            let output = command.args(["auth", "status"]).output().unwrap();
            let text = String::from_utf8_lossy(&output.stdout);
            assert!(!text.contains("synthetic-private"));
            let result: Value = serde_json::from_slice(&output.stdout).unwrap();
            if anonymous {
                assert!(output.status.success());
                assert_eq!(result["data"]["state"], "signed_out");
            } else {
                assert!(!output.status.success());
                assert_eq!(result["error"]["code"], "invalid_input");
                assert!(result["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("auth import"));
            }
        }
    }
}

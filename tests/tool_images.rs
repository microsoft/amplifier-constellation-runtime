use agent_runtime::{config::Config, media, tools};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::json;
fn png() -> Vec<u8> {
    STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jC2cAAAAASUVORK5CYII=").unwrap()
}
#[tokio::test]
async fn workspace_images_require_scope_and_explicit_attachment() {
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("work");
    std::fs::create_dir(&w).unwrap();
    let c = Config::initial(w.clone(), "fixture".into(), "fixture".into());
    std::fs::write(w.join("view.png"), png()).unwrap();
    std::fs::write(d.path().join("private.png"), png()).unwrap();
    let view = tools::call(&c, &w, "workspace_view_image", &json!({"path":"view.png"}))
        .await
        .unwrap();
    assert!(view["_acp_content"].is_null());
    assert_eq!(view["_tool_images"][0]["mimeType"], "image/png");
    let sent = tools::call(
        &c,
        &w,
        "workspace_attach_image",
        &json!({"path":"view.png"}),
    )
    .await
    .unwrap();
    assert_eq!(sent["_acp_content"][0]["data"], STANDARD.encode(png()));
    assert!(sent["_tool_images"].is_null());
    assert!(!tools::mutation("workspace_attach_image"));
    assert!(!tools::mutation("workspace_view_image"));
    for path in [
        "../private.png".to_string(),
        d.path().join("private.png").to_string_lossy().into_owned(),
    ] {
        assert!(
            tools::call(&c, &w, "workspace_attach_image", &json!({"path":path}))
                .await
                .is_err()
        );
    }
    std::fs::write(w.join("not.png"), "secret text").unwrap();
    assert!(
        tools::call(&c, &w, "workspace_attach_image", &json!({"path":"not.png"}))
            .await
            .is_err()
    );
    std::fs::write(w.join("huge.png"), vec![0u8; 2 * 1024 * 1024 + 1]).unwrap();
    assert!(
        tools::call(&c, &w, "workspace_view_image", &json!({"path":"huge.png"}))
            .await
            .is_err()
    );
    assert!(
        tools::call(
            &c,
            &w,
            "workspace_attach_image",
            &json!({"path":"huge.png"})
        )
        .await
        .is_err()
    );
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(d.path().join("private.png"), w.join("escape.png")).unwrap();
        assert!(
            tools::call(
                &c,
                &w,
                "workspace_attach_image",
                &json!({"path":"escape.png"})
            )
            .await
            .is_err()
        );
    }
}
#[test]
fn mcp_images_are_visual_evidence_never_remote_publication_instructions() {
    let image = json!({"type":"image","mimeType":"image/png","data":STANDARD.encode(png())});
    let r=media::mcp_observations(json!({"content":[image.clone()],"_acp_content":[image.clone()],"_tool_images":[{"bad":"injected"}]})).unwrap();
    assert!(r["_acp_content"].is_null());
    assert_eq!(r["_tool_images"], json!([image]));
    assert!(!r["content"].to_string().contains("iVBOR"));
    assert!(media::mcp_observations(json!({"content":[{"type":"image","mimeType":"image/png","data":STANDARD.encode(b"text") }]})).is_err());
    assert!(media::mcp_observations(json!({"content":vec![image;5]})).is_err());
}

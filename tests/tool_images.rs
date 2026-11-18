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

#[tokio::test]
async fn attaches_named_workspace_document_bytes_only_from_granted_roots() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("work");
    std::fs::create_dir(&root).unwrap();
    let config = Config::initial(root.clone(), "fixture".into(), "fixture".into());
    let name = "05-évidence & acceptance.md";
    let text = "# Evidence\r\n\nLast line — intact.\n";
    std::fs::write(root.join(name), text).unwrap();
    let result = tools::call(
        &config,
        &root,
        "workspace_attach_file",
        &json!({"path":name}),
    )
    .await
    .unwrap();
    let block = &result["_acp_content"][0]["resource"];
    assert_eq!(block["text"], text);
    assert_eq!(block["mimeType"], "text/markdown");
    let uri = reqwest::Url::parse(block["uri"].as_str().unwrap()).unwrap();
    assert_eq!(
        uri.query_pairs().find(|(k, _)| k == "name").unwrap().1,
        name
    );
    assert!(!uri.as_str().contains(root.to_str().unwrap()));
    for (path, bytes) in [
        ("large.md", vec![b'x'; 200_001]),
        ("invalid.md", vec![255]),
        ("binary.md", vec![0]),
        ("executable.sh", vec![b'x']),
    ] {
        std::fs::write(root.join(path), bytes).unwrap();
        assert!(
            tools::call(
                &config,
                &root,
                "workspace_attach_file",
                &json!({"path":path})
            )
            .await
            .is_err()
        );
    }
    std::fs::write(temp.path().join("private.md"), "private").unwrap();
    assert!(
        tools::call(
            &config,
            &root,
            "workspace_attach_file",
            &json!({"path":"../private.md"})
        )
        .await
        .is_err()
    );
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(temp.path().join("private.md"), root.join("alias.md")).unwrap();
        assert!(
            tools::call(
                &config,
                &root,
                "workspace_attach_file",
                &json!({"path":"alias.md"})
            )
            .await
            .is_err()
        );
    }
}

use waydroid_control_center_lib::{android, providers, workflow};

#[test]
#[ignore = "Requires explicit live Waydroid testing"]
fn live_waydroid_surface() {
    // Cold start: the workflow must start the session, connect ADB, select the
    // device and verify readiness without any manual setup.
    let report = workflow::run("bring_up", None).expect("bring_up workflow");
    println!(
        "workflow {} success={} steps={}",
        report.id,
        report.success,
        report.steps.len()
    );
    for step in &report.steps {
        println!("  ok={} {} -> {}", step.ok, step.name, step.detail);
    }
    assert!(report.success, "bring_up failed: {:?}", report.steps);

    let device = android::get_selected_device().expect("selected device");
    println!("selected: {}", device.serial);

    let display = android::display_info().expect("display info");
    println!("display: {display:?}");

    let shot = android::screenshot_base64().expect("screenshot");
    println!("screenshot base64 bytes: {}", shot.len());

    let files = android::list_android_files("/sdcard").expect("list files");
    println!(
        "files: {:?}",
        files.iter().map(|file| &file.name).collect::<Vec<_>>()
    );

    println!("tap: {:?}", android::tap(960, 540));
    println!("swipe: {:?}", android::swipe(960, 800, 960, 300, 250));
    println!("key home: {:?}", android::press_key("home"));
    println!("key unknown: {:?}", android::press_key("reboot"));

    std::fs::write("/tmp/opencode/upload-test.txt", b"waydroid control center").unwrap();
    println!(
        "upload: {:?}",
        android::push_file_to_android("/tmp/opencode/upload-test.txt", "/sdcard/Download")
    );

    println!(
        "ollama models: {:?}",
        providers::list_models(providers::OLLAMA, providers::DEFAULT_OLLAMA_URL)
    );
    println!(
        "ollama chat: {:?}",
        providers::chat(
            providers::OLLAMA,
            providers::DEFAULT_OLLAMA_URL,
            "qwen2.5-coder:1.5b",
            &[providers::ChatMessage {
                role: "user".to_string(),
                content: "Reply with the single word: ready".to_string(),
                images: None,
            }],
        )
    );
}

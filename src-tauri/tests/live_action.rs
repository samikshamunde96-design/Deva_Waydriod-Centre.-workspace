use std::time::{Duration, Instant};
use waydroid_control_center_lib::{agent, providers};

/// Opt-in check of the full Action loop against a real local model. It calls
/// only allowlisted read tools; with no ADB device selected they fail honestly,
/// which is exactly what the engine must report.
#[test]
#[ignore = "Requires a running local Ollama service; uses read-only tools only"]
fn live_action_loop_reports_tool_results_honestly() {
    let model = "qwen2.5-coder:1.5b".to_string();
    let task = agent::start(
        "Report the current Android device health. Do not change anything.",
        providers::OLLAMA,
        providers::DEFAULT_OLLAMA_URL,
        &model,
        Some(3),
        Some("read_only"),
    )
    .expect("action task started");
    println!("task {} status {}", task.id, task.status);

    let deadline = Instant::now() + Duration::from_secs(150);
    let mut latest = task;
    while !matches!(latest.status.as_str(), "completed" | "failed" | "cancelled") {
        assert!(
            Instant::now() < deadline,
            "action loop did not finish in time"
        );
        std::thread::sleep(Duration::from_millis(1500));
        latest = agent::get(&latest.id).expect("action snapshot");
    }

    println!("status: {}", latest.status);
    for step in &latest.steps {
        println!(
            "[{}] {} ok={} -> {}",
            step.index, step.title, step.ok, step.detail
        );
    }
    if let Some(error) = &latest.error {
        println!("error: {error}");
    }
    assert!(latest.steps.iter().any(|step| step.kind == "action"));
    assert!(latest.pending_approval.is_none());
}

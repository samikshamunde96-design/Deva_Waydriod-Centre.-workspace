use waydroid_control_center_lib::providers;

/// Opt-in check against the real local Ollama service. It never touches a
/// device, so it is safe to run while Waydroid is stopped.
#[test]
#[ignore = "Requires a running local Ollama service"]
fn live_ollama_models_capabilities_and_chat() {
    let models = providers::list_models(providers::OLLAMA, providers::DEFAULT_OLLAMA_URL)
        .expect("Ollama model list");
    assert!(!models.is_empty(), "Ollama reported no models");
    let model = models[0].name.clone();
    println!(
        "models: {:?}",
        models.iter().map(|m| &m.name).collect::<Vec<_>>()
    );

    let capabilities =
        providers::probe_model(providers::OLLAMA, providers::DEFAULT_OLLAMA_URL, &model)
            .expect("model capabilities");
    println!("capabilities: {capabilities:?}");

    let reply = providers::chat(
        providers::OLLAMA,
        providers::DEFAULT_OLLAMA_URL,
        &model,
        &[providers::ChatMessage {
            role: "user".to_string(),
            content: "Reply with the single word: ready".to_string(),
            images: None,
        }],
    )
    .expect("chat reply");
    println!("reply from {}: {}", reply.model, reply.content);
    assert!(!reply.content.trim().is_empty());
}

//! DS Code CLI — Phase 1: single-message chat with the agent.

use anyhow::Result;
use dscode_core::{
    agent::forge::Forge,
    agent::stream::StreamEvent,
    config::settings::Config,
    providers::openai::OpenAiProvider,
    providers::trait_def::{Message, MessageContent, Role},
    session::manager::SessionManager,
    tools::registry::ToolRegistry,
};
use std::sync::Arc;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();
    let message = if args.len() > 1 {
        args[1..].join(" ")
    } else {
        eprintln!("Usage: dscode-cli <message>");
        eprintln!("Example: dscode-cli \"List all .rs files\"");
        std::process::exit(1);
    };

    // Load config
    let config = Config::load()?;
    let working_dir = std::env::current_dir()?;

    // Setup provider
    let provider = OpenAiProvider::from_config(&config.default_model, &config);

    // Setup tools
    let mut registry = ToolRegistry::new();
    registry.register(dscode_core::tools::bash::DoBash::new());
    registry.register(dscode_core::tools::file_ops::DoFileRead::new());
    registry.register(dscode_core::tools::file_ops::DoFileWrite::new());
    let registry = Arc::new(registry);

    // Setup forge
    let forge = Forge::new(Box::new(provider), registry.clone(), working_dir.clone());
    let (tx, mut rx) = mpsc::unbounded_channel::<StreamEvent>();

    // Create initial session
    let session_manager = SessionManager::new(config.session.retention_days)
        .map_err(|e| anyhow::anyhow!("Session: {}", e))?;
    let session = session_manager.create_session("New Chat", "/Users/zay/Desktop/DS_code")
        .map_err(|e| anyhow::anyhow!("Create session: {}", e))?;

    // Persist user message
    session_manager.add_message(&session.id, &Message {
        role: Role::User,
        content: MessageContent::Text(message.clone()),
        name: None, tool_calls: None, tool_call_id: None,
        reasoning_content: None, created_at: chrono::Utc::now().timestamp(),
    }).ok();

    println!("🔧 DS Code CLI — Agent starting...\n");
    println!("📝 User: {}", &message);
    println!("🤖 Assistant: ");

    // Spawn agent execution
    let forge_handle = tokio::spawn({
        let forge = Arc::new(forge);
        let session_id = session.id.clone();
        async move {
            forge.execute(&message, &session_id, vec![], tx).await
        }
    });

    // Receive streaming events
    let mut had_error = false;
    let mut assistant_content = String::new();
    while let Some(event) = rx.recv().await {
        match event {
            StreamEvent::Thinking { content, .. } => {
                print!("\x1b[90m{}\x1b[0m", content);
            }
            StreamEvent::Token { content } => {
                print!("{}", content);
                assistant_content.push_str(&content);
            }
            StreamEvent::ToolStart { name, .. } => {
                println!("\n  🔧 Running: {}", name);
            }
            StreamEvent::ToolEnd { status, result, .. } => {
                let icon = match status {
                    dscode_core::agent::stream::ToolStatus::Success => "✅",
                    dscode_core::agent::stream::ToolStatus::Error => "❌",
                    dscode_core::agent::stream::ToolStatus::Running => "⏳",
                };
                println!("  {} Done ({} bytes)", icon, result.len());
            }
            StreamEvent::Fact { subject, predicate, object, .. } => {
                println!("  🧠 Fact: {} {} {}", subject, predicate, object);
            }
            StreamEvent::Error { content } => {
                eprintln!("\n❌ Error: {}", content);
                had_error = true;
            }
            StreamEvent::Complete { usage } => {
                if let Some(u) = usage {
                    println!(
                        "\n\n--- 📊 {:.1}K tokens (in: {:.1}K, out: {:.1}K) ---",
                        (u.input_tokens + u.output_tokens) as f64 / 1000.0,
                        u.input_tokens as f64 / 1000.0,
                        u.output_tokens as f64 / 1000.0,
                    );
                }
            }
            _ => {}
        }
    }

    // Persist assistant response
    if !assistant_content.is_empty() {
        session_manager.add_message(&session.id, &Message {
            role: Role::Assistant,
            content: MessageContent::Text(assistant_content),
            name: None, tool_calls: None, tool_call_id: None,
            reasoning_content: None, created_at: chrono::Utc::now().timestamp(),
        }).ok();
    }

    // Wait for forge to complete
    match forge_handle.await {
        Ok(Ok(())) => {
            if !had_error {
                println!("\n✅ Done");
            }
        }
        Ok(Err(e)) => {
            eprintln!("\n❌ Agent error: {}", e);
        }
        Err(e) => {
            eprintln!("\n❌ Join error: {}", e);
        }
    }

    Ok(())
}

//! Provider factory — construct the correct LLM backend from config + model id.

use super::anthropic::AnthropicProvider;
use super::openai::OpenAiProvider;
use super::trait_def::{LlmProvider, ProviderError};
use crate::config::settings::Config;

/// Create an [`LlmProvider`] for the given model using the app config.
///
/// Routing rules:
/// - `anthropic/*` or `claude-*` → native Anthropic Messages API
/// - everything else (DeepSeek / OpenAI / Ollama / custom) → OpenAI-compatible
pub fn create_provider(model: &str, conf: &Config) -> Result<Box<dyn LlmProvider>, ProviderError> {
    let pc = conf
        .provider_for_model(model)
        .ok_or(ProviderError::NoApiKey)?;

    if pc.api_key.trim().is_empty() && !model.starts_with("ollama/") {
        return Err(ProviderError::NoApiKey);
    }

    let is_anthropic = model.starts_with("anthropic/")
        || model.starts_with("claude-")
        || pc.base_url.contains("anthropic.com");

    if is_anthropic {
        Ok(Box::new(AnthropicProvider::from_config(model, conf)))
    } else {
        Ok(Box::new(OpenAiProvider::from_config(model, conf)))
    }
}

/// Clone-friendly helper: build two independent provider instances (primary + runtime).
/// Runtime model prefers `router_model` when configured and different.
pub fn create_provider_pair(
    model: &str,
    conf: &Config,
) -> Result<(Box<dyn LlmProvider>, Box<dyn LlmProvider>), ProviderError> {
    let primary = create_provider(model, conf)?;
    let runtime_model = if conf.router_model.is_empty() || conf.router_model == model {
        model
    } else {
        conf.router_model.as_str()
    };
    let runtime = match create_provider(runtime_model, conf) {
        Ok(p) => p,
        Err(_) => create_provider(model, conf)?,
    };
    Ok((primary, runtime))
}

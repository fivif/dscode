//! DeepSeek provider placeholder.
//!
//! DeepSeek is served by [`super::openai::OpenAiProvider`] (its API is
//! OpenAI-compatible, with the DeepSeek-specific `reasoning_effort` mapping
//! selected by base URL) or by [`super::responses::ResponsesProvider`] when the
//! channel sets `api_format = "responses"`. `factory::create_provider` never
//! constructs this type; it is kept only so existing imports keep compiling.
pub struct DeepSeekProvider;
impl DeepSeekProvider {
    pub fn new(_api_key: String, _model: String) -> Self { Self }
}

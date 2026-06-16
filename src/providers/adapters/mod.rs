pub mod anthropic;
pub mod generic;
pub mod openai;

pub use anthropic::AnthropicAdapter;
pub use generic::GenericAdapter;
pub use openai::OpenAiAdapter;

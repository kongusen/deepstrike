use compact_str::CompactString;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    pub provider: String,
    pub model_id: String,
    pub max_context_tokens: u32,
    pub max_output_tokens: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerationParams {
    #[serde(default = "default_temperature")]
    pub temperature: f64,
    #[serde(default)]
    pub top_p: Option<f64>,
    #[serde(default)]
    pub stop_sequences: Vec<CompactString>,
}

fn default_temperature() -> f64 {
    0.7
}

impl Default for GenerationParams {
    fn default() -> Self {
        Self { temperature: default_temperature(), top_p: None, stop_sequences: Vec::new() }
    }
}

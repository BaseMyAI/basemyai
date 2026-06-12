pub mod embedder;
pub mod llm;

pub use embedder::{
    BASELINE_DIM, BASELINE_MODEL_ID, HardwareProfile, ModelProvision, detect_hardware, provision,
    provision_with_progress,
};
pub use llm::{
    BackendKind, KNOWN_MODELS, KnownModel, LlmOption, LlmProvision, OllamaBackend, best_llm_option, choose_llm,
    detect_llm_options, propose_models_to_install,
};

//! Static catalog of well-known models per provider.
//!
//! Powers the TUI's `/models` command (providers with configured keys only).
//! The catalog is advisory: any model id can still be selected at runtime via
//! `/model [provider:]model` or `/models <provider> <model>` or the `--model` flag.

/// OpenAI models known to this build.
pub const OPENAI_MODELS: &[&str] = &["gpt-4.1", "gpt-4o", "gpt-4o-mini", "gpt-3.5-turbo"];

/// Anthropic Claude models.
pub const ANTHROPIC_MODELS: &[&str] = &[
    "claude-3-7-sonnet-20250219",
    "claude-3-5-sonnet",
    "claude-3-opus",
];

/// Google Gemini models (the `/models` picker refreshes these live).
pub const GOOGLE_MODELS: &[&str] = &[
    "gemini-3.8-flash",
    "gemini-3.7-flash",
    "gemini-3.6-flash",
    "gemini-3.5-flash",
    "gemini-3.5-flash-lite",
    "gemini-3.1-flash-lite",
    "gemini-2.5-flash",
    "gemini-2.5-flash-lite",
    "gemini-2.5-pro",
    "gemma-4-31b",
    "gemma-4-26b-a4b",
];

/// DeepSeek models.
pub const DEEPSEEK_MODELS: &[&str] = &["deepseek-chat", "deepseek-reasoner"];

/// OpenRouter models (`vendor/model` routing ids, including free tier models).
pub const OPENROUTER_MODELS: &[&str] = &[
    "anthropic/claude-3.5-sonnet",
    "openai/gpt-4o",
    "google/gemini-2.0-flash",
    "openrouter/free",
    "nvidia/nemotron-3-super-120b-a12b:free",
    "cohere/north-mini-code:free",
    "google/gemma-4-26b-a4b-it:free",
    "google/gemma-4-31b-it:free",
    "nvidia/nemotron-nano-9b-v2:free",
    "nvidia/nemotron-nano-12b-v2-vl:free",
    "poolside/laguna-s-2.1:free",
    "poolside/laguna-xs-2.1:free",
    "apodex/apodex-1.1-mini:free",
    "inclusionai/ling-3.0-flash-sante:free",
    "qwen/qwen3.8-27b:free",
    "dots-studio/dots-3-note-preview:free",
    "liquid/lfm-2.5-2.6b:free",
    "nvidia/nemotron-3.5-lightning:free",
    "thinkingmachines/inkling:free",
    "nvidia/nemotron-3-ultra-550b-a55b:free",
];

/// Qwen (DashScope) models.
pub const QWEN_MODELS: &[&str] = &["qwen-plus", "qwen-max", "qwen-turbo"];

/// Aion Labs models.
pub const AIONLABS_MODELS: &[&str] = &[
    "aion-labs/aion-2.0",
    "aion-labs/aion-rp-llama-3.1-8b",
    "aion-labs/aion-3.0",
    "aion-labs/aion-3.0-mini",
];

/// Cohere models.
pub const COHERE_MODELS: &[&str] = &[
    "command-r-plus",
    "command-r",
    "command-r7b",
    "command-a",
    "command-a-plus",
    "command-a-reasoning",
    "command-a-translate",
    "command-a-vision",
    "command-r7b-arabic",
    "aya-expanse-32b",
    "aya-vision-32b",
];

/// Mistral AI models.
pub const MISTRAL_MODELS: &[&str] = &[
    "mistral-small-latest",
    "mistral-medium-latest",
    "mistral-large-latest",
    "ministral-8b-latest",
    "codestral-latest",
    "ministral-3b-latest",
    "ministral-14b-latest",
    "mistral-small-4",
    "mistral-large-3",
    "mistral-medium-3.5",
    "codestral",
];

/// Z AI (Zhipu AI) models.
pub const ZAI_MODELS: &[&str] = &["glm-4.7-flash", "glm-4.5-flash", "glm-4.6v-flash"];

/// Groq models.
pub const GROQ_MODELS: &[&str] = &[
    "llama-3.3-70b-versatile",
    "llama-3.1-8b-instant",
    "openai/gpt-oss-120b",
    "openai/gpt-oss-20b",
    "qwen/qwen3.8-27b",
];

/// Cloudflare Workers AI models.
pub const CLOUDFLARE_MODELS: &[&str] = &[
    "@cf/meta/llama-3.3-70b-instruct-fp8-fast",
    "@cf/meta/llama-4-scout-17b-16e-instruct",
    "@cf/openai/gpt-oss-120b",
    "@cf/google/gemma-4-26b-a4b-it",
    "@cf/zai-org/glm-4.7-flash",
    "@cf/mistralai/mistral-small-3.1-24b-instruct",
    "@cf/deepseek-ai/deepseek-r1-distill-qwen-32b",
];

/// Hugging Face models.
pub const HUGGINGFACE_MODELS: &[&str] = &[
    "meta-llama/Meta-Llama-3.1-8B-Instruct",
    "google/gemma-3-4b-it",
    "microsoft/phi-4",
    "Qwen/Qwen2.5-Coder-7B-Instruct",
    "Qwen/Qwen2.5-7B-Instruct",
];

/// Kilo Code models.
pub const KILO_MODELS: &[&str] = &[
    "kilo-auto/free",
    "nvidia/nemotron-3-ultra-550b-a55b:free",
    "stepfun/step-3.7-flash:free",
    "nvidia/nemotron-3-super-120b-a12b:free",
    "nvidia/nemotron-3-nano-omni-30b-a3b-reasoning:free",
    "poolside/laguna-s-2.1:free",
    "poolside/laguna-xs-2.1:free",
    "cohere/north-mini-code:free",
    "openrouter/free",
    "tencent/hy3:free",
    "nvidia/nemotron-3.5-lightning:free",
    "liquid/lfm-2.5-2.6b:free",
    "inclusionai/ling-3.1-flash",
    "inclusionai/ling-3.0-flash-sante:free",
    "apodex/apodex-1.1-mini:free",
    "qwen/qwen3.8-27b:free",
    "dots-studio/dots-3-note-preview:free",
];

/// LLM7.io models.
pub const LLM7_MODELS: &[&str] = &[
    "gpt-oss:20b",
    "mistral-Nemo-Instruct-2407",
    "minimax-m2.7",
    "DeepSeek-V4-Flash-0731",
];

/// ModelScope models.
pub const MODELSCOPE_MODELS: &[&str] = &["Qwen/Qwen3.5-27B", "Qwen/Qwen3.5-35B-A3B"];

/// NVIDIA NIM models.
pub const NVIDIA_MODELS: &[&str] = &[
    "nvidia/nemotron-3-super-120b-a12b",
    "nvidia/llama-3.1-nemotron-ultra-253b-v1",
    "google/gemma-4-31b-it",
    "mistralai/mistral-large-2-instruct",
    "nvidia/nemotron-3-ultra-550b-a55b",
    "openai/gpt-oss-20b",
];

/// Ollama Cloud models.
pub const OLLAMA_MODELS: &[&str] = &[
    "deepseek-v4-flash",
    "deepseek-v4-pro",
    "minimax-m3",
    "kimi-k3",
    "gpt-oss:120b",
    "gpt-oss:20b",
    "nemotron-3-ultra",
    "mistral-large-3:675b",
    "qwen3.5:397b",
];

/// OVHcloud models.
pub const OVHCLOUD_MODELS: &[&str] = &[
    "Qwen3.5-397B-A17B",
    "gpt-oss-120b",
    "gpt-oss-20b",
    "Meta-Llama-3_3-70B-Instruct",
    "Qwen3.8-27B",
    "Qwen3.6-27B",
    "Qwen3.5-9B",
    "Qwen3-32B",
    "Qwen2.5-VL-72B-Instruct",
    "Mistral-Small-3.2-24B-Instruct",
    "Mistral-Nemo-Instruct-2407",
];

/// SiliconFlow models.
pub const SILICONFLOW_MODELS: &[&str] = &["Qwen/Qwen3-8B"];

/// Cerebras models.
pub const CEREBRAS_MODELS: &[&str] = &[
    "llama-3.3-70b",
    "llama-4-scout-17b-16e-instruct",
    "qwen-3-32b",
];

/// SambaNova models.
pub const SAMBANOVA_MODELS: &[&str] = &[
    "Meta-Llama-3.3-70B-Instruct",
    "DeepSeek-R1",
    "DeepSeek-V3-0324",
    "Qwen3-32B",
    "QwQ-32B",
];

/// Together AI models.
pub const TOGETHER_MODELS: &[&str] = &[
    "meta-llama/Llama-3.3-70B-Instruct-Turbo",
    "deepseek-ai/DeepSeek-R1",
    "Qwen/Qwen2.5-72B-Instruct-Turbo",
    "google/gemma-2-27b-it",
    "meta-llama/Meta-Llama-3.1-8B-Instruct-Turbo",
];

/// Chutes AI models.
pub const CHUTES_MODELS: &[&str] = &[
    "deepseek-ai/DeepSeek-V3-0324",
    "deepseek-ai/DeepSeek-R1",
    "Qwen/Qwen3-235B-A22B",
    "meta-llama/Llama-4-Maverick-17B-128E-Instruct",
];

/// GitHub Models.
pub const GITHUB_MODELS: &[&str] = &[
    "gpt-4o",
    "gpt-4o-mini",
    "Meta-Llama-3.1-405B-Instruct",
    "DeepSeek-R1",
    "Llama-3.3-70B-Instruct",
    "Phi-4",
];

/// Catalog of known models for `provider` (accepts aliases).
///
/// The built-in default model (see `default_model_for`) is listed first.
/// Returns an empty slice for providers without a catalog (e.g. custom
/// OpenAI-compatible endpoints).
pub fn known_models(provider: &str) -> &'static [&'static str] {
    match provider.trim().to_ascii_lowercase().as_str() {
        "openai" => OPENAI_MODELS,
        "anthropic" => ANTHROPIC_MODELS,
        "google" | "gemini" => GOOGLE_MODELS,
        "deepseek" => DEEPSEEK_MODELS,
        "openrouter" => OPENROUTER_MODELS,
        "qwen" => QWEN_MODELS,
        "aionlabs" | "aion-labs" | "aion" => AIONLABS_MODELS,
        "cohere" | "co" => COHERE_MODELS,
        "mistral" | "mistralai" | "mistral-ai" => MISTRAL_MODELS,
        "zai" | "zhipu" | "zhipuai" | "z-ai" => ZAI_MODELS,
        "groq" => GROQ_MODELS,
        "cloudflare" | "workers-ai" | "cf" => CLOUDFLARE_MODELS,
        "huggingface" | "hf" => HUGGINGFACE_MODELS,
        "kilo" | "kilo-code" => KILO_MODELS,
        "llm7" | "llm7.io" => LLM7_MODELS,
        "modelscope" => MODELSCOPE_MODELS,
        "nvidia" | "nim" | "nvidia-nim" => NVIDIA_MODELS,
        "ollama" | "ollama-cloud" => OLLAMA_MODELS,
        "ovhcloud" | "ovh" => OVHCLOUD_MODELS,
        "siliconflow" | "silicon-flow" => SILICONFLOW_MODELS,
        "cerebras" => CEREBRAS_MODELS,
        "sambanova" | "samba-nova" => SAMBANOVA_MODELS,
        "together" | "togetherai" | "together-ai" => TOGETHER_MODELS,
        "chutes" | "chutesai" | "chutes-ai" => CHUTES_MODELS,
        "github" | "github-models" => GITHUB_MODELS,
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covers_every_builtin_provider() {
        assert!(known_models("openai").contains(&"gpt-4.1"));
        assert!(known_models("anthropic").contains(&"claude-3-7-sonnet-20250219"));
        assert!(known_models("deepseek").contains(&"deepseek-chat"));
        assert!(!known_models("openrouter").is_empty());
        assert!(known_models("qwen").contains(&"qwen-plus"));
        assert!(known_models("aionlabs").contains(&"aion-labs/aion-2.0"));
        assert!(known_models("cohere").contains(&"command-r-plus"));
        assert!(known_models("mistral").contains(&"mistral-small-latest"));
        assert!(known_models("zai").contains(&"glm-4.7-flash"));
        assert!(known_models("groq").contains(&"llama-3.3-70b-versatile"));
        assert!(known_models("cerebras").contains(&"llama-3.3-70b"));
        assert!(known_models("sambanova").contains(&"Meta-Llama-3.3-70B-Instruct"));
        assert!(known_models("together").contains(&"meta-llama/Llama-3.3-70B-Instruct-Turbo"));
        assert!(known_models("chutes").contains(&"deepseek-ai/DeepSeek-V3-0324"));
        assert!(known_models("github").contains(&"gpt-4o"));
    }

    #[test]
    fn aliases_match() {
        assert_eq!(known_models("gemini"), known_models("google"));
        assert_eq!(known_models("aion-labs"), known_models("aionlabs"));
        assert_eq!(known_models("mistralai"), known_models("mistral"));
        assert_eq!(known_models("zhipu"), known_models("zai"));
        assert_eq!(known_models("nim"), known_models("nvidia"));
        assert_eq!(known_models("samba-nova"), known_models("sambanova"));
        assert_eq!(known_models("togetherai"), known_models("together"));
        assert_eq!(known_models("chutesai"), known_models("chutes"));
        assert_eq!(known_models("github-models"), known_models("github"));
    }

    #[test]
    fn default_models_are_listed_first() {
        // Keep in sync with `default_model_for` in agent-core.
        assert_eq!(known_models("openai")[0], "gpt-4.1");
        assert_eq!(known_models("anthropic")[0], "claude-3-7-sonnet-20250219");
        assert_eq!(known_models("google")[0], "gemini-3.8-flash");
        assert_eq!(known_models("deepseek")[0], "deepseek-chat");
        assert_eq!(known_models("openrouter")[0], "anthropic/claude-3.5-sonnet");
        assert_eq!(known_models("qwen")[0], "qwen-plus");
        assert_eq!(known_models("aionlabs")[0], "aion-labs/aion-2.0");
        assert_eq!(known_models("cohere")[0], "command-r-plus");
        assert_eq!(known_models("mistral")[0], "mistral-small-latest");
        assert_eq!(known_models("zai")[0], "glm-4.7-flash");
        assert_eq!(known_models("groq")[0], "llama-3.3-70b-versatile");
        assert_eq!(
            known_models("cloudflare")[0],
            "@cf/meta/llama-3.3-70b-instruct-fp8-fast"
        );
        assert_eq!(
            known_models("huggingface")[0],
            "meta-llama/Meta-Llama-3.1-8B-Instruct"
        );
        assert_eq!(known_models("kilo")[0], "kilo-auto/free");
        assert_eq!(known_models("llm7")[0], "gpt-oss:20b");
        assert_eq!(known_models("modelscope")[0], "Qwen/Qwen3.5-27B");
        assert_eq!(
            known_models("nvidia")[0],
            "nvidia/nemotron-3-super-120b-a12b"
        );
        assert_eq!(known_models("ollama")[0], "deepseek-v4-flash");
        assert_eq!(known_models("ovhcloud")[0], "Qwen3.5-397B-A17B");
        assert_eq!(known_models("siliconflow")[0], "Qwen/Qwen3-8B");
        assert_eq!(known_models("cerebras")[0], "llama-3.3-70b");
        assert_eq!(known_models("sambanova")[0], "Meta-Llama-3.3-70B-Instruct");
        assert_eq!(
            known_models("together")[0],
            "meta-llama/Llama-3.3-70B-Instruct-Turbo"
        );
        assert_eq!(known_models("chutes")[0], "deepseek-ai/DeepSeek-V3-0324");
        assert_eq!(known_models("github")[0], "gpt-4o");
    }

    #[test]
    fn unknown_providers_have_no_catalog() {
        assert!(known_models("my-local").is_empty());
        assert!(known_models("scripted").is_empty());
    }
}

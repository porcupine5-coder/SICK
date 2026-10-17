//! Live model listings via provider `/models` HTTP APIs.
//!
//! The TUI's `/models` picker starts from the static [`crate::catalog`] and
//! upgrades to these lists when they arrive, so users can pick from every
//! model a provider currently serves — not just well-known ids.

use std::time::Duration;

use crate::provider::ProviderError;

/// How long one provider lookup may take before giving up.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);

/// Fetch the model ids `provider_id` currently serves, using `api_key`.
///
/// `base_url` overrides the built-in endpoint (custom OpenAI-compatible
/// deployments). Returns an error when the provider has no known models
/// endpoint, the request fails, or the payload cannot be parsed; callers
/// fall back to the static catalog.
pub async fn fetch_models(
    provider_id: &str,
    api_key: &str,
    base_url: Option<&str>,
) -> Result<Vec<String>, ProviderError> {
    let provider = provider_id.trim().to_ascii_lowercase();
    let base = if let Some(base_url) = base_url {
        Some(base_url.trim().trim_end_matches('/').to_string())
    } else if matches!(provider.as_str(), "cloudflare" | "workers-ai" | "cf") {
        let account_id = std::env::var("CLOUDFLARE_ACCOUNT_ID").map_err(|_| {
            ProviderError::Other(
                "Cloudflare needs CLOUDFLARE_ACCOUNT_ID; set it to your 32-character account id \
                 or configure a full Cloudflare Workers AI base_url"
                    .to_string(),
            )
        })?;
        Some(cloudflare_base_url(&account_id)?)
    } else {
        default_base(&provider).map(str::to_string)
    };
    let Some(base) = base else {
        return Err(ProviderError::Other(format!(
            "no models endpoint known for provider `{provider_id}` \
             (set `base_url` for an OpenAI-compatible endpoint)"
        )));
    };

    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|error| ProviderError::Other(error.to_string()))?;

    let request = match provider.as_str() {
        "google" | "gemini" => client
            .get(format!("{base}/models"))
            .query(&[("key", api_key), ("pageSize", "200")]),
        "anthropic" => client
            .get(format!("{base}/v1/models"))
            .query(&[("limit", "1000")])
            .header("x-api-key", api_key)
            .header("anthropic-version", crate::anthropic::API_VERSION),
        _ => client.get(format!("{base}/models")).bearer_auth(api_key),
    };

    let response = request.send().await.map_err(|error| {
        ProviderError::Other(format!("{provider} models request failed: {error}"))
    })?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| ProviderError::Other(error.to_string()))?;
    if !status.is_success() {
        return Err(ProviderError::Other(format!(
            "{provider} models request returned HTTP {status}"
        )));
    }
    let json: serde_json::Value = serde_json::from_str(&body).map_err(|error| {
        ProviderError::Other(format!("unexpected {provider} models payload: {error}"))
    })?;
    Ok(parse_models(&provider, &json))
}

/// Build the Cloudflare Workers AI API URL from a Cloudflare account id.
pub fn cloudflare_base_url(account_id: &str) -> Result<String, ProviderError> {
    let account_id = account_id.trim();
    if account_id.len() != 32 || !account_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ProviderError::Other(
            "invalid CLOUDFLARE_ACCOUNT_ID: expected a 32-character hexadecimal account id"
                .to_string(),
        ));
    }
    Ok(format!(
        "https://api.cloudflare.com/client/v4/accounts/{account_id}/ai/v1"
    ))
}

/// Built-in models endpoint per provider (`None` = unknown, needs `base_url`).
fn default_base(provider: &str) -> Option<&'static str> {
    match provider {
        "openai" => Some(crate::openai::DEFAULT_BASE_URL),
        "anthropic" => Some(crate::anthropic::DEFAULT_BASE_URL),
        "google" | "gemini" => Some(crate::google::DEFAULT_BASE_URL),
        "openrouter" => Some(crate::openrouter::DEFAULT_BASE_URL),
        "deepseek" => Some("https://api.deepseek.com/v1"),
        "qwen" => Some("https://dashscope.aliyuncs.com/compatible-mode/v1"),
        "aionlabs" | "aion-labs" | "aion" => Some("https://api.aionlabs.ai/v1"),
        "cohere" => Some("https://api.cohere.com/v2"),
        "mistral" | "mistralai" | "mistral-ai" => Some("https://api.mistral.ai/v1"),
        "zai" | "zhipu" | "zhipuai" | "z-ai" => Some("https://open.bigmodel.cn/api/paas/v4"),
        "groq" => Some("https://api.groq.com/openai/v1"),
        "huggingface" | "hf" => Some("https://router.huggingface.co/v1"),
        "kilo" | "kilo-code" => Some("https://api.kilo.ai/api/gateway"),
        "llm7" | "llm7.io" => Some("https://api.llm7.io/v1"),
        "modelscope" => Some("https://api-inference.modelscope.cn/v1"),
        "nvidia" | "nim" | "nvidia-nim" => Some("https://integrate.api.nvidia.com/v1"),
        "ollama" | "ollama-cloud" => Some("https://ollama.com/v1"),
        "ovhcloud" | "ovh" => Some("https://oai.endpoints.kepler.ai.cloud.ovh.net/v1"),
        "siliconflow" | "silicon-flow" => Some("https://api.siliconflow.cn/v1"),
        "cerebras" => Some("https://api.cerebras.ai/v1"),
        "sambanova" | "samba-nova" => Some("https://api.sambanova.ai/v1"),
        "together" | "togetherai" | "together-ai" => Some("https://api.together.xyz/v1"),
        "chutes" | "chutesai" | "chutes-ai" => Some("https://api.chutes.ai/v1"),
        "github" | "github-models" => Some("https://models.inference.ai.azure.com"),
        _ => None,
    }
}

/// Extract, normalize, filter, and sort model ids from a list payload.
///
/// OpenAI-style providers use `data[].id`; Gemini uses `models[].name` with a
/// `models/` prefix and a `supportedGenerationMethods` capability list.
fn parse_models(provider: &str, json: &serde_json::Value) -> Vec<String> {
    let google = matches!(provider, "google" | "gemini");
    let entries = if google {
        json.get("models")
    } else {
        json.get("data")
    }
    .and_then(serde_json::Value::as_array);
    let Some(entries) = entries else {
        return Vec::new();
    };

    let mut models: Vec<String> = entries
        .iter()
        .filter(|entry| !google || supports_generate_content(entry))
        .filter_map(|entry| model_id(entry, google))
        .filter(|id| is_chat_model(id))
        .collect();
    models.sort();
    models.dedup();
    models
}

/// Model id of one list entry (`id` for OpenAI-style, `name` for Gemini).
fn model_id(entry: &serde_json::Value, google: bool) -> Option<String> {
    if google {
        let raw = entry.get("name")?.as_str()?;
        Some(raw.strip_prefix("models/").unwrap_or(raw).to_string())
    } else {
        Some(entry.get("id")?.as_str()?.to_string())
    }
}

/// Whether a Gemini entry can answer `generateContent` requests.
///
/// Entries without the capability list are kept (older API revisions).
fn supports_generate_content(entry: &serde_json::Value) -> bool {
    match entry
        .get("supportedGenerationMethods")
        .and_then(serde_json::Value::as_array)
    {
        Some(methods) => methods
            .iter()
            .any(|method| method.as_str() == Some("generateContent")),
        None => true,
    }
}

/// Crude filter for models a chat picker should offer.
fn is_chat_model(id: &str) -> bool {
    const EXCLUDED: &[&str] = &[
        "audio",
        "banana",
        "dall-e",
        "embed",
        "image",
        "lyria",
        "moderation",
        "robotics",
        "transcribe",
        "tts",
        "whisper",
    ];
    let lower = id.to_ascii_lowercase();
    !EXCLUDED.iter().any(|needle| lower.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_openai_style_ids_and_drops_non_chat_models() {
        let payload = json!({"data": [
            {"id": "gpt-4.1"},
            {"id": "text-embedding-3-small"},
            {"id": "whisper-1"},
        ]});
        assert_eq!(parse_models("openai", &payload), vec!["gpt-4.1"]);
    }

    #[test]
    fn google_strips_prefix_and_requires_generate_content() {
        let payload = json!({"models": [
            {"name": "models/gemini-2.0-flash", "supportedGenerationMethods": ["generateContent"]},
            {"name": "models/text-embedding-004", "supportedGenerationMethods": ["embedContent"]},
            {"name": "models/gemini-1.5-pro"},
            {"name": "gemini-3.8-flash", "supportedGenerationMethods": ["generateContent"]},
            {"name": "models/nano-banana-pro-preview", "supportedGenerationMethods": ["generateContent"]},
            {"name": "models/lyria-3-pro-preview", "supportedGenerationMethods": ["generateContent"]},
        ]});
        assert_eq!(
            parse_models("google", &payload),
            vec!["gemini-1.5-pro", "gemini-2.0-flash", "gemini-3.8-flash"]
        );
    }

    #[test]
    fn sorts_and_dedupes() {
        let payload = json!({"data": [{"id": "b-model"}, {"id": "a-model"}, {"id": "a-model"}]});
        assert_eq!(
            parse_models("deepseek", &payload),
            vec!["a-model", "b-model"]
        );
    }

    #[test]
    fn missing_lists_yield_nothing() {
        assert!(parse_models("openai", &json!({"error": "nope"})).is_empty());
    }

    #[test]
    fn cloudflare_base_requires_and_uses_account_id() {
        assert_eq!(
            cloudflare_base_url("0123456789abcdef0123456789abcdef").unwrap(),
            "https://api.cloudflare.com/client/v4/accounts/0123456789abcdef0123456789abcdef/ai/v1"
        );
        assert!(
            cloudflare_base_url("{account_id}")
                .unwrap_err()
                .to_string()
                .contains("32-character")
        );
    }

    #[tokio::test]
    async fn unknown_provider_without_base_fails_without_network() {
        let error = fetch_models("mystery", "key", None).await.unwrap_err();
        assert!(error.to_string().contains("no models endpoint"));
    }
}

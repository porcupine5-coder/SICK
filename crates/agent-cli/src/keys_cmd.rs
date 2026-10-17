//! `sick keys add` — interactive provider picker + API key paste.
//!
//! Flow: choose a provider (numbered picker or the `provider` argument), paste
//! the key with hidden input, validate it with one minimal request, then save
//! it to the encrypted `<SICK_HOME>/keys.enc` (preserving existing entries;
//! plaintext `keys.toml` / `keys.json` files are migrated to `*.bak`).

use std::io::{IsTerminal, Write};

use shell_agent_core::{
    AgentConfig, KeyValidation, cloudflare_base_url, resolve_provider, validate_api_key,
    validation_model,
};
use shell_key_manager::{
    KeyError, KeyStore, add_key_secure, add_key_secure_with_base_url, normalize_provider,
};

/// Providers offered by the picker (id, display label).
pub(crate) const SETUP_PROVIDERS: &[(&str, &str)] = &[
    ("openai", "OpenAI"),
    ("anthropic", "Anthropic Claude"),
    ("google", "Google Gemini"),
    ("deepseek", "DeepSeek"),
    ("openrouter", "OpenRouter"),
    ("qwen", "Qwen (DashScope)"),
    ("aionlabs", "Aion Labs"),
    ("cohere", "Cohere"),
    ("mistral", "Mistral AI"),
    ("zai", "Z AI (Zhipu AI)"),
    ("groq", "Groq"),
    ("cloudflare", "Cloudflare Workers AI"),
    ("huggingface", "Hugging Face"),
    ("kilo", "Kilo Code"),
    ("llm7", "LLM7.io"),
    ("modelscope", "ModelScope"),
    ("nvidia", "NVIDIA NIM"),
    ("ollama", "Ollama Cloud"),
    ("ovhcloud", "OVHcloud AI"),
    ("siliconflow", "SiliconFlow"),
];

/// Run the `keys add` flow.
pub(crate) async fn add(provider: Option<String>, no_validate: bool) -> anyhow::Result<()> {
    if !std::io::stdin().is_terminal() {
        eprintln!("sick: `keys add` needs a terminal (the key prompt is hidden while you paste).");
        eprintln!(
            "hint: set an env var instead, e.g. SICK_OPENAI_API_KEY=... or OPENAI_API_KEY=..."
        );
        std::process::exit(2);
    }

    let provider = match provider {
        Some(provider) => normalize_provider(&provider),
        None => prompt_provider()?,
    };

    let prompt = format!("Paste the {provider} API key (input hidden, Enter to confirm): ");
    let secret = rpassword::prompt_password(&prompt)?.trim().to_string();
    if secret.is_empty() {
        anyhow::bail!("no key entered — nothing was saved");
    }

    let account_base_url = if provider == "cloudflare" {
        let account_id = rpassword::prompt_password(
            "Cloudflare account ID (32 hex characters, not token ID): ",
        )?;
        Some(cloudflare_base_url(&account_id)?)
    } else {
        None
    };

    if !no_validate {
        print!("Validating the key... ");
        std::io::stdout().flush()?;
        match validate(&provider, &secret, account_base_url.as_deref()).await {
            Ok(KeyValidation::Verified) => println!("✓ verified"),
            Ok(KeyValidation::Unverified(reason)) => {
                println!("⚠ could not verify ({reason}) — saving anyway");
            }
            Err(error) => anyhow::bail!("{error}\n  the key was NOT saved"),
        }
    }

    let save_key = |password| match &account_base_url {
        Some(base_url) => {
            add_key_secure_with_base_url(&provider, &secret, Some(base_url), password)
        }
        None => add_key_secure(&provider, &secret, password),
    };
    let report = match save_key(None) {
        Ok(report) => report,
        Err(KeyError::PasswordRequired) | Err(KeyError::KeychainUnavailable { .. }) => {
            // No OS keychain and no SICK_KEY_PASSWORD: unlock (or create) the
            // password-protected keys file interactively.
            let encrypted_exists = shell_key_manager::default_encrypted_path().is_file();
            let password = if encrypted_exists {
                rpassword::prompt_password("Master password for the keys.enc file: ")?
            } else {
                crate::prompt_new_password()?
            };
            save_key(Some(&password))?
        }
        Err(error) => return Err(error.into()),
    };
    let mask: String = secret.chars().take(8).collect();
    if report.added {
        println!(
            "✓ added {provider} key {mask}… to {} ({})",
            report.save.path.display(),
            report.save.source.describe()
        );
    } else {
        println!(
            "• {provider} key {mask}… is already in {}",
            report.save.path.display()
        );
    }
    for migrated in &report.save.migrated {
        println!("Plaintext keys file migrated to {}", migrated.display());
    }
    Ok(())
}

/// Validate `secret` with one minimal request. `Err` only when the provider
/// explicitly rejected the key (401/403).
async fn validate(
    provider: &str,
    secret: &str,
    base_url: Option<&str>,
) -> anyhow::Result<KeyValidation> {
    let mut store = KeyStore::new();
    // Let `base_url` / `default_model` entries from the keys file apply.
    let _ = store.load_default_file();
    if let Some(base_url) = base_url {
        store.set_provider_base_url(provider, base_url);
    }
    let model = validation_model(&store, provider);
    let config = AgentConfig::new(provider, model.clone());
    let provider = resolve_provider(&config, &store, provider)?;
    validate_api_key(&provider, &model, secret)
        .await
        .map_err(Into::into)
}

/// Numbered provider picker; empty input cancels. Re-prompts on bad input.
fn prompt_provider() -> anyhow::Result<String> {
    println!("Choose a provider for the new API key:");
    for (index, (id, label)) in SETUP_PROVIDERS.iter().enumerate() {
        println!("  {})  {label}  ({id})", index + 1);
    }
    loop {
        print!("provider [1-{}] (Enter cancels): ", SETUP_PROVIDERS.len());
        std::io::stdout().flush()?;
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line)? == 0 {
            anyhow::bail!("no provider selected");
        }
        match parse_provider_choice(&line) {
            Ok(Some(id)) => return Ok(id.to_string()),
            Ok(None) => anyhow::bail!("no provider selected"),
            Err(message) => eprintln!("{message}"),
        }
    }
}

/// Parse one picker response: an index (`2`), a provider id (`anthropic`) or
/// alias (`gemini`). `Ok(None)` means the input was empty (cancel).
fn parse_provider_choice(input: &str) -> Result<Option<&'static str>, String> {
    let choice = input.trim().to_ascii_lowercase();
    if choice.is_empty() {
        return Ok(None);
    }
    if let Ok(index) = choice.parse::<usize>() {
        if (1..=SETUP_PROVIDERS.len()).contains(&index) {
            return Ok(Some(SETUP_PROVIDERS[index - 1].0));
        }
        return Err(format!(
            "please pick a number between 1 and {} (or a provider id)",
            SETUP_PROVIDERS.len()
        ));
    }
    let normalized = normalize_provider(&choice);
    match SETUP_PROVIDERS.iter().find(|(id, _)| *id == normalized) {
        Some((id, _)) => Ok(Some(id)),
        None => Err(format!(
            "unknown provider `{choice}` — pick one of: {}",
            SETUP_PROVIDERS
                .iter()
                .map(|(id, _)| *id)
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_choice_accepts_index_name_and_alias() {
        assert_eq!(parse_provider_choice("1").unwrap(), Some("openai"));
        assert_eq!(parse_provider_choice(" 4 \n").unwrap(), Some("deepseek"));
        assert_eq!(
            parse_provider_choice("Anthropic").unwrap(),
            Some("anthropic")
        );
        assert_eq!(parse_provider_choice("gemini").unwrap(), Some("google"));
        assert_eq!(parse_provider_choice("").unwrap(), None);
        assert_eq!(parse_provider_choice("   \n").unwrap(), None);
    }

    #[test]
    fn provider_choice_rejects_bad_input() {
        assert!(parse_provider_choice("99").is_err());
        assert!(parse_provider_choice("0").is_err());
        assert!(parse_provider_choice("nope").is_err());
    }
}

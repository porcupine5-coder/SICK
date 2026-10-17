//! Skills: user-authored instruction packs stored as
//! `<SICK_HOME>/skills/<name>/SKILL.md`.
//!
//! A skill file starts with an optional front-matter block:
//!
//! ```markdown
//! ---
//! name: rust-review
//! description: Review Rust code for correctness and style.
//! allowed-tools: read, grep
//! ---
//! Body: the instructions the agent should follow.
//! ```
//!
//! Skills can be activated for a session (`/skill <name>` in the TUI, which
//! injects the body into the system prompt of every subsequent turn) or
//! requested by the model mid-turn through the `skill` tool.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::{Value, json};
use shell_common::{ShellError, ShellResult, sick_home};
use shell_tools::{Tool, ToolContext, ToolResult};

/// A loaded skill (front matter + body).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    /// Skill name (front matter, or the containing directory name).
    pub name: String,
    /// One-line description shown to the model.
    pub description: String,
    /// Tools the skill recommends (`allowed-tools` front matter).
    pub allowed_tools: Vec<String>,
    /// Instruction body injected into the system prompt once active.
    pub body: String,
    /// Path of the `SKILL.md` file it was loaded from.
    pub path: PathBuf,
}

/// Default skills directory: `<SICK_HOME>/skills`.
pub fn skills_dir() -> PathBuf {
    sick_home().join("skills")
}

/// Load a skill by name from the default skills directory.
pub fn load_skill(name: &str) -> ShellResult<Skill> {
    load_skill_from(&skills_dir(), name)
}

/// Load a skill by name from `dir` (`dir/<name>/SKILL.md`).
pub fn load_skill_from(dir: &Path, name: &str) -> ShellResult<Skill> {
    validate_skill_name(name)?;
    let path = dir.join(name).join("SKILL.md");
    let raw = std::fs::read_to_string(&path).map_err(|error| {
        ShellError::Config(format!(
            "could not read skill file {}: {error}",
            path.display()
        ))
    })?;
    let mut skill = parse_skill_md(&raw)?;
    if skill.name.trim().is_empty() {
        skill.name = name.to_string();
    }
    skill.path = path;
    Ok(skill)
}

/// Parse `SKILL.md` content (front matter optional).
pub fn parse_skill_md(raw: &str) -> ShellResult<Skill> {
    let normalized = raw.replace("\r\n", "\n");
    let mut skill = Skill {
        name: String::new(),
        description: String::new(),
        allowed_tools: Vec::new(),
        body: normalized.trim().to_string(),
        path: PathBuf::new(),
    };

    if let Some(rest) = normalized.trim_start().strip_prefix("---\n")
        && let Some(end) = rest.find("\n---")
    {
        let front = &rest[..end];
        let body = &rest[end + 4..];
        skill.body = body.trim_start_matches('\n').trim_end().to_string();
        for line in front.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let value = value.trim().trim_matches('"').trim();
            match key.trim().to_ascii_lowercase().as_str() {
                "name" => skill.name = value.to_string(),
                "description" => skill.description = value.to_string(),
                "allowed-tools" | "allowed_tools" | "tools" => {
                    skill.allowed_tools = value
                        .split(',')
                        .map(|tool| tool.trim().to_string())
                        .filter(|tool| !tool.is_empty())
                        .collect();
                }
                _ => {}
            }
        }
    }

    Ok(skill)
}

/// Load every valid skill from the default directory (sorted by name).
///
/// Unreadable or malformed skills are skipped with a warning so a broken file
/// never blocks a turn.
pub fn list_skills() -> Vec<Skill> {
    list_skills_from(&skills_dir())
}

/// Load every valid skill from `dir` (sorted by name).
pub fn list_skills_from(dir: &Path) -> Vec<Skill> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    names.sort();
    names
        .iter()
        .filter_map(|name| match load_skill_from(dir, name) {
            Ok(skill) => Some(skill),
            Err(error) => {
                tracing::warn!(skill = %name, error = %error, "skipping unreadable skill");
                None
            }
        })
        .collect()
}

/// Scaffold a template skill in the default directory; returns the new path.
pub fn scaffold_skill(name: &str) -> ShellResult<PathBuf> {
    scaffold_skill_in(&skills_dir(), name)
}

/// Scaffold a template skill in `dir`; `dir/<name>/SKILL.md`.
pub fn scaffold_skill_in(dir: &Path, name: &str) -> ShellResult<PathBuf> {
    validate_skill_name(name)?;
    let skill_dir = dir.join(name);
    let path = skill_dir.join("SKILL.md");
    if path.exists() {
        return Err(ShellError::Config(format!(
            "skill `{name}` already exists at {}",
            path.display()
        )));
    }
    std::fs::create_dir_all(&skill_dir).map_err(|error| {
        ShellError::Config(format!(
            "could not create skill directory {}: {error}",
            skill_dir.display()
        ))
    })?;
    std::fs::write(&path, skill_template(name)).map_err(|error| {
        ShellError::Config(format!("could not write {}: {error}", path.display()))
    })?;
    Ok(path)
}

/// Reject names that are empty or could escape the skills directory.
pub fn validate_skill_name(name: &str) -> ShellResult<()> {
    let name = name.trim();
    if name.is_empty() {
        return Err(ShellError::Config(
            "skill name must be non-empty".to_string(),
        ));
    }
    if name == "." || name == ".." {
        return Err(ShellError::Config(format!("invalid skill name `{name}`")));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return Err(ShellError::Config(format!(
            "invalid skill name `{name}` (use letters, digits, `-`, `_`, `.`)"
        )));
    }
    Ok(())
}

fn skill_template(name: &str) -> String {
    format!(
        "---\n\
         name: {name}\n\
         description: Describe when the agent should use this skill.\n\
         # allowed-tools: read, grep, glob, ls\n\
         ---\n\
         \n\
         # {name}\n\
         \n\
         Write the instructions the agent should follow when this skill is active.\n"
    )
}

/// The `skill` tool: lets the model load a skill body mid-turn.
pub struct SkillTool {
    dir: PathBuf,
}

impl SkillTool {
    /// Tool serving skills from `dir`.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }
}

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &str {
        "skill"
    }

    fn description(&self) -> &str {
        "Load a skill's full instructions by name. Skills are reusable \
         instruction packs; the available ones are listed in the system prompt."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Skill name, as listed in the system prompt."
                }
            },
            "required": ["name"]
        })
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let Some(name) = args.get("name").and_then(Value::as_str).map(str::trim) else {
            return ToolResult::err("missing required string argument `name`");
        };
        match load_skill_from(&self.dir, name) {
            Ok(skill) => ToolResult::ok(format!(
                "Skill `{}` loaded — follow these instructions:\n\n{}",
                skill.name, skill.body
            )),
            Err(error) => ToolResult::err(error.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_front_matter_and_body() {
        let raw = "---\nname: rust-review\ndescription: Review Rust code.\n\
                   allowed-tools: read, grep\n---\n\nStep 1: read the diff.\n";
        let skill = parse_skill_md(raw).unwrap();
        assert_eq!(skill.name, "rust-review");
        assert_eq!(skill.description, "Review Rust code.");
        assert_eq!(skill.allowed_tools, vec!["read", "grep"]);
        assert_eq!(skill.body, "Step 1: read the diff.");
    }

    #[test]
    fn parses_without_front_matter() {
        let skill = parse_skill_md("Just a body.").unwrap();
        assert!(skill.name.is_empty());
        assert!(skill.description.is_empty());
        assert!(skill.allowed_tools.is_empty());
        assert_eq!(skill.body, "Just a body.");
    }

    #[test]
    fn handles_crlf_and_comments() {
        let raw = "---\r\n# comment\r\nname: windows\r\n---\r\nBody line.\r\n";
        let skill = parse_skill_md(raw).unwrap();
        assert_eq!(skill.name, "windows");
        assert_eq!(skill.body, "Body line.");
    }

    #[test]
    fn rejects_bad_names() {
        assert!(validate_skill_name("").is_err());
        assert!(validate_skill_name("..").is_err());
        assert!(validate_skill_name("a/b").is_err());
        assert!(validate_skill_name("ok-name_1.2").is_ok());
    }

    #[test]
    fn load_missing_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let error = load_skill_from(dir.path(), "nope").unwrap_err();
        assert!(error.to_string().contains("SKILL.md"), "{error}");
    }

    #[test]
    fn scaffold_creates_a_loadable_template() {
        let dir = tempfile::tempdir().unwrap();
        let path = scaffold_skill_in(dir.path(), "my-skill").unwrap();
        assert!(path.exists());
        // Scaffolding twice fails (no silent overwrite).
        assert!(scaffold_skill_in(dir.path(), "my-skill").is_err());

        let skill = load_skill_from(dir.path(), "my-skill").unwrap();
        assert_eq!(skill.name, "my-skill");
        assert!(!skill.description.is_empty());
        assert!(skill.body.contains("my-skill"));

        let listed = list_skills_from(dir.path());
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "my-skill");
    }

    #[tokio::test]
    async fn skill_tool_returns_body_and_errors_for_unknown() {
        let dir = tempfile::tempdir().unwrap();
        scaffold_skill_in(dir.path(), "helper").unwrap();
        let tool = SkillTool::new(dir.path());
        let ctx = ToolContext::new(dir.path());

        let ok = tool.execute(json!({"name": "helper"}), &ctx).await;
        assert!(ok.success, "{}", ok.output);
        assert!(ok.output.contains("helper"));

        let missing = tool.execute(json!({"name": "nope"}), &ctx).await;
        assert!(!missing.success);
        assert!(missing.output.contains("nope"));

        let bad = tool.execute(json!({}), &ctx).await;
        assert!(!bad.success);
    }
}

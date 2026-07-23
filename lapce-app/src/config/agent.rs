//! ACP agent-server configuration (the `[acp]` section of `settings.toml`).
//!
//! Mirrors crow-ade's `acp.agents` / `acp.defaultAgent`: a list of named ACP
//! agent servers (each a `command` + `args`/`env`/`cwd`) plus the name of the
//! one to spawn by default. The chat picks the configured default agent when it
//! creates a session, instead of a hard-coded `crow-cli acp`.
//!
//! The field names intentionally match crow-ade's `AgentConfig`
//! (`name`/`command`/`args`/`env`/`cwd`) so an agent entry copied from a
//! crow-ade `settings.json` ports across with only the section/casing changed
//! (lapce settings are TOML and `kebab-case`, so the section is `[acp]` and the
//! default-name key is `default-agent`).

use serde::{Deserialize, Serialize};

/// Default agent name used when `settings.toml` has no `[acp]` section (or no
/// `default-agent` key that matches a listed agent).
pub const DEFAULT_AGENT_NAME: &str = "crow-cli";

/// A single configured ACP agent server. Shape matches crow-ade's
/// `AgentConfig` so entries are portable between the two.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub struct AcpAgentConfig {
    /// Human-readable name (e.g. `"crow-cli"`, `"claude-code"`). Referenced by
    /// `default-agent` and shown in the (future) agent picker.
    pub name: String,
    /// Executable to spawn (resolved via `PATH` or an absolute path).
    pub command: String,
    /// Arguments passed to the command (e.g. `["acp"]`).
    #[serde(default)]
    pub args: Vec<String>,
    /// Extra environment variables as `KEY=VALUE` strings.
    #[serde(default)]
    pub env: Vec<String>,
    /// Working directory for the agent. When absent, the workspace root is used.
    #[serde(default)]
    pub cwd: Option<String>,
}

impl AcpAgentConfig {
    /// The built-in fallback agent: `crow-cli acp`. Used to seed the default
    /// config and as a last resort if a user's `acp.agents` is empty.
    pub fn builtin_crow_cli() -> Self {
        Self {
            name: DEFAULT_AGENT_NAME.to_string(),
            command: DEFAULT_AGENT_NAME.to_string(),
            args: vec!["acp".to_string()],
            env: Vec::new(),
            cwd: None,
        }
    }
}

/// The `[acp]` settings section.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub struct AcpConfig {
    /// Name of the agent (from `agents`) to spawn by default.
    #[serde(default)]
    pub default_agent: String,
    /// The configured agent servers. When empty, the built-in `crow-cli` agent
    /// is used (see [`AcpConfig::default`]).
    #[serde(default)]
    pub agents: Vec<AcpAgentConfig>,
}

impl Default for AcpConfig {
    fn default() -> Self {
        Self {
            default_agent: DEFAULT_AGENT_NAME.to_string(),
            agents: vec![AcpAgentConfig::builtin_crow_cli()],
        }
    }
}

impl AcpConfig {
    /// Resolve the agent to spawn: the one whose `name` matches `default_agent`,
    /// else the first listed agent, else the built-in `crow-cli` agent (so we
    /// never hand the proxy an empty command, even on a misconfigured/empty
    /// `acp.agents`).
    pub fn selected_agent(&self) -> AcpAgentConfig {
        self.agents
            .iter()
            .find(|a| a.name == self.default_agent)
            .or_else(|| self.agents.first())
            .cloned()
            .unwrap_or_else(AcpAgentConfig::builtin_crow_cli)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_seeds_builtin_agent() {
        let cfg = AcpConfig::default();
        assert_eq!(cfg.default_agent, "crow-cli");
        assert_eq!(cfg.agents.len(), 1);
        assert_eq!(cfg.agents[0], AcpAgentConfig::builtin_crow_cli());
        assert_eq!(cfg.selected_agent().command, "crow-cli");
    }

    #[test]
    fn selected_agent_prefers_default_name_then_first_then_builtin() {
        let a = AcpAgentConfig {
            name: "alpha".into(),
            command: "alpha-bin".into(),
            args: vec![],
            env: vec![],
            cwd: None,
        };
        let b = AcpAgentConfig {
            name: "beta".into(),
            command: "beta-bin".into(),
            args: vec!["acp".into()],
            env: vec![],
            cwd: Some("/tmp".into()),
        };
        // default-agent matches a listed agent → that one.
        let cfg = AcpConfig {
            default_agent: "beta".into(),
            agents: vec![a.clone(), b.clone()],
        };
        assert_eq!(cfg.selected_agent(), b);
        // default-agent matches nothing → first listed.
        let cfg = AcpConfig {
            default_agent: "missing".into(),
            agents: vec![a.clone(), b.clone()],
        };
        assert_eq!(cfg.selected_agent(), a);
        // empty list → built-in fallback (never an empty command).
        let cfg = AcpConfig {
            default_agent: "whatever".into(),
            agents: vec![],
        };
        assert_eq!(cfg.selected_agent(), AcpAgentConfig::builtin_crow_cli());
    }

    #[test]
    fn deserializes_from_toml_like_settings_file() {
        // The `config` crate parses the real settings.toml; deserialize a
        // representative `[acp]` table the same way to prove the field/alias
        // names line up (kebab-case section keys, crow-ade field names).
        let toml = r#"
default-agent = "claude-code"

[[agents]]
name = "crow-cli"
command = "crow-cli"
args = ["acp"]
env = []

[[agents]]
name = "claude-code"
command = "claude"
args = ["--acp"]
env = ["KEY=VALUE"]
cwd = "/home/me/project"
"#;
        let cfg: AcpConfig = config::Config::builder()
            .add_source(config::File::from_str(
                toml,
                config::FileFormat::Toml,
            ))
            .build()
            .unwrap()
            .try_deserialize()
            .unwrap();
        assert_eq!(cfg.default_agent, "claude-code");
        assert_eq!(cfg.agents.len(), 2);
        assert_eq!(cfg.agents[0].name, "crow-cli");
        assert_eq!(cfg.agents[0].args, vec!["acp".to_string()]);
        assert_eq!(cfg.agents[1].command, "claude");
        assert_eq!(cfg.agents[1].env, vec!["KEY=VALUE".to_string()]);
        assert_eq!(cfg.agents[1].cwd.as_deref(), Some("/home/me/project"));
        // selection honours default-agent
        assert_eq!(cfg.selected_agent().name, "claude-code");
    }

    #[test]
    fn absent_acp_section_yields_builtin_default() {
        // The real guarantee: when the settings tree has NO `[acp]` table, the
        // `#[serde(default)]` on the *parent* struct's `acp` field fills in
        // `AcpConfig::default()` (the built-in agent) — exactly as `LapceConfig`
        // does it (and as `color_theme` does today). Deserializing `AcpConfig`
        // as the *root* would instead apply each field's own default, so we
        // mirror the production shape with a parent wrapper here.
        #[derive(Deserialize)]
        struct Wrapper {
            #[serde(default)]
            acp: AcpConfig,
            #[allow(dead_code)]
            other: i32,
        }
        let w: Wrapper = config::Config::builder()
            .add_source(config::File::from_str(
                "other = 5\n",
                config::FileFormat::Toml,
            ))
            .build()
            .unwrap()
            .try_deserialize()
            .unwrap();
        assert_eq!(w.acp, AcpConfig::default());
        assert_eq!(w.acp.selected_agent().command, "crow-cli");
    }

    #[test]
    fn shipped_default_settings_deserializes_with_builtin_agent() {
        // The shipped `defaults/settings.toml` has no `[acp]` table. This is the
        // exact source `LapceConfig::default_config()` feeds the startup
        // `.expect(...)` deserialization, so it MUST succeed and yield the
        // built-in agent via `#[serde(default)]`.
        let shipped = include_str!("../../../defaults/settings.toml");
        let cfg: crate::config::LapceConfig = config::Config::builder()
            .add_source(config::File::from_str(
                shipped,
                config::FileFormat::Toml,
            ))
            .build()
            .unwrap()
            .try_deserialize()
            .expect("shipped default settings must deserialize into LapceConfig");
        assert_eq!(cfg.acp, AcpConfig::default());
        assert_eq!(cfg.acp.selected_agent().name, "crow-cli");
    }
}

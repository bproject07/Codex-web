use std::sync::Arc;

use futures_util::future::join_all;
use serde::Serialize;

use crate::{
    config::{AgentKind, Config, ShellKind},
    terminal::{self, CommandInspectionState, TerminalConfig},
};

const CATALOG_SCHEMA_VERSION: u8 = 1;

#[derive(Clone)]
pub struct AgentCatalog {
    profiles: Arc<Vec<CatalogProfile>>,
    server_shell: &'static str,
}

#[derive(Clone)]
struct CatalogProfile {
    terminal: TerminalConfig,
    explicit_override: bool,
    dangerously_skip_permissions: bool,
}

pub struct AgentProfiles {
    pub primary: TerminalConfig,
    pub new_session: TerminalConfig,
    pub additional: Vec<TerminalConfig>,
    pub catalog: AgentCatalog,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentCatalogState {
    Ready,
    Missing,
    Misconfigured,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentConfiguration {
    Auto,
    Override,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCatalogResponse {
    schema_version: u8,
    server: AgentCatalogServer,
    agents: Vec<AgentCatalogEntry>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentCatalogServer {
    os: &'static str,
    arch: &'static str,
    shell: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentCatalogEntry {
    kind: AgentKind,
    state: AgentCatalogState,
    configuration: AgentConfiguration,
    version: Option<String>,
    dangerously_skip_permissions: bool,
    install: AgentInstallInstructions,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentInstallInstructions {
    command: &'static str,
    shell: &'static str,
    verify_command: &'static str,
    update_command: &'static str,
    docs_url: &'static str,
    requires_server_access: bool,
}

impl AgentCatalog {
    pub async fn snapshot(&self) -> AgentCatalogResponse {
        let profiles = self.profiles.as_ref().clone();
        let tasks: Vec<_> = profiles
            .iter()
            .cloned()
            .map(|profile| {
                tokio::task::spawn_blocking(move || {
                    terminal::inspect_command(&profile.terminal, profile.explicit_override)
                })
            })
            .collect();

        let mut agents = Vec::with_capacity(profiles.len());
        for (profile, result) in profiles.into_iter().zip(join_all(tasks).await) {
            let (state, version) = match result {
                Ok(inspection) => {
                    let state = match inspection.state {
                        CommandInspectionState::Ready => AgentCatalogState::Ready,
                        CommandInspectionState::Missing => AgentCatalogState::Missing,
                        CommandInspectionState::Misconfigured => AgentCatalogState::Misconfigured,
                    };
                    (state, inspection.version)
                }
                Err(_) => (AgentCatalogState::Misconfigured, None),
            };
            agents.push(AgentCatalogEntry {
                kind: profile.terminal.agent,
                state,
                configuration: if profile.explicit_override {
                    AgentConfiguration::Override
                } else {
                    AgentConfiguration::Auto
                },
                version,
                dangerously_skip_permissions: profile.dangerously_skip_permissions,
                install: install_instructions(profile.terminal.agent),
            });
        }
        agents.sort_by_key(|entry| agent_order(entry.kind));

        AgentCatalogResponse {
            schema_version: CATALOG_SCHEMA_VERSION,
            server: AgentCatalogServer {
                os: std::env::consts::OS,
                arch: std::env::consts::ARCH,
                shell: self.server_shell,
            },
            agents,
        }
    }

    pub async fn ready_agents(&self) -> Vec<AgentKind> {
        self.snapshot()
            .await
            .agents
            .into_iter()
            .filter_map(|entry| (entry.state == AgentCatalogState::Ready).then_some(entry.kind))
            .collect()
    }
}

pub fn build_agent_profiles(config: &Config) -> AgentProfiles {
    let primary_agent = config.primary_agent;
    let primary_specific_override = agent_command_override(config, primary_agent);
    let primary_command = config
        .command
        .clone()
        .or_else(|| primary_specific_override.clone())
        .unwrap_or_else(|| default_command(primary_agent).to_owned());
    let primary_explicit = config.command.is_some() || primary_specific_override.as_ref().is_some();
    let primary = terminal_config(config, primary_agent, primary_command.clone());

    let new_session_explicit = config.new_session_command.is_some() || primary_explicit;
    let new_session = terminal_config(
        config,
        primary_agent,
        config
            .new_session_command
            .clone()
            .unwrap_or(primary_command),
    );

    let mut additional = Vec::new();
    let mut catalog_profiles = vec![CatalogProfile {
        terminal: new_session.clone(),
        explicit_override: new_session_explicit,
        dangerously_skip_permissions: dangerously_skip_permissions(config, primary_agent),
    }];

    for agent in AgentKind::ALL {
        if agent == primary_agent {
            continue;
        }
        let command_override = agent_command_override(config, agent);
        if config.no_agent_auto_detect && command_override.is_none() {
            continue;
        }
        let explicit_override = command_override.is_some();
        let terminal = terminal_config(
            config,
            agent,
            command_override.unwrap_or_else(|| default_command(agent).to_owned()),
        );
        additional.push(terminal.clone());
        catalog_profiles.push(CatalogProfile {
            terminal,
            explicit_override,
            dangerously_skip_permissions: dangerously_skip_permissions(config, agent),
        });
    }
    catalog_profiles.sort_by_key(|profile| agent_order(profile.terminal.agent));

    AgentProfiles {
        primary,
        new_session,
        additional,
        catalog: AgentCatalog {
            profiles: Arc::new(catalog_profiles),
            server_shell: server_shell(config.shell),
        },
    }
}

fn terminal_config(config: &Config, agent: AgentKind, command: String) -> TerminalConfig {
    TerminalConfig {
        project_dir: config.project_dir.clone(),
        command,
        arguments: agent_arguments(config, agent),
        agent,
        shell: config.shell,
    }
}

fn agent_command_override(config: &Config, agent: AgentKind) -> Option<String> {
    match agent {
        AgentKind::Codex => config.codex_command.clone(),
        AgentKind::Claude => config.claude_command.clone(),
        AgentKind::Agy => config.agy_command.clone(),
    }
}

fn dangerously_skip_permissions(config: &Config, agent: AgentKind) -> bool {
    match agent {
        AgentKind::Codex => false,
        AgentKind::Claude => config.claude_dangerously_skip_permissions,
        AgentKind::Agy => config.agy_dangerously_skip_permissions,
    }
}

fn agent_arguments(config: &Config, agent: AgentKind) -> Vec<String> {
    dangerously_skip_permissions(config, agent)
        .then(|| "--dangerously-skip-permissions".to_owned())
        .into_iter()
        .collect()
}

const fn default_command(agent: AgentKind) -> &'static str {
    match agent {
        AgentKind::Codex => "codex",
        AgentKind::Claude => "claude",
        AgentKind::Agy => "agy",
    }
}

const fn agent_order(agent: AgentKind) -> u8 {
    match agent {
        AgentKind::Codex => 0,
        AgentKind::Claude => 1,
        AgentKind::Agy => 2,
    }
}

const fn server_shell(shell: ShellKind) -> &'static str {
    #[cfg(windows)]
    {
        match shell {
            ShellKind::Powershell => "powershell",
            ShellKind::Cmd => "cmd",
        }
    }

    #[cfg(not(windows))]
    {
        let _ = shell;
        "sh"
    }
}

#[cfg(windows)]
const fn install_instructions(agent: AgentKind) -> AgentInstallInstructions {
    match agent {
        AgentKind::Codex => AgentInstallInstructions {
            command: "powershell -ExecutionPolicy ByPass -c \"irm https://chatgpt.com/codex/install.ps1 | iex\"",
            shell: "powershell",
            verify_command: "codex --version",
            update_command: "codex update",
            docs_url: "https://learn.chatgpt.com/docs/codex/cli",
            requires_server_access: true,
        },
        AgentKind::Claude => AgentInstallInstructions {
            command: "irm https://claude.ai/install.ps1 | iex",
            shell: "powershell",
            verify_command: "claude --version",
            update_command: "claude update",
            docs_url: "https://code.claude.com/docs/en/setup",
            requires_server_access: true,
        },
        AgentKind::Agy => AgentInstallInstructions {
            command: "irm https://antigravity.google/cli/install.ps1 | iex",
            shell: "powershell",
            verify_command: "agy --version",
            update_command: "irm https://antigravity.google/cli/install.ps1 | iex",
            docs_url: "https://antigravity.google/docs/cli/install",
            requires_server_access: true,
        },
    }
}

#[cfg(not(windows))]
const fn install_instructions(agent: AgentKind) -> AgentInstallInstructions {
    match agent {
        AgentKind::Codex => AgentInstallInstructions {
            command: "curl -fsSL https://chatgpt.com/codex/install.sh | sh",
            shell: "sh",
            verify_command: "codex --version",
            update_command: "codex update",
            docs_url: "https://learn.chatgpt.com/docs/codex/cli",
            requires_server_access: true,
        },
        AgentKind::Claude => AgentInstallInstructions {
            command: "curl -fsSL https://claude.ai/install.sh | bash",
            shell: "bash",
            verify_command: "claude --version",
            update_command: "claude update",
            docs_url: "https://code.claude.com/docs/en/setup",
            requires_server_access: true,
        },
        AgentKind::Agy => AgentInstallInstructions {
            command: "curl -fsSL https://antigravity.google/cli/install.sh | bash",
            shell: "bash",
            verify_command: "agy --version",
            update_command: "curl -fsSL https://antigravity.google/cli/install.sh | bash",
            docs_url: "https://antigravity.google/docs/cli/install",
            requires_server_access: true,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::{
        net::{IpAddr, Ipv4Addr},
        path::PathBuf,
    };

    use super::*;

    fn config(primary_agent: AgentKind) -> Config {
        Config {
            host: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 8787,
            project_dir: PathBuf::from("."),
            state_dir: PathBuf::from("."),
            shell: ShellKind::Powershell,
            command: None,
            primary_agent,
            new_session_command: None,
            codex_command: None,
            claude_command: None,
            claude_dangerously_skip_permissions: false,
            agy_command: None,
            no_agent_auto_detect: false,
            agy_dangerously_skip_permissions: false,
            token: None,
            no_open_browser: true,
            log_level: "info".to_owned(),
        }
    }

    #[test]
    fn primary_agent_selects_its_own_default_command() {
        let profiles = build_agent_profiles(&config(AgentKind::Claude));

        assert_eq!(profiles.primary.command, "claude");
        assert_eq!(profiles.new_session.command, "claude");
        assert_eq!(profiles.additional.len(), 2);
    }

    #[test]
    fn explicit_primary_command_has_priority() {
        let mut config = config(AgentKind::Agy);
        config.command = Some("trusted-agy-wrapper".to_owned());
        config.agy_command = Some("agy-other-sessions".to_owned());

        let profiles = build_agent_profiles(&config);

        assert_eq!(profiles.primary.command, "trusted-agy-wrapper");
        assert_eq!(profiles.new_session.command, "trusted-agy-wrapper");
    }

    #[test]
    fn disabling_auto_detect_keeps_primary_and_explicit_profiles_only() {
        let mut config = config(AgentKind::Codex);
        config.no_agent_auto_detect = true;
        config.claude_command = Some("claude-custom".to_owned());

        let profiles = build_agent_profiles(&config);

        assert_eq!(profiles.additional.len(), 1);
        assert_eq!(profiles.additional[0].agent, AgentKind::Claude);
        assert_eq!(profiles.additional[0].command, "claude-custom");
    }

    #[test]
    fn non_codex_primary_can_use_an_explicit_codex_profile() {
        let mut config = config(AgentKind::Claude);
        config.codex_command = Some("trusted-codex-wrapper".to_owned());

        let profiles = build_agent_profiles(&config);
        let codex = profiles
            .additional
            .iter()
            .find(|profile| profile.agent == AgentKind::Codex)
            .expect("Codex profile");

        assert_eq!(codex.command, "trusted-codex-wrapper");
    }

    #[test]
    fn dangerous_arguments_remain_agent_specific() {
        let mut config = config(AgentKind::Codex);
        config.claude_dangerously_skip_permissions = true;
        config.agy_dangerously_skip_permissions = true;

        let profiles = build_agent_profiles(&config);

        assert!(profiles.primary.arguments.is_empty());
        assert_eq!(
            profiles
                .additional
                .iter()
                .find(|profile| profile.agent == AgentKind::Claude)
                .expect("Claude profile")
                .arguments,
            ["--dangerously-skip-permissions"]
        );
    }

    #[test]
    fn catalog_contract_uses_fixed_install_guidance() {
        let instructions = install_instructions(AgentKind::Agy);

        assert_eq!(instructions.verify_command, "agy --version");
        assert_eq!(instructions.update_command, instructions.command);
        assert!(instructions.docs_url.starts_with("https://"));
        assert!(instructions.requires_server_access);
    }

    #[tokio::test]
    async fn catalog_response_matches_the_versioned_api_schema() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let command = write_version_fixture(directory.path());
        let profile = CatalogProfile {
            terminal: TerminalConfig {
                project_dir: directory.path().to_path_buf(),
                command: command.to_string_lossy().into_owned(),
                arguments: Vec::new(),
                agent: AgentKind::Codex,
                shell: ShellKind::Powershell,
            },
            explicit_override: true,
            dangerously_skip_permissions: false,
        };
        let catalog = AgentCatalog {
            profiles: Arc::new(vec![profile]),
            server_shell: "test",
        };

        let value = serde_json::to_value(catalog.snapshot().await).expect("serialize catalog");
        let agent = &value["agents"][0];

        assert_eq!(value["schemaVersion"], 1);
        assert!(value["server"]["os"].is_string());
        assert!(value["server"]["arch"].is_string());
        assert_eq!(value["server"]["shell"], "test");
        assert_eq!(agent["kind"], "codex");
        assert_eq!(agent["state"], "ready");
        assert_eq!(agent["configuration"], "override");
        assert_eq!(agent["version"], "9.8.7");
        assert_eq!(agent["dangerouslySkipPermissions"], false);
        assert_eq!(agent["install"]["verifyCommand"], "codex --version");
        assert_eq!(agent["install"]["requiresServerAccess"], true);
        assert!(agent.get("path").is_none());
        assert!(agent.get("error").is_none());
    }

    #[cfg(windows)]
    fn write_version_fixture(directory: &std::path::Path) -> PathBuf {
        let path = directory.join("codex.cmd");
        std::fs::write(&path, "@echo off\r\necho codex-cli 9.8.7\r\nexit /b 0\r\n")
            .expect("write version fixture");
        path
    }

    #[cfg(unix)]
    fn write_version_fixture(directory: &std::path::Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;

        let path = directory.join("codex");
        std::fs::write(&path, "#!/bin/sh\necho 'codex-cli 9.8.7'\n")
            .expect("write version fixture");
        let mut permissions = std::fs::metadata(&path)
            .expect("fixture metadata")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&path, permissions).expect("make fixture executable");
        path
    }
}
